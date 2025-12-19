mod transport;

pub use transport::PipeTransport;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    pub disk: Option<DiskConfig>,
    pub cmdline: String,
    pub cpus: u32,
    pub memory_mb: u32,
    pub shares: Vec<SharedDirConfig>,
    pub network: NetworkMode,
    pub console_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskConfig {
    pub path: PathBuf,
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedDirConfig {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum NetworkMode {
    None,
    Nat,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct VmHandleId(pub u64);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum VmState {
    Running,
    Stopped,
    Error,
}

pub type RpcResult<T> = Result<T, String>;

#[tarpc::service]
pub trait VmService {
    async fn is_available() -> bool;
    async fn start(config: VmConfig, console_socket_path: Option<String>) -> RpcResult<VmHandleId>;
    async fn is_running(handle: VmHandleId) -> RpcResult<bool>;
    async fn wait(handle: VmHandleId) -> RpcResult<i32>;
    async fn shutdown(handle: VmHandleId) -> RpcResult<()>;
    async fn kill(handle: VmHandleId) -> RpcResult<()>;
    async fn release(handle: VmHandleId) -> RpcResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_vm_config() -> VmConfig {
        VmConfig {
            kernel: "/path/to/kernel".into(),
            initrd: "/path/to/initrd".into(),
            disk: Some(DiskConfig {
                path: "/path/to/disk.raw".into(),
                read_only: false,
            }),
            cmdline: "console=hvc0 root=/dev/vda".to_string(),
            cpus: 2,
            memory_mb: 1024,
            shares: vec![SharedDirConfig {
                host_path: "/host/path".into(),
                guest_path: "/guest/path".to_string(),
                read_only: true,
            }],
            network: NetworkMode::Nat,
            console_enabled: true,
        }
    }

    mod vm_config {
        use super::*;

        #[test]
        fn serialization_roundtrip() {
            let config = sample_vm_config();
            let json = serde_json::to_string(&config).unwrap();
            let deserialized: VmConfig = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.kernel, config.kernel);
            assert_eq!(deserialized.initrd, config.initrd);
            assert_eq!(deserialized.cmdline, config.cmdline);
            assert_eq!(deserialized.cpus, config.cpus);
            assert_eq!(deserialized.memory_mb, config.memory_mb);
            assert_eq!(deserialized.console_enabled, config.console_enabled);
        }

        #[test]
        fn serialization_without_disk() {
            let mut config = sample_vm_config();
            config.disk = None;
            let json = serde_json::to_string(&config).unwrap();
            let deserialized: VmConfig = serde_json::from_str(&json).unwrap();
            assert!(deserialized.disk.is_none());
        }

        #[test]
        fn serialization_with_empty_shares() {
            let mut config = sample_vm_config();
            config.shares = vec![];
            let json = serde_json::to_string(&config).unwrap();
            let deserialized: VmConfig = serde_json::from_str(&json).unwrap();
            assert!(deserialized.shares.is_empty());
        }
    }

    mod disk_config {
        use super::*;

        #[test]
        fn serialization_roundtrip() {
            let disk = DiskConfig {
                path: "/path/to/disk.qcow2".into(),
                read_only: true,
            };
            let json = serde_json::to_string(&disk).unwrap();
            let deserialized: DiskConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.path, disk.path);
            assert_eq!(deserialized.read_only, disk.read_only);
        }
    }

    mod shared_dir_config {
        use super::*;

        #[test]
        fn serialization_roundtrip() {
            let share = SharedDirConfig {
                host_path: "/host/workspace".into(),
                guest_path: "/workspace".to_string(),
                read_only: false,
            };
            let json = serde_json::to_string(&share).unwrap();
            let deserialized: SharedDirConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.host_path, share.host_path);
            assert_eq!(deserialized.guest_path, share.guest_path);
            assert_eq!(deserialized.read_only, share.read_only);
        }
    }

    mod network_mode {
        use super::*;

        #[test]
        fn serializes_variants() {
            assert!(
                serde_json::to_string(&NetworkMode::None)
                    .unwrap()
                    .contains("None")
            );
            assert!(
                serde_json::to_string(&NetworkMode::Nat)
                    .unwrap()
                    .contains("Nat")
            );
        }

        #[test]
        fn deserializes_variants() {
            let none: NetworkMode = serde_json::from_str("\"None\"").unwrap();
            assert!(matches!(none, NetworkMode::None));

            let nat: NetworkMode = serde_json::from_str("\"Nat\"").unwrap();
            assert!(matches!(nat, NetworkMode::Nat));
        }
    }

    mod vm_handle_id {
        use super::*;
        use std::collections::HashSet;

        #[test]
        fn equality() {
            let id1 = VmHandleId(42);
            let id2 = VmHandleId(42);
            let id3 = VmHandleId(43);

            assert_eq!(id1, id2);
            assert_ne!(id1, id3);
        }

        #[test]
        fn hashable() {
            let mut set = HashSet::new();
            set.insert(VmHandleId(1));
            set.insert(VmHandleId(2));
            set.insert(VmHandleId(1)); // duplicate

            assert_eq!(set.len(), 2);
        }

        #[test]
        fn serialization_roundtrip() {
            let id = VmHandleId(12345);
            let json = serde_json::to_string(&id).unwrap();
            let deserialized: VmHandleId = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, id);
        }
    }

    mod vm_state {
        use super::*;

        #[test]
        fn serializes_variants() {
            assert!(
                serde_json::to_string(&VmState::Running)
                    .unwrap()
                    .contains("Running")
            );
            assert!(
                serde_json::to_string(&VmState::Stopped)
                    .unwrap()
                    .contains("Stopped")
            );
            assert!(
                serde_json::to_string(&VmState::Error)
                    .unwrap()
                    .contains("Error")
            );
        }

        #[test]
        fn deserializes_variants() {
            let running: VmState = serde_json::from_str("\"Running\"").unwrap();
            assert!(matches!(running, VmState::Running));

            let stopped: VmState = serde_json::from_str("\"Stopped\"").unwrap();
            assert!(matches!(stopped, VmState::Stopped));

            let error: VmState = serde_json::from_str("\"Error\"").unwrap();
            assert!(matches!(error, VmState::Error));
        }
    }

    mod rpc_result {
        use super::*;

        #[test]
        fn ok_result_serializes() {
            let result: RpcResult<i32> = Ok(42);
            let json = serde_json::to_string(&result).unwrap();
            let deserialized: RpcResult<i32> = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.unwrap(), 42);
        }

        #[test]
        fn err_result_serializes() {
            let result: RpcResult<i32> = Err("something went wrong".to_string());
            let json = serde_json::to_string(&result).unwrap();
            let deserialized: RpcResult<i32> = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.unwrap_err(), "something went wrong");
        }
    }
}
