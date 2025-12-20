//! The `version` command - shows version information.

use clap::Args;

#[derive(Args)]
pub struct VersionArgs {}

pub fn run(_args: VersionArgs) {
    println!("capsa {}", env!("CARGO_PKG_VERSION"));
}
