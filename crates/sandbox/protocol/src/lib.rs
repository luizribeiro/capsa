//! Shared types and RPC service definition for capsa sandbox agent.
//!
//! This crate defines the protocol between the host and the guest agent,
//! used by both `capsa` (host) and `capsa-sandbox-agent` (guest).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// vsock port for agent communication.
pub const AGENT_VSOCK_PORT: u32 = 52;

/// Result of executing a command in the guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Information about the guest system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub kernel_version: String,
    pub hostname: String,
    pub cpus: u32,
    pub memory_bytes: u64,
    pub mounts: Vec<MountInfo>,
}

/// Information about a mounted filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountInfo {
    pub source: String,
    pub target: String,
    pub fstype: String,
}

/// A directory entry returned by ListDir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub type RpcResult<T> = Result<T, String>;

/// RPC service for host-agent communication.
#[tarpc::service]
pub trait AgentService {
    /// Health check - returns immediately.
    async fn ping() -> ();

    /// Execute a command and return the result.
    async fn exec(command: String, env: HashMap<String, String>) -> RpcResult<ExecResult>;

    /// Read a file's contents.
    async fn read_file(path: String) -> RpcResult<Vec<u8>>;

    /// Write contents to a file.
    async fn write_file(path: String, contents: Vec<u8>) -> RpcResult<()>;

    /// List directory contents.
    async fn list_dir(path: String) -> RpcResult<Vec<DirEntry>>;

    /// Check if a path exists.
    async fn exists(path: String) -> bool;

    /// Get system information.
    async fn info() -> SystemInfo;

    /// Request VM shutdown.
    async fn shutdown() -> RpcResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    mod exec_result {
        use super::*;

        #[test]
        fn serialization_roundtrip() {
            let result = ExecResult {
                stdout: "hello".to_string(),
                stderr: "".to_string(),
                exit_code: 0,
            };
            let json = serde_json::to_string(&result).unwrap();
            let deserialized: ExecResult = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.stdout, "hello");
            assert_eq!(deserialized.exit_code, 0);
        }
    }

    mod system_info {
        use super::*;

        #[test]
        fn serialization_roundtrip() {
            let info = SystemInfo {
                kernel_version: "6.1.0".to_string(),
                hostname: "sandbox".to_string(),
                cpus: 4,
                memory_bytes: 1024 * 1024 * 1024,
                mounts: vec![MountInfo {
                    source: "share0".to_string(),
                    target: "/mnt".to_string(),
                    fstype: "virtiofs".to_string(),
                }],
            };
            let json = serde_json::to_string(&info).unwrap();
            let deserialized: SystemInfo = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.kernel_version, "6.1.0");
            assert_eq!(deserialized.cpus, 4);
            assert_eq!(deserialized.mounts.len(), 1);
        }
    }

    mod dir_entry {
        use super::*;

        #[test]
        fn serialization_roundtrip() {
            let entry = DirEntry {
                name: "file.txt".to_string(),
                is_dir: false,
                size: 1024,
            };
            let json = serde_json::to_string(&entry).unwrap();
            let deserialized: DirEntry = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.name, "file.txt");
            assert!(!deserialized.is_dir);
        }
    }

    mod rpc_result {
        use super::*;

        #[test]
        fn ok_result_serializes() {
            let result: RpcResult<ExecResult> = Ok(ExecResult {
                stdout: "success".to_string(),
                stderr: "".to_string(),
                exit_code: 0,
            });
            let json = serde_json::to_string(&result).unwrap();
            let deserialized: RpcResult<ExecResult> = serde_json::from_str(&json).unwrap();
            assert!(deserialized.is_ok());
        }

        #[test]
        fn err_result_serializes() {
            let result: RpcResult<ExecResult> = Err("command failed".to_string());
            let json = serde_json::to_string(&result).unwrap();
            let deserialized: RpcResult<ExecResult> = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.unwrap_err(), "command failed");
        }
    }
}
