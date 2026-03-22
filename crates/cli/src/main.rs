use std::path::PathBuf;

use anyhow::Result;
use clap::{ArgAction, ArgGroup, Parser};

#[derive(Debug, Parser)]
#[command(
    name = "capsa",
    version,
    about = "Start a microVM with libkrun",
    group(ArgGroup::new("boot-source").required(true).args(["root", "kernel"]))
)]
struct Cli {
    /// Path to VM root filesystem directory.
    #[arg(long, conflicts_with = "kernel")]
    root: Option<PathBuf>,

    /// Kernel image path.
    #[arg(long, conflicts_with = "root")]
    kernel: Option<PathBuf>,

    /// Optional initramfs path.
    #[arg(long, requires = "kernel")]
    initramfs: Option<PathBuf>,

    /// Optional kernel command line.
    #[arg(long = "kernel-cmdline")]
    kernel_cmdline: Option<String>,

    /// Number of virtual CPUs.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..))]
    vcpus: u8,

    /// VM memory in MiB.
    #[arg(long, default_value_t = 512, value_parser = clap::value_parser!(u32).range(1..))]
    memory_mib: u32,

    /// Increase verbosity (-v: normal logs, -vv: debug logs). Default is quiet.
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
}

impl Cli {
    fn to_vm_config(&self) -> capsa::VmConfig {
        capsa::VmConfig {
            root: self.root.clone(),
            kernel: self.kernel.clone(),
            initramfs: self.initramfs.clone(),
            kernel_cmdline: self.kernel_cmdline.clone(),
            vcpus: self.vcpus,
            memory_mib: self.memory_mib,
            verbosity: self.verbose,
            interfaces: vec![],
        }
    }
}

fn main() -> Result<()> {
    let args = Cli::parse();
    args.to_vm_config().start()
}
