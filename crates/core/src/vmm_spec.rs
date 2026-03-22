use serde::{Deserialize, Serialize};

use crate::config::VmConfig;

/// Resolved network interface with launcher-assigned fd.
///
/// Not part of user-facing config. Serialized only inside `VmmLaunchSpec`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedNetworkInterface {
    /// MAC address (resolved: always populated, non-zero).
    pub mac: [u8; 6],
    /// FD number in the VMM sidecar process (assigned by launcher, must be >= 0).
    pub guest_fd: i32,
}

/// Launcher -> VMM JSON specification.
///
/// This is an internal contract consumed by the VMM sidecar, not the user-facing API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmmLaunchSpec {
    pub vm_config: VmConfig,
    #[serde(default)]
    pub resolved_interfaces: Vec<ResolvedNetworkInterface>,
}
