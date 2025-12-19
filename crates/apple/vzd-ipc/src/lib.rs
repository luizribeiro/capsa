mod transport;

pub use transport::PipeTransport;

pub use capsa_core::{
    ConsoleMode, DiskImage, MountMode, NetworkMode, ResourceConfig, SharedDir, VmConfig,
};

use serde::{Deserialize, Serialize};

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
    use std::collections::HashSet;

    mod vm_handle_id {
        use super::*;

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
