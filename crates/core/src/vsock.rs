//! Virtio-vsock configuration types.
//!
//! vsock provides direct host-guest communication independent of network
//! configuration. Each port maps to a Unix domain socket on the host.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Configuration for a single vsock port mapping.
///
/// Maps a vsock port number to a Unix domain socket on the host.
/// Use the builder methods to create instances.
///
/// # Example
///
/// ```
/// use capsa_core::VsockPortConfig;
///
/// // Listen mode with user-provided path
/// let config = VsockPortConfig::listen(1024, "/tmp/agent.sock");
///
/// // Connect mode with auto-cleanup
/// let config = VsockPortConfig::connect(2048, "/tmp/service.sock")
///     .with_auto_cleanup();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VsockPortConfig {
    port: u32,
    socket_path: PathBuf,
    #[serde(default)]
    connect: bool,
    #[serde(default)]
    auto_cleanup: bool,
}

impl VsockPortConfig {
    /// Creates a new vsock port config for listening (guest connects to host).
    pub fn listen(port: u32, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            port,
            socket_path: socket_path.into(),
            connect: false,
            auto_cleanup: false,
        }
    }

    /// Creates a new vsock port config for listening with an auto-generated socket path.
    ///
    /// The builder will generate a unique socket path in `/tmp` and clean it up
    /// when the VM stops.
    ///
    /// This is equivalent to using `From<u32>`:
    /// ```
    /// use capsa_core::VsockPortConfig;
    ///
    /// // These are equivalent:
    /// let config1 = VsockPortConfig::listen_auto(1024);
    /// let config2 = VsockPortConfig::from(1024);
    /// ```
    pub fn listen_auto(port: u32) -> Self {
        Self {
            port,
            socket_path: PathBuf::new(), // Builder will generate the path
            connect: false,
            auto_cleanup: true,
        }
    }

    /// Creates a new vsock port config for connecting (host connects to guest).
    pub fn connect(port: u32, socket_path: impl Into<PathBuf>) -> Self {
        Self {
            port,
            socket_path: socket_path.into(),
            connect: true,
            auto_cleanup: false,
        }
    }

    /// Marks this socket for automatic cleanup when the VM stops.
    pub fn with_auto_cleanup(mut self) -> Self {
        self.auto_cleanup = true;
        self
    }

    /// Returns the vsock port number.
    pub fn port(&self) -> u32 {
        self.port
    }

    /// Returns the socket path on the host.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns true if this is connect mode (host initiates to guest).
    pub fn is_connect(&self) -> bool {
        self.connect
    }

    /// Returns true if the socket should be auto-cleaned on VM stop.
    pub fn auto_cleanup(&self) -> bool {
        self.auto_cleanup
    }
}

impl From<u32> for VsockPortConfig {
    /// Creates a vsock port config for listening with an auto-generated socket path.
    ///
    /// This is a convenience conversion for the simple case where you just want
    /// to listen on a port with an auto-managed socket.
    fn from(port: u32) -> Self {
        VsockPortConfig::listen_auto(port)
    }
}

/// Vsock device configuration for a VM.
///
/// Contains all port mappings for the vsock device.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VsockConfig {
    /// List of port configurations.
    #[serde(default)]
    pub ports: Vec<VsockPortConfig>,
}

impl VsockConfig {
    /// Creates an empty vsock configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true if any vsock ports are configured.
    pub fn is_enabled(&self) -> bool {
        !self.ports.is_empty()
    }

    /// Adds a port configuration.
    pub fn add_port(&mut self, config: VsockPortConfig) {
        self.ports.push(config);
    }

    /// Returns paths that should be cleaned up when VM stops.
    pub fn auto_cleanup_paths(&self) -> impl Iterator<Item = &Path> {
        self.ports
            .iter()
            .filter(|p| p.auto_cleanup())
            .map(|p| p.socket_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vsock_port_config_listen() {
        let config = VsockPortConfig::listen(1024, "/tmp/test.sock");
        assert_eq!(config.port(), 1024);
        assert!(!config.is_connect());
        assert!(!config.auto_cleanup());
    }

    #[test]
    fn vsock_port_config_connect() {
        let config = VsockPortConfig::connect(1024, "/tmp/test.sock");
        assert_eq!(config.port(), 1024);
        assert!(config.is_connect());
        assert!(!config.auto_cleanup());
    }

    #[test]
    fn vsock_port_config_with_auto_cleanup() {
        let config = VsockPortConfig::listen(1024, "/tmp/test.sock").with_auto_cleanup();
        assert!(config.auto_cleanup());
    }

    #[test]
    fn vsock_config_is_enabled() {
        let mut config = VsockConfig::new();
        assert!(!config.is_enabled());

        config.add_port(VsockPortConfig::listen(1024, "/tmp/test.sock"));
        assert!(config.is_enabled());
    }

    #[test]
    fn vsock_config_auto_cleanup_paths() {
        let mut config = VsockConfig::new();
        config.add_port(VsockPortConfig::listen(1024, "/tmp/user.sock"));
        config.add_port(VsockPortConfig::listen(1025, "/tmp/auto.sock").with_auto_cleanup());
        config.add_port(VsockPortConfig::listen(1026, "/tmp/auto2.sock").with_auto_cleanup());

        let cleanup_paths: Vec<_> = config.auto_cleanup_paths().collect();
        assert_eq!(cleanup_paths.len(), 2);
        assert!(cleanup_paths.iter().any(|p| p.ends_with("auto.sock")));
        assert!(cleanup_paths.iter().any(|p| p.ends_with("auto2.sock")));
    }

    #[test]
    fn vsock_config_serialization() {
        let mut config = VsockConfig::new();
        config.add_port(VsockPortConfig::listen(1024, "/tmp/test.sock"));

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: VsockConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.ports.len(), 1);
        assert_eq!(deserialized.ports[0].port(), 1024);
    }
}
