//! Host-side client for communicating with the sandbox agent.
//!
//! # Security Considerations
//!
//! The [`AgentClient`] provides unrestricted access to the guest VM's filesystem
//! and command execution. The security boundary is the VM isolation itself.
//! The agent is trusted by design within the guest environment.

use crate::vsock::VsockSocket;
use capsa_core::{Error, Result};
use capsa_sandbox_protocol::{
    AGENT_VSOCK_PORT, AgentServiceClient, DirEntry, ExecResult, SystemInfo,
};
use std::collections::HashMap;
use std::time::Duration;
use tarpc::tokio_serde::formats::Bincode;
use tarpc::tokio_util::codec::LengthDelimitedCodec;
use tokio::time::{interval, timeout};

/// Maximum RPC frame size (16MB).
///
/// This limit prevents memory exhaustion from malicious or buggy guests.
/// Large file transfers should use chunking.
const MAX_RPC_FRAME_SIZE: usize = 16 * 1024 * 1024;

const DEFAULT_WAIT_READY_TIMEOUT: Duration = Duration::from_secs(30);
const PING_INTERVAL: Duration = Duration::from_millis(100);

/// Client for communicating with the sandbox agent running in the guest.
pub struct AgentClient {
    client: AgentServiceClient,
}

impl AgentClient {
    /// Connects to the sandbox agent via the vsock socket.
    pub async fn connect(socket: &VsockSocket) -> Result<Self> {
        let stream = socket
            .connect()
            .await
            .map_err(|e| Error::Agent(format!("failed to connect to agent: {}", e)))?;

        let framed = LengthDelimitedCodec::builder()
            .max_frame_length(MAX_RPC_FRAME_SIZE)
            .new_framed(stream);
        let transport = tarpc::serde_transport::new(framed, Bincode::default());

        let client = AgentServiceClient::new(tarpc::client::Config::default(), transport).spawn();

        Ok(Self { client })
    }

    /// Pings the agent to check if it's responding.
    pub async fn ping(&self) -> Result<()> {
        self.client
            .ping(tarpc::context::current())
            .await
            .map_err(|e| Error::Agent(format!("ping failed: {}", e)))
    }

    /// Executes a command in the guest and returns the result.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let result = agent.exec("ls -la /mnt", HashMap::new()).await?;
    /// println!("stdout: {}", result.stdout);
    /// ```
    pub async fn exec(&self, command: &str, env: HashMap<String, String>) -> Result<ExecResult> {
        self.client
            .exec(tarpc::context::current(), command.to_string(), env)
            .await
            .map_err(|e| Error::Agent(format!("exec failed: {}", e)))?
            .map_err(Error::Agent)
    }

    /// Reads a file from the guest filesystem.
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.client
            .read_file(tarpc::context::current(), path.to_string())
            .await
            .map_err(|e| Error::Agent(format!("read_file failed: {}", e)))?
            .map_err(Error::Agent)
    }

    /// Writes contents to a file in the guest filesystem.
    pub async fn write_file(&self, path: &str, contents: &[u8]) -> Result<()> {
        self.client
            .write_file(
                tarpc::context::current(),
                path.to_string(),
                contents.to_vec(),
            )
            .await
            .map_err(|e| Error::Agent(format!("write_file failed: {}", e)))?
            .map_err(Error::Agent)
    }

    /// Lists the contents of a directory in the guest filesystem.
    pub async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.client
            .list_dir(tarpc::context::current(), path.to_string())
            .await
            .map_err(|e| Error::Agent(format!("list_dir failed: {}", e)))?
            .map_err(Error::Agent)
    }

    /// Checks if a path exists in the guest filesystem.
    pub async fn exists(&self, path: &str) -> Result<bool> {
        self.client
            .exists(tarpc::context::current(), path.to_string())
            .await
            .map_err(|e| Error::Agent(format!("exists failed: {}", e)))
    }

    /// Returns information about the guest system.
    pub async fn info(&self) -> Result<SystemInfo> {
        self.client
            .info(tarpc::context::current())
            .await
            .map_err(|e| Error::Agent(format!("info failed: {}", e)))
    }

    /// Requests the guest VM to shutdown.
    pub async fn shutdown(&self) -> Result<()> {
        self.client
            .shutdown(tarpc::context::current())
            .await
            .map_err(|e| Error::Agent(format!("shutdown failed: {}", e)))?
            .map_err(Error::Agent)
    }
}

/// Waits for the sandbox agent to become ready.
///
/// This function repeatedly tries to connect and ping the agent until
/// it responds or the timeout is reached (default: 30 seconds).
pub async fn wait_ready(socket: &VsockSocket) -> Result<AgentClient> {
    wait_ready_timeout(socket, DEFAULT_WAIT_READY_TIMEOUT).await
}

/// Waits for the sandbox agent to become ready with a custom timeout.
pub async fn wait_ready_timeout(
    socket: &VsockSocket,
    wait_timeout: Duration,
) -> Result<AgentClient> {
    let result = timeout(wait_timeout, async {
        let mut interval = interval(PING_INTERVAL);

        loop {
            match AgentClient::connect(socket).await {
                Ok(client) => match client.ping().await {
                    Ok(()) => return Ok(client),
                    Err(e) => {
                        tracing::debug!("agent ping failed: {}, retrying...", e);
                    }
                },
                Err(e) => {
                    tracing::debug!("agent connection failed: {}, retrying...", e);
                }
            }

            interval.tick().await;
        }
    })
    .await;

    match result {
        Ok(Ok(client)) => Ok(client),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Timeout(format!(
            "agent not ready after {:?}",
            wait_timeout
        ))),
    }
}

/// Returns the vsock port used by the sandbox agent.
pub fn agent_port() -> u32 {
    AGENT_VSOCK_PORT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_port_is_52() {
        assert_eq!(agent_port(), 52);
    }

    #[test]
    fn default_timeout_is_30_seconds() {
        assert_eq!(DEFAULT_WAIT_READY_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn ping_interval_is_100ms() {
        assert_eq!(PING_INTERVAL, Duration::from_millis(100));
    }

    #[test]
    fn max_frame_size_is_16mb() {
        assert_eq!(MAX_RPC_FRAME_SIZE, 16 * 1024 * 1024);
    }
}
