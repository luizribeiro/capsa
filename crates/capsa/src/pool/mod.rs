//! VM Pool management.
//!
//! A [`VmPool`] pre-creates identical VMs that can be reserved for temporary use.
//! When a [`PooledVm`] is dropped, it is killed and replaced with a fresh VM.
//!
//! See the [VM Pools guide](crate::guides::vm_pools) for patterns and best practices.

// TODO: even though VM pools don't allow for their VMs to have disks,
// in practice, they might still allowed to have root disks (if the root
// disk is set on LinuxDirectBootConfig, for example). however, we may want
// to allow those with read-only access to the disk. it is also unclear
// what happens when LinuxDirectBootConfig is setup with a root disk right now
// and VMs are pooled using that config

mod poolable;

pub(crate) use poolable::{No, Poolability, Yes};

use crate::backend::select_backend;
use crate::handle::VmHandle;
use capsa_core::{Error, GuestOs, HypervisorBackend, Result, VmConfig};
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

struct VmPoolInner {
    config: VmConfig,
    backend: Box<dyn HypervisorBackend>,
    available: Mutex<Vec<VmHandle>>,
    notify: Notify,
    shutting_down: std::sync::atomic::AtomicBool,
}

impl VmPoolInner {
    fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A pool of pre-created VMs that can be reserved for temporary use.
///
/// Create via [`Capsa::pool`](crate::Capsa::pool). VMs are killed and replaced
/// when the [`PooledVm`] is dropped, ensuring fresh state for each reservation.
///
/// Thread-safe: can be shared across tasks using `Arc`.
///
/// See the [VM Pools guide](crate::guides::vm_pools) for usage patterns.
pub struct VmPool {
    inner: Arc<VmPoolInner>,
}

impl VmPool {
    pub(crate) async fn new(config: VmConfig, size: usize) -> Result<Self> {
        if size == 0 {
            return Err(Error::InvalidConfig("pool size must be at least 1".into()));
        }

        let backend = select_backend()?;

        let mut vms = Vec::with_capacity(size);
        for _ in 0..size {
            let vm = Self::spawn_vm(&config, backend.as_ref()).await?;
            vms.push(vm);
        }

        let inner = Arc::new(VmPoolInner {
            config,
            backend,
            available: Mutex::new(vms),
            notify: Notify::new(),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        });

        Ok(Self { inner })
    }

    async fn spawn_vm(config: &VmConfig, backend: &dyn HypervisorBackend) -> Result<VmHandle> {
        let backend_handle = backend.start(config).await?;
        Ok(VmHandle::new(
            backend_handle,
            GuestOs::Linux,
            config.resources.clone(),
        ))
    }

    /// Reserves a VM from the pool, waiting if none are available.
    ///
    /// This method will block asynchronously until a VM becomes available.
    /// If you need non-blocking behavior, use [`try_reserve`](Self::try_reserve).
    ///
    /// # Errors
    ///
    /// Returns [`Error::PoolShutdown`] if the pool is being shut down.
    pub async fn reserve(&self) -> Result<PooledVm> {
        loop {
            // Register for notification BEFORE checking state to avoid race
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);

            // Enable the notification to ensure we don't miss it
            notified.as_mut().enable();

            if self.inner.is_shutting_down() {
                return Err(Error::PoolShutdown);
            }

            {
                let mut available = self.inner.available.lock().await;
                if let Some(vm) = available.pop() {
                    return Ok(PooledVm {
                        handle: Some(vm),
                        pool: Arc::clone(&self.inner),
                    });
                }
            }

            notified.await;
        }
    }

    /// Attempts to reserve a VM without waiting.
    ///
    /// Returns immediately with [`Error::PoolEmpty`] if no VMs are available.
    /// This is useful in hot paths where blocking is not acceptable.
    ///
    /// # Errors
    ///
    /// - [`Error::PoolEmpty`] - No VMs available or lock is contended
    /// - [`Error::PoolShutdown`] - Pool is being shut down
    pub fn try_reserve(&self) -> Result<PooledVm> {
        if self.inner.is_shutting_down() {
            return Err(Error::PoolShutdown);
        }

        let mut available = match self.inner.available.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Err(Error::PoolEmpty),
        };

        match available.pop() {
            Some(vm) => Ok(PooledVm {
                handle: Some(vm),
                pool: Arc::clone(&self.inner),
            }),
            None => Err(Error::PoolEmpty),
        }
    }

    /// Returns the current number of available VMs in the pool.
    ///
    /// Note: This is a snapshot and may be stale by the time you read it
    /// due to concurrent reservations.
    pub async fn available_count(&self) -> usize {
        self.inner.available.lock().await.len()
    }
}

impl Drop for VmPool {
    fn drop(&mut self) {
        // Always set shutdown flag and notify waiters to unblock any waiting reserve() calls
        self.inner
            .shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.notify.notify_waiters();

        // Only cleanup VMs if we're the last reference
        if Arc::strong_count(&self.inner) == 1 {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let mut available = inner.available.lock().await;
                for vm in available.drain(..) {
                    let _ = vm.kill().await;
                }
            });
        }
    }
}

/// A VM reserved from a pool.
///
/// Implements [`Deref<Target=VmHandle>`](std::ops::Deref), so all [`VmHandle`]
/// methods work directly. When dropped, the VM is killed and replaced.
pub struct PooledVm {
    handle: Option<VmHandle>,
    pool: Arc<VmPoolInner>,
}

impl Deref for PooledVm {
    type Target = VmHandle;

    fn deref(&self) -> &Self::Target {
        self.handle.as_ref().expect("PooledVm handle taken")
    }
}

impl Drop for PooledVm {
    fn drop(&mut self) {
        if let Some(vm) = self.handle.take() {
            let pool = Arc::clone(&self.pool);
            tokio::spawn(async move {
                let _ = vm.kill().await;

                if pool.is_shutting_down() {
                    return;
                }

                match VmPool::spawn_vm(&pool.config, pool.backend.as_ref()).await {
                    Ok(new_vm) => {
                        pool.available.lock().await.push(new_vm);
                        pool.notify.notify_one();
                    }
                    Err(e) => {
                        tracing::error!("Failed to spawn replacement VM: {}", e);
                    }
                }
            });
        }
    }
}
