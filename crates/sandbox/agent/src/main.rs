//! Capsa sandbox agent.
//!
//! This binary runs inside the guest VM and handles RPC requests from the host.
//! It listens on vsock port 52 for incoming connections.
//!
//! # Security Model
//!
//! The agent runs with the same privileges as PID 1 (init) in the guest VM.
//! It can execute arbitrary commands and access the entire guest filesystem.
//! The security boundary is the VM isolation itself - the agent is trusted
//! by design within the guest environment.
//!
//! Uses a single-threaded runtime to minimize resource usage in the guest.

use capsa_sandbox_protocol::{
    AGENT_VSOCK_PORT, AgentService, DirEntry, ExecResult, MountInfo, RpcResult, SystemInfo,
};
use futures::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use tarpc::server::{self, Channel};
use tarpc::tokio_serde::formats::Bincode;
use tarpc::tokio_util::codec::LengthDelimitedCodec;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener};

const MAX_RPC_FRAME_SIZE: usize = 16 * 1024 * 1024;

fn main() {
    if let Err(e) = run() {
        eprintln!("agent error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Use enable_io() and enable_time() instead of enable_all() because
    // enable_all() also enables io_uring which may not be available in
    // minimal kernels (requires CONFIG_IO_URING)
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    rt.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "capsa-sandbox-agent starting on vsock port {}",
        AGENT_VSOCK_PORT
    );

    let mut listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, AGENT_VSOCK_PORT))?;
    println!("listening for connections...");

    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("accept error: {}", e);
                continue;
            }
        };
        println!("connection from {:?}", addr);

        let framed = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_RPC_FRAME_SIZE)
            .new_framed(stream);
        let transport = tarpc::serde_transport::new(framed, Bincode::default());

        let server = server::BaseChannel::with_defaults(transport);
        let agent = AgentServer;

        tokio::spawn(async move {
            server
                .execute(agent.serve())
                .for_each(|response| async move {
                    tokio::spawn(response);
                })
                .await;
            println!("connection closed from {:?}", addr);
        });
    }
}

#[derive(Clone)]
struct AgentServer;

impl AgentService for AgentServer {
    async fn ping(self, _ctx: tarpc::context::Context) {}

    async fn exec(
        self,
        _ctx: tarpc::context::Context,
        command: String,
        env: HashMap<String, String>,
    ) -> RpcResult<ExecResult> {
        exec_command(&command, &env)
    }

    async fn read_file(self, _ctx: tarpc::context::Context, path: String) -> RpcResult<Vec<u8>> {
        read_file_contents(&path)
    }

    async fn write_file(
        self,
        _ctx: tarpc::context::Context,
        path: String,
        contents: Vec<u8>,
    ) -> RpcResult<()> {
        write_file_contents(&path, &contents)
    }

    async fn list_dir(
        self,
        _ctx: tarpc::context::Context,
        path: String,
    ) -> RpcResult<Vec<DirEntry>> {
        list_directory(&path)
    }

    async fn exists(self, _ctx: tarpc::context::Context, path: String) -> bool {
        Path::new(&path).exists()
    }

    async fn info(self, _ctx: tarpc::context::Context) -> SystemInfo {
        get_system_info()
    }

    async fn shutdown(self, _ctx: tarpc::context::Context) -> RpcResult<()> {
        println!("shutdown requested");
        // Spawn the exit so the response can be sent first
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            std::process::exit(0);
        });
        Ok(())
    }
}

fn exec_command(command: &str, env: &HashMap<String, String>) -> RpcResult<ExecResult> {
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .envs(env)
        .output()
        .map_err(|e| format!("failed to execute command: {}", e))?;

    Ok(ExecResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

fn read_file_contents(path: &str) -> RpcResult<Vec<u8>> {
    fs::read(path).map_err(|e| format!("failed to read {}: {}", path, e))
}

fn write_file_contents(path: &str, contents: &[u8]) -> RpcResult<()> {
    fs::write(path, contents).map_err(|e| format!("failed to write {}: {}", path, e))
}

fn list_directory(path: &str) -> RpcResult<Vec<DirEntry>> {
    let entries =
        fs::read_dir(path).map_err(|e| format!("failed to read directory {}: {}", path, e))?;

    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read entry: {}", e))?;
        let metadata = entry
            .metadata()
            .map_err(|e| format!("failed to read metadata: {}", e))?;
        result.push(DirEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }

    Ok(result)
}

fn get_system_info() -> SystemInfo {
    SystemInfo {
        kernel_version: read_kernel_version(),
        hostname: read_hostname(),
        cpus: count_cpus(),
        memory_bytes: read_memory_bytes(),
        mounts: read_mounts(),
    }
}

fn read_kernel_version() -> String {
    fs::read_to_string("/proc/version")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn read_hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "sandbox".to_string())
}

fn count_cpus() -> u32 {
    fs::read_to_string("/proc/cpuinfo")
        .map(|s| s.matches("processor").count() as u32)
        .unwrap_or(1)
}

fn read_memory_bytes() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| parse_meminfo_total(&s))
        .unwrap_or(0)
}

fn parse_meminfo_total(content: &str) -> Option<u64> {
    content
        .lines()
        .find(|line| line.starts_with("MemTotal:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

fn read_mounts() -> Vec<MountInfo> {
    fs::read_to_string("/proc/mounts")
        .map(|s| parse_mounts(&s))
        .unwrap_or_default()
}

fn parse_mounts(content: &str) -> Vec<MountInfo> {
    content
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                Some(MountInfo {
                    source: parts[0].to_string(),
                    target: parts[1].to_string(),
                    fstype: parts[2].to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn exec_command_success() {
        let result = exec_command("echo hello", &HashMap::new()).unwrap();
        assert_eq!(result.stdout.trim(), "hello");
        assert!(result.stderr.is_empty());
        assert_eq!(result.exit_code, 0);
    }

    #[test]
    fn exec_command_with_env() {
        let mut env = HashMap::new();
        env.insert("TEST_VAR".to_string(), "test_value".to_string());
        let result = exec_command("echo $TEST_VAR", &env).unwrap();
        assert_eq!(result.stdout.trim(), "test_value");
    }

    #[test]
    fn exec_command_failure_exit_code() {
        let result = exec_command("exit 42", &HashMap::new()).unwrap();
        assert_eq!(result.exit_code, 42);
    }

    #[test]
    fn exec_command_stderr() {
        let result = exec_command("echo error >&2", &HashMap::new()).unwrap();
        assert_eq!(result.stderr.trim(), "error");
    }

    #[test]
    fn read_file_contents_success() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, b"test content").unwrap();

        let result = read_file_contents(file_path.to_str().unwrap()).unwrap();
        assert_eq!(result, b"test content");
    }

    #[test]
    fn read_file_contents_nonexistent() {
        let result = read_file_contents("/nonexistent/file/path");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to read"));
    }

    #[test]
    fn write_file_contents_success() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("output.txt");

        write_file_contents(file_path.to_str().unwrap(), b"written data").unwrap();
        let content = std::fs::read(&file_path).unwrap();
        assert_eq!(content, b"written data");
    }

    #[test]
    fn write_file_contents_overwrites() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("output.txt");

        write_file_contents(file_path.to_str().unwrap(), b"first").unwrap();
        write_file_contents(file_path.to_str().unwrap(), b"second").unwrap();
        let content = std::fs::read(&file_path).unwrap();
        assert_eq!(content, b"second");
    }

    #[test]
    fn list_directory_success() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("file1.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let mut entries = list_directory(dir.path().to_str().unwrap()).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file1.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].is_dir);
    }

    #[test]
    fn list_directory_empty() {
        let dir = TempDir::new().unwrap();
        let entries = list_directory(dir.path().to_str().unwrap()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_directory_nonexistent() {
        let result = list_directory("/nonexistent/directory");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("failed to read directory"));
    }

    #[test]
    fn list_directory_includes_file_size() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("sized.txt");
        std::fs::write(&file_path, b"12345").unwrap();

        let entries = list_directory(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, 5);
    }

    #[test]
    fn parse_meminfo_total_success() {
        let input = "MemTotal:       16384000 kB\nMemFree:        8192000 kB\n";
        assert_eq!(parse_meminfo_total(input), Some(16384000 * 1024));
    }

    #[test]
    fn parse_meminfo_total_missing() {
        assert_eq!(parse_meminfo_total("MemFree: 1234 kB"), None);
    }

    #[test]
    fn parse_meminfo_total_empty() {
        assert_eq!(parse_meminfo_total(""), None);
    }

    #[test]
    fn parse_mounts_success() {
        let input = "/dev/sda1 / ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n";
        let mounts = parse_mounts(input);

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].source, "/dev/sda1");
        assert_eq!(mounts[0].target, "/");
        assert_eq!(mounts[0].fstype, "ext4");
        assert_eq!(mounts[1].source, "tmpfs");
        assert_eq!(mounts[1].target, "/tmp");
        assert_eq!(mounts[1].fstype, "tmpfs");
    }

    #[test]
    fn parse_mounts_empty() {
        assert!(parse_mounts("").is_empty());
    }

    #[test]
    fn parse_mounts_ignores_short_lines() {
        let input = "/dev/sda1 /\nbad line\n/dev/sdb1 /mnt ext4\n";
        let mounts = parse_mounts(input);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].source, "/dev/sdb1");
    }
}
