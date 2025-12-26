//! Capsa sandbox agent.
//!
//! This binary runs inside the guest VM and handles RPC requests from the host.
//! It listens on vsock port 52 for incoming connections.
//!
//! Uses a single-threaded runtime to minimize resource usage in the guest.

use capsa_sandbox_protocol::{
    AGENT_VSOCK_PORT, AgentService, DirEntry, ExecResult, RpcResult, SystemInfo,
};
use futures::prelude::*;
use std::collections::HashMap;
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
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
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
        _command: String,
        _env: HashMap<String, String>,
    ) -> RpcResult<ExecResult> {
        Err("exec not yet implemented".to_string())
    }

    async fn read_file(self, _ctx: tarpc::context::Context, _path: String) -> RpcResult<Vec<u8>> {
        Err("read_file not yet implemented".to_string())
    }

    async fn write_file(
        self,
        _ctx: tarpc::context::Context,
        _path: String,
        _contents: Vec<u8>,
    ) -> RpcResult<()> {
        Err("write_file not yet implemented".to_string())
    }

    async fn list_dir(
        self,
        _ctx: tarpc::context::Context,
        _path: String,
    ) -> RpcResult<Vec<DirEntry>> {
        Err("list_dir not yet implemented".to_string())
    }

    async fn exists(self, _ctx: tarpc::context::Context, _path: String) -> bool {
        false
    }

    async fn info(self, _ctx: tarpc::context::Context) -> SystemInfo {
        SystemInfo {
            kernel_version: "unknown".to_string(),
            hostname: "sandbox".to_string(),
            cpus: 1,
            memory_bytes: 0,
            mounts: vec![],
        }
    }

    async fn shutdown(self, _ctx: tarpc::context::Context) -> RpcResult<()> {
        Err("shutdown not yet implemented".to_string())
    }
}
