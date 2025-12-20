mod commands;
mod console;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "capsa")]
#[command(about = "A cross-platform VM runtime for secure workload isolation")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a virtual machine
    Run(commands::run::RunArgs),

    /// Show available backends and their capabilities
    Backends(commands::backends::BackendsArgs),

    /// Show version information
    Version(commands::version::VersionArgs),
}

#[apple_main::main]
async fn main() {
    tracing_subscriber::fmt::init();

    if let Err(e) = run().await {
        eprintln!("Error: {e:?}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run(args) => commands::run::run(args).await?,
        Commands::Backends(args) => commands::backends::run(args),
        Commands::Version(args) => commands::version::run(args),
    }

    Ok(())
}
