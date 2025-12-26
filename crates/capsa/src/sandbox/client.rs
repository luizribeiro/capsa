//! Host-side client for communicating with the sandbox agent.

use crate::vsock::VsockSocket;
use capsa_core::{Error, Result};
use capsa_sandbox_protocol::{AGENT_VSOCK_PORT, AgentServiceClient};
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
