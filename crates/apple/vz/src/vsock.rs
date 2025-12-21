//! Vsock port-to-socket bridging for the native Virtualization.framework backend.
//!
//! This module bridges vsock ports to Unix domain sockets, allowing host applications
//! to communicate with guest applications via standard Unix socket APIs.
//!
//! ## Limitations
//!
//! Currently, each vsock port only supports **one connection**. After the first connection
//! closes, the port becomes unavailable. This limitation exists because the synchronous
//! channel used to pass file descriptors from the Objective-C delegate is consumed after
//! the first connection.

use capsa_core::VsockPortConfig;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_virtualization::{
    VZVirtioSocketConnection, VZVirtioSocketDevice, VZVirtioSocketListener,
    VZVirtioSocketListenerDelegate,
};
use std::cell::Cell;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::mpsc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Buffer size for vsock bridging. 4KB matches typical Unix socket buffer sizes
/// and provides a good balance between memory usage and throughput.
const BRIDGE_BUFFER_SIZE: usize = 4096;

/// Configuration for a vsock bridge task (internal use only).
pub(crate) struct VsockBridgeTask {
    socket_path: PathBuf,
    conn_rx: mpsc::Receiver<RawFd>,
    port: u32,
}

/// Manages vsock port-to-socket bridging for a VM.
///
/// Keeps the Objective-C listener objects alive for the lifetime of the VM.
/// Socket cleanup is handled by the VmHandle through temp_files.
///
/// ## Limitations
///
/// Currently only supports **one connection per port**. After the first connection
/// closes, the port becomes unavailable.
pub struct VsockBridge {
    /// Background task handles
    _tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Objective-C listener objects that must stay alive for the VM's lifetime.
    /// They are properly released in Drop.
    _objc_listeners: Vec<SendableObjcPtr>,
    _objc_delegates: Vec<SendableObjcPtr>,
}

/// Channel sender for vsock connection file descriptors.
type ConnectionSender = mpsc::SyncSender<RawFd>;

/// Ivars for the vsock listener delegate.
pub struct VsockListenerDelegateIvars {
    connection_sender: Cell<Option<ConnectionSender>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = VsockListenerDelegateIvars]
    pub struct VsockListenerDelegate;

    unsafe impl NSObjectProtocol for VsockListenerDelegate {}

    unsafe impl VZVirtioSocketListenerDelegate for VsockListenerDelegate {
        #[unsafe(method(listener:shouldAcceptNewConnection:fromSocketDevice:))]
        fn listener_should_accept_new_connection(
            &self,
            _listener: &VZVirtioSocketListener,
            connection: &VZVirtioSocketConnection,
            _socket_device: &VZVirtioSocketDevice,
        ) -> objc2::runtime::Bool {
            let fd = unsafe { connection.fileDescriptor() };

            if fd < 0 {
                return false.into();
            }

            // Duplicate the fd so we own a copy (connection owns the original)
            let dup_fd = unsafe { libc::dup(fd) };
            if dup_fd < 0 {
                return false.into();
            }

            // Send the fd to the bridging task
            if let Some(sender) = self.ivars().connection_sender.take() {
                let _ = sender.try_send(dup_fd);
                // Put it back for potential future connections
                self.ivars().connection_sender.set(Some(sender));
            }

            true.into()
        }
    }
);

impl VsockListenerDelegate {
    fn new(mtm: MainThreadMarker, connection_sender: ConnectionSender) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(VsockListenerDelegateIvars {
            connection_sender: Cell::new(Some(connection_sender)),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// Wrapper for raw pointers that are safe to send across threads.
///
/// SAFETY: The wrapped pointer must point to an Objective-C object that is
/// ref-counted and will remain valid as long as this wrapper exists.
#[derive(Debug)]
pub struct SendableObjcPtr(*const std::ffi::c_void);

// SAFETY: The Objective-C objects (VZVirtioSocketListener and VsockListenerDelegate)
// are ref-counted by the Virtualization framework. After setup, we only hold these
// pointers to prevent deallocation - we don't access the objects through them.
// The actual object access happens through the framework's internal mechanisms.
unsafe impl Send for SendableObjcPtr {}
unsafe impl Sync for SendableObjcPtr {}

impl SendableObjcPtr {
    fn new(ptr: *const std::ffi::c_void) -> Self {
        Self(ptr)
    }

    fn as_ptr(&self) -> *const std::ffi::c_void {
        self.0
    }
}

/// Result of setting up vsock listeners (internal use only).
pub(crate) struct VsockSetupResult {
    /// Tasks to be spawned for bridging
    pub(crate) tasks: Vec<VsockBridgeTask>,
    /// Raw pointers to Objective-C listener objects (for Drop)
    pub(crate) objc_listeners: Vec<SendableObjcPtr>,
    /// Raw pointers to Objective-C delegate objects (for Drop)
    pub(crate) objc_delegates: Vec<SendableObjcPtr>,
}

impl VsockBridge {
    /// Sets up vsock listeners and returns tasks to be spawned.
    ///
    /// # Safety
    /// `socket_device_addr` must be a valid pointer to a VZVirtioSocketDevice.
    /// Must be called on the main thread.
    #[allow(unsafe_op_in_unsafe_fn)]
    pub unsafe fn setup_listeners(
        socket_device_addr: usize,
        ports: Vec<VsockPortConfig>,
        mtm: MainThreadMarker,
    ) -> VsockSetupResult {
        let mut tasks = Vec::new();
        let mut objc_listeners = Vec::new();
        let mut objc_delegates = Vec::new();

        if socket_device_addr == 0 || ports.is_empty() {
            return VsockSetupResult {
                tasks,
                objc_listeners,
                objc_delegates,
            };
        }

        let socket_device = unsafe {
            let ptr = socket_device_addr as *const VZVirtioSocketDevice;
            &*ptr
        };

        for port_config in ports {
            if port_config.is_connect() {
                // Connect mode not yet implemented
                continue;
            }

            let socket_path = port_config.socket_path().to_path_buf();

            // Create a channel for passing the vsock connection fd
            let (conn_tx, conn_rx) = mpsc::sync_channel::<RawFd>(1);

            // Create the listener delegate
            let delegate = VsockListenerDelegate::new(mtm, conn_tx);
            let delegate_obj = ProtocolObject::from_ref(&*delegate);

            // Create and configure the listener
            let listener = unsafe { VZVirtioSocketListener::new() };
            unsafe { listener.setDelegate(Some(delegate_obj)) };

            // Register the listener for this port
            unsafe { socket_device.setSocketListener_forPort(&listener, port_config.port()) };

            // Store raw pointers so they stay alive (released in Drop)
            objc_listeners.push(SendableObjcPtr::new(
                Retained::into_raw(listener) as *const std::ffi::c_void
            ));
            objc_delegates.push(SendableObjcPtr::new(
                Retained::into_raw(delegate) as *const std::ffi::c_void
            ));

            // Add task configuration to be spawned later
            tasks.push(VsockBridgeTask {
                socket_path,
                conn_rx,
                port: port_config.port(),
            });
        }

        VsockSetupResult {
            tasks,
            objc_listeners,
            objc_delegates,
        }
    }

    /// Creates a VsockBridge from setup results and spawns bridging tasks.
    pub fn from_setup_result(result: VsockSetupResult) -> Self {
        let tasks = result
            .tasks
            .into_iter()
            .map(|task| {
                tokio::spawn(unix_socket_bridge_task(
                    task.socket_path,
                    task.conn_rx,
                    task.port,
                ))
            })
            .collect();

        Self {
            _tasks: tasks,
            _objc_listeners: result.objc_listeners,
            _objc_delegates: result.objc_delegates,
        }
    }
}

impl Drop for VsockBridge {
    fn drop(&mut self) {
        // Socket file cleanup is handled by VmHandle through temp_files.
        // Release the Objective-C objects we kept alive.
        // SAFETY: These pointers were created by Retained::into_raw() in setup_listeners,
        // so they are valid Retained pointers. We're converting them back to Retained
        // so they can be properly released when dropped.
        for ptr in &self._objc_listeners {
            let raw = ptr.as_ptr();
            if !raw.is_null() {
                unsafe {
                    let _ = Retained::from_raw(raw as *mut VZVirtioSocketListener);
                }
            }
        }
        for ptr in &self._objc_delegates {
            let raw = ptr.as_ptr();
            if !raw.is_null() {
                unsafe {
                    let _ = Retained::from_raw(raw as *mut VsockListenerDelegate);
                }
            }
        }
    }
}

/// Background task that manages the Unix socket for a vsock port.
///
/// This task:
/// 1. Creates a Unix domain socket at `socket_path`
/// 2. Waits for a host application to connect
/// 3. Waits for the guest to connect via vsock
/// 4. Bridges data between them until either side disconnects
///
/// Note: Currently only supports one connection per port.
async fn unix_socket_bridge_task(socket_path: PathBuf, conn_rx: mpsc::Receiver<RawFd>, port: u32) {
    // Remove existing socket file if present
    let _ = std::fs::remove_file(&socket_path);

    // Create the Unix socket listener
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[vsock] port {}: failed to create Unix socket at {:?}: {}",
                port, socket_path, e
            );
            return;
        }
    };

    loop {
        // Accept a connection from the host
        let (host_stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("[vsock] port {}: accept failed: {}, retrying", port, e);
                continue;
            }
        };

        // Wait for a guest connection (blocks until guest connects to this port)
        let guest_fd = match tokio::task::spawn_blocking(move || conn_rx.recv()).await {
            Ok(Ok(fd)) => fd,
            Ok(Err(_)) => {
                // Channel closed, VM is shutting down
                break;
            }
            Err(_) => {
                // Task panicked
                eprintln!("[vsock] port {}: recv task panicked", port);
                break;
            }
        };

        // Spawn a task to bridge this connection pair
        tokio::spawn(bridge_connection(host_stream, guest_fd, port));

        // Currently only support one connection per port (channel is consumed).
        // See module-level documentation for details.
        break;
    }

    // Clean up socket file
    let _ = std::fs::remove_file(&socket_path);
}

/// Bridges data between a Unix socket stream and a vsock file descriptor.
///
/// Runs two concurrent copy loops (host→guest and guest→host) until either
/// side closes or an error occurs.
async fn bridge_connection(host_stream: UnixStream, guest_fd: RawFd, port: u32) {
    // SAFETY: We own this fd - it was dup'd in the delegate callback specifically
    // for us to take ownership. The original fd is owned by VZVirtioSocketConnection.
    let guest_fd_owned = unsafe { OwnedFd::from_raw_fd(guest_fd) };

    // SAFETY: We own guest_fd and are setting it to non-blocking mode before
    // wrapping in AsyncFd. The fd is valid for the duration of this function.
    // F_GETFL and F_SETFL are safe operations on a valid fd.
    unsafe {
        let flags = libc::fcntl(guest_fd, libc::F_GETFL);
        libc::fcntl(guest_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    let guest_async = match tokio::io::unix::AsyncFd::new(guest_fd_owned) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!(
                "[vsock] port {}: failed to create async fd for guest connection: {}",
                port, e
            );
            return;
        }
    };

    let (mut host_read, mut host_write) = tokio::io::split(host_stream);

    // Copy data from host to guest
    let host_to_guest = async {
        let mut buf = [0u8; BRIDGE_BUFFER_SIZE];
        loop {
            let n = match host_read.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => n,
                Err(_) => break,
            };

            // Write to guest fd using libc (AsyncFd doesn't impl AsyncWrite)
            let mut written = 0;
            while written < n {
                let mut ready = match guest_async.writable().await {
                    Ok(r) => r,
                    Err(_) => break,
                };
                match ready.try_io(|fd| {
                    // SAFETY: buf[written..n] is valid, fd is valid and writable.
                    // libc::write returns bytes written or -1 on error.
                    let ret = unsafe {
                        libc::write(
                            fd.as_raw_fd(),
                            buf[written..n].as_ptr() as *const libc::c_void,
                            n - written,
                        )
                    };
                    if ret < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(ret as usize)
                    }
                }) {
                    Ok(Ok(w)) => written += w,
                    Ok(Err(_)) => break,
                    Err(_would_block) => continue,
                }
            }
        }
    };

    // Copy data from guest to host
    let guest_to_host = async {
        let mut buf = [0u8; BRIDGE_BUFFER_SIZE];
        loop {
            let mut ready = match guest_async.readable().await {
                Ok(r) => r,
                Err(_) => break,
            };
            let n = match ready.try_io(|fd| {
                // SAFETY: buf is valid, fd is valid and readable.
                // libc::read returns bytes read or -1 on error.
                let ret = unsafe {
                    libc::read(
                        fd.as_raw_fd(),
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                    )
                };
                if ret < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(ret as usize)
                }
            }) {
                Ok(Ok(0)) => break, // EOF
                Ok(Ok(n)) => n,
                Ok(Err(_)) => break,
                Err(_would_block) => continue,
            };

            if host_write.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };

    tokio::select! {
        _ = host_to_guest => {}
        _ = guest_to_host => {}
    }
}
