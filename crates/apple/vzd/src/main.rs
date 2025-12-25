//! VM daemon for capsa subprocess backend (macOS only).
//!
//! This binary provides an RPC interface to the NativeVirtualizationBackend.

mod console;
mod network;
mod server;
#[cfg(test)]
mod tests;

use capsa_apple_vzd_ipc::{PipeTransport, VmService};
use futures::prelude::*;
use tarpc::server::{BaseChannel, Channel};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::info;

#[apple_main::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("capsa-apple-vzd starting");

    let server = server::VzdServer::new();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let transport = PipeTransport::new(stdin, stdout);
    let framed = Framed::new(transport, LengthDelimitedCodec::new());
    let transport = tarpc::serde_transport::new(framed, tokio_serde::formats::Bincode::default());

    BaseChannel::with_defaults(transport)
        .execute(server.serve())
        .for_each(|response| async move {
            tokio::spawn(response);
        })
        .await;

    info!("capsa-apple-vzd shutting down");
}
