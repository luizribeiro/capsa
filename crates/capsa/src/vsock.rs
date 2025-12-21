//! Vsock socket interface for host-guest communication.
//!
//! Provides an ergonomic API for connecting to vsock services running in VMs.

use std::path::{Path, PathBuf};
use tokio::net::UnixStream;

/// A vsock socket endpoint for host-guest communication.
///
/// This represents a Unix domain socket that's bridged to a vsock port
/// inside the VM. Use [`connect`](VsockSocket::connect) to establish
/// a connection to a service running in the guest.
///
/// # Example
///
/// ```rust,no_run
/// # use capsa::{Capsa, LinuxDirectBootConfig};
/// # async fn example() -> capsa::Result<()> {
/// let config = LinuxDirectBootConfig::new("./kernel", "./initrd");
/// let vm = Capsa::vm(config)
///     .vsock_listen(1024)  // Guest will connect to this port
///     .build().await?;
///
/// // Get the socket and connect
/// if let Some(socket) = vm.vsock_socket(1024) {
///     let stream = socket.connect().await?;
///     // Use stream for bidirectional communication
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct VsockSocket {
    port: u32,
    path: PathBuf,
}

impl VsockSocket {
    pub(crate) fn new(port: u32, path: PathBuf) -> Self {
        Self { port, path }
    }

    /// Returns the vsock port number (guest-side).
    pub fn port(&self) -> u32 {
        self.port
    }

    /// Returns the Unix socket path on the host.
    ///
    /// Use this if you need to connect using a custom method or pass
    /// the path to another process.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Connects to the vsock socket asynchronously.
    ///
    /// Returns a [`UnixStream`] that can be used for bidirectional
    /// communication with the guest service.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails (e.g., socket doesn't
    /// exist yet, guest service not listening, permission denied).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use capsa::VsockSocket;
    /// # use tokio::io::{AsyncReadExt, AsyncWriteExt};
    /// # async fn example(socket: &VsockSocket) -> std::io::Result<()> {
    /// let mut stream = socket.connect().await?;
    ///
    /// // Write to guest
    /// stream.write_all(b"hello guest").await?;
    ///
    /// // Read response
    /// let mut buf = [0u8; 1024];
    /// let n = stream.read(&mut buf).await?;
    /// println!("Guest said: {}", String::from_utf8_lossy(&buf[..n]));
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect(&self) -> std::io::Result<UnixStream> {
        UnixStream::connect(&self.path).await.map_err(|e| {
            let hint = match e.kind() {
                std::io::ErrorKind::NotFound => {
                    " (socket not ready - is the guest service listening?)"
                }
                std::io::ErrorKind::ConnectionRefused => {
                    " (connection refused - guest service may have stopped)"
                }
                _ => "",
            };
            std::io::Error::new(
                e.kind(),
                format!(
                    "failed to connect to vsock port {} at {:?}: {}{}",
                    self.port, self.path, e, hint
                ),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_socket_with_port_and_path() {
        let socket = VsockSocket::new(1024, PathBuf::from("/tmp/test.sock"));
        assert_eq!(socket.port(), 1024);
        assert_eq!(socket.path(), Path::new("/tmp/test.sock"));
    }

    #[test]
    fn path_returns_reference() {
        let socket = VsockSocket::new(1024, PathBuf::from("/tmp/test.sock"));
        let path: &Path = socket.path();
        assert_eq!(path, Path::new("/tmp/test.sock"));
    }

    #[test]
    fn socket_is_clone() {
        let socket = VsockSocket::new(1024, PathBuf::from("/tmp/test.sock"));
        let cloned = socket.clone();
        assert_eq!(cloned.port(), socket.port());
        assert_eq!(cloned.path(), socket.path());
    }

    #[tokio::test]
    async fn connect_fails_for_nonexistent_socket() {
        let socket = VsockSocket::new(1024, PathBuf::from("/nonexistent/path.sock"));
        let result = socket.connect().await;
        assert!(result.is_err());
    }
}
