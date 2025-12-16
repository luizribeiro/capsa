use super::{BackendVmHandle, ConsoleStream, HypervisorBackend, InternalVmConfig};
use crate::boot::KernelCmdline;
use crate::capabilities::{
    BackendCapabilities, BootMethodSupport, GuestOsSupport, ImageFormatSupport, NetworkModeSupport,
};
use crate::error::{Error, Result};
use crate::types::{ConsoleMode, MountMode, NetworkMode, ShareMechanism};
use async_trait::async_trait;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::pty::{openpty, OpenptyResult};
use nix::sys::termios::{self, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::io::FromRawFd;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

#[allow(dead_code)]
pub struct VfkitBackend {
    vfkit_path: Option<PathBuf>,
    capabilities: BackendCapabilities,
}

impl VfkitBackend {
    pub fn new() -> Self {
        let vfkit_path = which::which("vfkit").ok();

        let capabilities = BackendCapabilities {
            guest_os: GuestOsSupport { linux: true },
            boot_methods: BootMethodSupport { linux_direct: true },
            image_formats: ImageFormatSupport {
                raw: true,
                qcow2: false,
            },
            network_modes: NetworkModeSupport {
                none: true,
                nat: true,
            },
            virtio_fs: true,
            virtio_9p: false,
            vsock: true,
            max_cpus: None,
            max_memory_mb: None,
        };

        Self {
            vfkit_path,
            capabilities,
        }
    }

    fn build_args(&self, config: &InternalVmConfig) -> Vec<String> {
        let mut args = vec![
            "--cpus".to_string(),
            config.resources.cpus.to_string(),
            "--memory".to_string(),
            config.resources.memory_mb.to_string(),
            "--kernel".to_string(),
            config.kernel.display().to_string(),
            "--initrd".to_string(),
            config.initrd.display().to_string(),
            "--kernel-cmdline".to_string(),
            config.cmdline.clone(),
        ];

        if let Some(disk) = &config.disk {
            args.push("--device".to_string());
            args.push(format!("virtio-blk,path={}", disk.path.display()));
        }

        match &config.network {
            NetworkMode::None => {}
            NetworkMode::Nat => {
                args.push("--device".to_string());
                args.push("virtio-net,nat".to_string());
            }
        }

        for share in &config.shares {
            let mode = match share.mode {
                MountMode::ReadOnly => "ro",
                MountMode::ReadWrite => "rw",
            };

            let tag = match &share.mechanism {
                ShareMechanism::Auto => {
                    share.guest_path.replace('/', "_").trim_matches('_').to_string()
                }
                ShareMechanism::VirtioFs(cfg) => cfg.tag.clone().unwrap_or_else(|| {
                    share.guest_path.replace('/', "_").trim_matches('_').to_string()
                }),
                ShareMechanism::Virtio9p(cfg) => cfg.tag.clone().unwrap_or_else(|| {
                    share.guest_path.replace('/', "_").trim_matches('_').to_string()
                }),
            };

            args.push("--device".to_string());
            args.push(format!(
                "virtio-fs,sharedDir={},mountTag={},{}",
                share.host_path.display(),
                tag,
                mode
            ));
        }

        // Console is always added as stdio - we use PTY to handle it
        match &config.console {
            ConsoleMode::Disabled => {}
            ConsoleMode::Enabled | ConsoleMode::Stdio => {
                args.push("--device".to_string());
                args.push("virtio-serial,stdio".to_string());
            }
        }

        args
    }
}

impl Default for VfkitBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HypervisorBackend for VfkitBackend {
    fn name(&self) -> &'static str {
        "vfkit"
    }

    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn is_available(&self) -> bool {
        self.vfkit_path.is_some()
    }

    async fn start(&self, config: &InternalVmConfig) -> Result<Box<dyn BackendVmHandle>> {
        let vfkit_path = self
            .vfkit_path
            .as_ref()
            .ok_or_else(|| Error::BackendUnavailable {
                name: "vfkit".to_string(),
                reason: "vfkit binary not found in PATH".to_string(),
            })?;

        let args = self.build_args(config);

        tracing::debug!("Starting vfkit with args: {:?}", args);

        // Create PTY for console if enabled
        let pty = if config.console != ConsoleMode::Disabled {
            Some(Pty::new().map_err(|e| Error::StartFailed(format!("Failed to create PTY: {}", e)))?)
        } else {
            None
        };

        let mut cmd = Command::new(vfkit_path);
        cmd.args(&args);

        if let Some(ref pty) = pty {
            // Connect vfkit's stdio to the PTY slave
            let (stdin, stdout, stderr) = pty.slave_stdio()?;
            cmd.stdin(stdin);
            cmd.stdout(stdout);
            cmd.stderr(stderr);
        } else {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }

        let child = cmd
            .spawn()
            .map_err(|e| Error::StartFailed(format!("Failed to spawn vfkit: {}", e)))?;

        Ok(Box::new(VfkitVmHandle::new(child, pty)))
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        let mut cmdline = KernelCmdline::new();
        cmdline.console("hvc0");
        cmdline.arg("reboot=t");
        cmdline.arg("panic=-1");
        cmdline
    }

    fn default_root_device(&self) -> &str {
        "/dev/vda"
    }
}

/// PTY pair for console I/O
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

impl Pty {
    fn new() -> std::io::Result<Self> {
        let OpenptyResult { master, slave } =
            openpty(None, None).map_err(std::io::Error::other)?;

        // Configure PTY for raw pass-through of control characters while keeping
        // output processing for proper line endings
        use std::os::fd::BorrowedFd;
        let slave_fd = unsafe { BorrowedFd::borrow_raw(slave.as_raw_fd()) };
        if let Ok(mut termios) = termios::tcgetattr(slave_fd) {
            // Disable input processing that would intercept control characters
            termios.input_flags.remove(InputFlags::IGNBRK);
            termios.input_flags.remove(InputFlags::BRKINT);
            termios.input_flags.remove(InputFlags::PARMRK);
            termios.input_flags.remove(InputFlags::ISTRIP);
            termios.input_flags.remove(InputFlags::INLCR);
            termios.input_flags.remove(InputFlags::IGNCR);
            termios.input_flags.remove(InputFlags::ICRNL);
            termios.input_flags.remove(InputFlags::IXON);

            // Keep output processing for proper line endings (\n -> \r\n)
            termios.output_flags.insert(OutputFlags::OPOST);
            termios.output_flags.insert(OutputFlags::ONLCR);

            // Disable local flags that would intercept signals or do line editing
            termios.local_flags.remove(LocalFlags::ECHO);
            termios.local_flags.remove(LocalFlags::ECHONL);
            termios.local_flags.remove(LocalFlags::ICANON);
            termios.local_flags.remove(LocalFlags::ISIG);
            termios.local_flags.remove(LocalFlags::IEXTEN);

            // 8-bit clean
            termios.control_flags.remove(ControlFlags::CSIZE);
            termios.control_flags.remove(ControlFlags::PARENB);
            termios.control_flags.insert(ControlFlags::CS8);

            let _ = termios::tcsetattr(slave_fd, SetArg::TCSANOW, &termios);
        }

        Ok(Self { master, slave })
    }

    fn slave_stdio(&self) -> Result<(Stdio, Stdio, Stdio)> {
        let dup_fd = |fd: &OwnedFd| -> Result<Stdio> {
            let new_fd = nix::unistd::dup(fd.as_raw_fd())
                .map_err(|e| Error::StartFailed(format!("Failed to dup fd: {}", e)))?;
            Ok(unsafe { Stdio::from_raw_fd(new_fd) })
        };

        Ok((dup_fd(&self.slave)?, dup_fd(&self.slave)?, dup_fd(&self.slave)?))
    }

    fn into_async_master(self) -> std::io::Result<AsyncPtyMaster> {
        // Set non-blocking mode on master
        let flags = fcntl(self.master.as_raw_fd(), FcntlArg::F_GETFL)
            .map_err(std::io::Error::other)?;
        let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(self.master.as_raw_fd(), FcntlArg::F_SETFL(flags))
            .map_err(std::io::Error::other)?;

        let async_fd = AsyncFd::new(self.master)?;
        Ok(AsyncPtyMaster { inner: async_fd })
    }
}

/// Async wrapper around PTY master fd
struct AsyncPtyMaster {
    inner: AsyncFd<OwnedFd>,
}

impl AsyncRead for AsyncPtyMaster {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            let mut guard = match self.inner.poll_read_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };

            let fd = self.inner.get_ref().as_raw_fd();
            let unfilled = buf.initialize_unfilled();

            match nix::unistd::read(fd, unfilled) {
                Ok(n) => {
                    buf.advance(n);
                    return std::task::Poll::Ready(Ok(()));
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    guard.clear_ready();
                    continue;
                }
                Err(e) => {
                    return std::task::Poll::Ready(Err(std::io::Error::other(e)));
                }
            }
        }
    }
}

impl AsyncWrite for AsyncPtyMaster {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        loop {
            let mut guard = match self.inner.poll_write_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };

            match nix::unistd::write(self.inner.get_ref(), buf) {
                Ok(n) => return std::task::Poll::Ready(Ok(n)),
                Err(nix::errno::Errno::EAGAIN) => {
                    guard.clear_ready();
                    continue;
                }
                Err(e) => {
                    return std::task::Poll::Ready(Err(std::io::Error::other(e)));
                }
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct VfkitVmHandle {
    child: Arc<Mutex<Child>>,
    running: AtomicBool,
    pty: Option<Mutex<Option<Pty>>>,
}

impl VfkitVmHandle {
    fn new(child: Child, pty: Option<Pty>) -> Self {
        Self {
            child: Arc::new(Mutex::new(child)),
            running: AtomicBool::new(true),
            pty: pty.map(|p| Mutex::new(Some(p))),
        }
    }
}

#[async_trait]
impl BackendVmHandle for VfkitVmHandle {
    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn wait(&self) -> Result<i32> {
        let mut child = self.child.lock().await;
        let status = child.wait().await?;
        self.running.store(false, Ordering::SeqCst);
        Ok(status.code().unwrap_or(-1))
    }

    async fn shutdown(&self) -> Result<()> {
        let child = self.child.lock().await;
        if let Some(id) = child.id() {
            #[cfg(unix)]
            {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                let _ = signal::kill(Pid::from_raw(id as i32), Signal::SIGTERM);
            }
        }
        Ok(())
    }

    async fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        child.kill().await?;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn console_stream(&self) -> Result<Option<ConsoleStream>> {
        let Some(pty_mutex) = &self.pty else {
            return Ok(None);
        };

        let mut pty_guard = pty_mutex.lock().await;
        let Some(pty) = pty_guard.take() else {
            return Err(Error::ConsoleNotEnabled);
        };

        let async_master = pty
            .into_async_master()
            .map_err(|e| Error::StartFailed(format!("Failed to create async PTY master: {}", e)))?;

        Ok(Some(Box::new(async_master)))
    }
}
