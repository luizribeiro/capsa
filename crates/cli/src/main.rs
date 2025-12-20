use capsa::{BackendInfo, Capsa, DiskImage, LinuxDirectBootConfig, MountMode, available_backends};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(unix)]
use nix::sys::termios::{self, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg, Termios};

fn print_backends_json(backends: &[BackendInfo]) {
    println!("{{");
    println!("  \"backends\": [");
    for (i, backend) in backends.iter().enumerate() {
        let caps = &backend.capabilities;
        println!("    {{");
        println!("      \"name\": \"{}\",", backend.name);
        println!("      \"platform\": \"{}\",", backend.platform);
        println!("      \"available\": {},", backend.available);
        println!("      \"capabilities\": {{");
        println!("        \"guest_os\": {{");
        println!("          \"linux\": {}", caps.guest_os.linux);
        println!("        }},");
        println!("        \"boot_methods\": {{");
        println!(
            "          \"linux_direct\": {}",
            caps.boot_methods.linux_direct
        );
        println!("        }},");
        println!("        \"image_formats\": {{");
        println!("          \"raw\": {},", caps.image_formats.raw);
        println!("          \"qcow2\": {}", caps.image_formats.qcow2);
        println!("        }},");
        println!("        \"network_modes\": {{");
        println!("          \"none\": {},", caps.network_modes.none);
        println!("          \"nat\": {}", caps.network_modes.nat);
        println!("        }},");
        println!("        \"share_mechanisms\": {{");
        println!(
            "          \"virtio_fs\": {},",
            caps.share_mechanisms.virtio_fs
        );
        println!(
            "          \"virtio_9p\": {}",
            caps.share_mechanisms.virtio_9p
        );
        println!("        }},");
        println!(
            "        \"max_cpus\": {},",
            caps.max_cpus.map_or("null".to_string(), |n| n.to_string())
        );
        println!(
            "        \"max_memory_mb\": {}",
            caps.max_memory_mb
                .map_or("null".to_string(), |n| n.to_string())
        );
        println!("      }}");
        if i < backends.len() - 1 {
            println!("    }},");
        } else {
            println!("    }}");
        }
    }
    println!("  ]");
    println!("}}");
}

fn print_backends_text(backends: &[BackendInfo]) {
    if backends.is_empty() {
        println!("No backends available.");
        return;
    }

    println!("Available backends:");
    println!();

    for backend in backends {
        let caps = &backend.capabilities;
        let status = if backend.available {
            "Available"
        } else {
            "Not available"
        };

        println!("  {} ({})", backend.name, backend.platform);
        println!("    Status: {status}");
        println!("    Guest OS: Linux={}", yes_no(caps.guest_os.linux));
        println!(
            "    Boot methods: direct={}",
            yes_no(caps.boot_methods.linux_direct)
        );
        println!(
            "    Disk formats: raw={}, qcow2={}",
            yes_no(caps.image_formats.raw),
            yes_no(caps.image_formats.qcow2)
        );
        println!(
            "    Network: none={}, nat={}",
            yes_no(caps.network_modes.none),
            yes_no(caps.network_modes.nat)
        );
        println!(
            "    Shares: virtio-fs={}, 9p={}",
            yes_no(caps.share_mechanisms.virtio_fs),
            yes_no(caps.share_mechanisms.virtio_9p)
        );
        if caps.max_cpus.is_some() || caps.max_memory_mb.is_some() {
            println!(
                "    Limits: cpus={}, memory={}",
                caps.max_cpus
                    .map_or("unlimited".to_string(), |n| n.to_string()),
                caps.max_memory_mb
                    .map_or("unlimited".to_string(), |n| format!("{n} MB"))
            );
        }
        println!();
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

const MAX_CPUS: u32 = 256;
const MAX_MEMORY_MB: u32 = 1024 * 1024; // 1 TB

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

#[derive(Debug, Clone)]
struct Share {
    host: String,
    guest: String,
    mode: MountMode,
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
    Run {
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
    },

    /// Show available backends and their capabilities
    Backends {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Show version information
    Version,
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
        Commands::Run {
            config: _,
            kernel,
            initrd,
            disk,
            cpus,
            memory,
            share,
            no_console,
        } => {
            let kernel = kernel.ok_or_else(|| anyhow::anyhow!("--kernel is required"))?;
            let initrd = initrd.ok_or_else(|| anyhow::anyhow!("--initrd is required"))?;

            let boot_config = LinuxDirectBootConfig::new(&kernel, &initrd);

            macro_rules! with_shares {
                ($builder:expr) => {{
                    let mut b = $builder;
                    for s in &share {
                        b = b.share(&s.host, &s.guest, s.mode);
                    }
                    b.build().await?
                }};
            }

            let mut base = Capsa::linux(boot_config).cpus(cpus).memory_mb(memory);

            if !no_console {
                base = base.console_enabled();
            }

            let vm = match disk {
                Some(disk_path) => with_shares!(base.disk(DiskImage::new(disk_path))),
                None => with_shares!(base),
            };

            if no_console {
                eprintln!("VM started, waiting for exit...");
                let status = vm.wait().await?;
                eprintln!("VM exited with status: {:?}", status);
            } else {
                run_stdio_console(&vm).await?;
            }
        }

        Commands::Backends { json } => {
            let backends = available_backends();

            if json {
                print_backends_json(&backends);
            } else {
                print_backends_text(&backends);
            }
        }

        Commands::Version => {
            println!("capsa {}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}

// TODO: split console stuff out of capsa-cli/src/main.rs

#[cfg(unix)]
struct RawTerminalGuard {
    original: Termios,
}

#[cfg(unix)]
impl RawTerminalGuard {
    fn new() -> Option<Self> {
        use std::os::fd::AsFd;

        let stdin = std::io::stdin();
        let stdin_fd = stdin.as_fd();
        let original = termios::tcgetattr(stdin_fd).ok()?;
        let mut raw = original.clone();

        // Equivalent to cfmakeraw() - full raw mode
        // Input flags
        raw.input_flags.remove(InputFlags::IGNBRK);
        raw.input_flags.remove(InputFlags::BRKINT);
        raw.input_flags.remove(InputFlags::PARMRK);
        raw.input_flags.remove(InputFlags::ISTRIP);
        raw.input_flags.remove(InputFlags::INLCR);
        raw.input_flags.remove(InputFlags::IGNCR);
        raw.input_flags.remove(InputFlags::ICRNL);
        raw.input_flags.remove(InputFlags::IXON);

        // Output flags
        raw.output_flags.remove(OutputFlags::OPOST);

        // Local flags
        raw.local_flags.remove(LocalFlags::ECHO);
        raw.local_flags.remove(LocalFlags::ECHONL);
        raw.local_flags.remove(LocalFlags::ICANON);
        raw.local_flags.remove(LocalFlags::ISIG);
        raw.local_flags.remove(LocalFlags::IEXTEN);

        // Control flags
        raw.control_flags.remove(ControlFlags::CSIZE);
        raw.control_flags.remove(ControlFlags::PARENB);
        raw.control_flags.insert(ControlFlags::CS8);

        termios::tcsetattr(stdin_fd, SetArg::TCSANOW, &raw).ok()?;
        Some(Self { original })
    }
}

#[cfg(unix)]
impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let stdin_fd = stdin.as_fd();
        let _ = termios::tcsetattr(stdin_fd, SetArg::TCSANOW, &self.original);
    }
}

async fn run_stdio_console(vm: &capsa::VmHandle) -> anyhow::Result<()> {
    let console = vm.console().await?;
    let (mut reader, mut writer) = console.split().await?;

    // Put terminal in raw mode so Ctrl+C etc go to the VM.
    // Raw mode disables ISIG, so Ctrl+C is passed as byte 0x03 instead of generating SIGINT.
    #[cfg(unix)]
    let _raw_guard = RawTerminalGuard::new();

    eprintln!("Connected to VM console. Press Ctrl+] to exit.\r");

    let (detach_tx, mut detach_rx) = tokio::sync::oneshot::channel::<()>();

    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 1];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(1) => {
                    // Ctrl+] (0x1D) is the escape sequence to exit
                    if buf[0] == 0x1D {
                        let _ = detach_tx.send(());
                        break;
                    }
                    if writer.write_all(&buf).await.is_err() {
                        break;
                    }
                }
                Ok(_) | Err(_) => break,
            }
        }
    });

    let stdout_task = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if stdout.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = stdout.flush().await;
                }
                Err(_) => break,
            }
        }
    });

    tokio::select! {
        _ = vm.wait() => {
            eprintln!("\r\nVM exited.\r");
        }
        _ = &mut detach_rx => {
            eprintln!("\r\nStopping VM...\r");
            let _ = vm.kill().await;
        }
        _ = stdin_task => {
            // EOF on stdin, kill the VM
            let _ = vm.kill().await;
        }
        _ = stdout_task => {
            // Console closed, kill the VM
            let _ = vm.kill().await;
        }
    }

    Ok(())
}
