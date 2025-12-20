//! The `run` command - runs a virtual machine.

use crate::console;
use capsa::{Capsa, DiskImage, LinuxDirectBootConfig, MountMode};
use clap::Args;
use std::path::PathBuf;

const MAX_CPUS: u32 = 256;
const MAX_MEMORY_MB: u32 = 1024 * 1024; // 1 TB

#[derive(Args)]
pub struct RunArgs {
    /// Path to configuration file
    #[arg(long, short)]
    config: Option<PathBuf>,

    /// Path to kernel image (for Linux VMs)
    #[arg(long, value_parser = parse_existing_file)]
    kernel: Option<PathBuf>,

    /// Path to initrd image (for Linux VMs)
    #[arg(long, value_parser = parse_existing_file)]
    initrd: Option<PathBuf>,

    /// Path to disk image
    #[arg(long, value_parser = parse_existing_file)]
    disk: Option<PathBuf>,

    /// Number of CPUs (1-256)
    #[arg(long, default_value = "1", value_parser = parse_cpus)]
    cpus: u32,

    /// Memory in MB (1-1048576)
    #[arg(long, default_value = "512", value_parser = parse_memory)]
    memory: u32,

    /// Shared directories (format: host:guest or host:guest:ro|rw)
    #[arg(long, short, value_parser = parse_share)]
    share: Vec<Share>,

    /// Disable console (by default, console is enabled and connected to stdio)
    #[arg(long)]
    no_console: bool,
}

#[derive(Debug, Clone)]
pub struct Share {
    host: String,
    guest: String,
    mode: MountMode,
}

pub async fn run(args: RunArgs) -> anyhow::Result<()> {
    let kernel = args
        .kernel
        .ok_or_else(|| anyhow::anyhow!("--kernel is required"))?;
    let initrd = args
        .initrd
        .ok_or_else(|| anyhow::anyhow!("--initrd is required"))?;

    let boot_config = LinuxDirectBootConfig::new(&kernel, &initrd);

    macro_rules! with_shares {
        ($builder:expr) => {{
            let mut b = $builder;
            for s in &args.share {
                b = b.share(&s.host, &s.guest, s.mode);
            }
            b.build().await?
        }};
    }

    let mut base = Capsa::vm(boot_config)
        .cpus(args.cpus)
        .memory_mb(args.memory);

    if !args.no_console {
        base = base.console_enabled();
    }

    let vm = match args.disk {
        Some(disk_path) => with_shares!(base.disk(DiskImage::new(disk_path))),
        None => with_shares!(base),
    };

    if args.no_console {
        eprintln!("VM started, waiting for exit...");
        let status = vm.wait().await?;
        eprintln!("VM exited with status: {:?}", status);
    } else {
        console::run_stdio_console(&vm).await?;
    }

    Ok(())
}

fn parse_cpus(s: &str) -> Result<u32, String> {
    let cpus: u32 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid number"))?;
    if cpus == 0 {
        return Err("cpus must be at least 1".to_string());
    }
    if cpus > MAX_CPUS {
        return Err(format!("cpus cannot exceed {MAX_CPUS}"));
    }
    Ok(cpus)
}

fn parse_memory(s: &str) -> Result<u32, String> {
    let memory: u32 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid number"))?;
    if memory == 0 {
        return Err("memory must be at least 1 MB".to_string());
    }
    if memory > MAX_MEMORY_MB {
        return Err(format!("memory cannot exceed {MAX_MEMORY_MB} MB"));
    }
    Ok(memory)
}

fn parse_existing_file(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if !path.exists() {
        return Err(format!("file not found: {s}"));
    }
    if !path.is_file() {
        return Err(format!("not a file: {s}"));
    }
    Ok(path)
}

fn parse_share(s: &str) -> Result<Share, String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 {
        return Err("share format must be 'host:guest' or 'host:guest:mode'".to_string());
    }
    if parts.len() > 3 {
        return Err("too many colons in share format".to_string());
    }

    let host = parts[0];
    let guest = parts[1];

    if host.is_empty() {
        return Err("host path cannot be empty".to_string());
    }
    if guest.is_empty() {
        return Err("guest path cannot be empty".to_string());
    }

    let host_path = PathBuf::from(host);
    if !host_path.exists() {
        return Err(format!("share host path not found: {host}"));
    }

    let mode = if parts.len() == 3 {
        match parts[2] {
            "ro" => MountMode::ReadOnly,
            "rw" => MountMode::ReadWrite,
            other => {
                return Err(format!(
                    "invalid mount mode '{other}', expected 'ro' or 'rw'"
                ));
            }
        }
    } else {
        MountMode::ReadOnly
    };

    Ok(Share {
        host: host.to_string(),
        guest: guest.to_string(),
        mode,
    })
}
