use capsa::{Capsa, DiskImage, LinuxDirectBootConfig, MountMode};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(unix)]
use nix::sys::termios::{self, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg, Termios};

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
        #[arg(long)]
        kernel: Option<PathBuf>,

        /// Path to initrd image (for Linux VMs)
        #[arg(long)]
        initrd: Option<PathBuf>,

        /// Path to disk image
        #[arg(long)]
        disk: Option<PathBuf>,

        /// Number of CPUs
        #[arg(long, default_value = "1")]
        cpus: u32,

        /// Memory in MB
        #[arg(long, default_value = "512")]
        memory: u32,

        /// Shared directories (format: host:guest:mode)
        #[arg(long, short)]
        share: Vec<String>,

        /// Console mode (disabled, enabled, stdio)
        #[arg(long, default_value = "stdio")]
        console: String,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

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
            console,
        } => {
            let kernel = kernel.ok_or_else(|| anyhow::anyhow!("--kernel is required"))?;
            let initrd = initrd.ok_or_else(|| anyhow::anyhow!("--initrd is required"))?;

            let boot_config = LinuxDirectBootConfig::new(&kernel, &initrd);

            let mut builder = Capsa::vm(boot_config).cpus(cpus).memory_mb(memory);

            if let Some(disk_path) = disk {
                builder = builder.disk(DiskImage::new(disk_path));
            }

            for s in share {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() >= 2 {
                    let host = parts[0];
                    let guest = parts[1];
                    let mode = if parts.len() > 2 && parts[2] == "rw" {
                        MountMode::ReadWrite
                    } else {
                        MountMode::ReadOnly
                    };
                    builder = builder.share(host, guest, mode);
                }
            }

            let use_stdio = console == "stdio";
            builder = match console.as_str() {
                "disabled" => builder.console(capsa::ConsoleMode::Disabled),
                "enabled" => builder.console_enabled(),
                _ => builder.console_stdio(),
            };

            let vm = builder.build().await?;

            if use_stdio {
                run_stdio_console(&vm).await?;
            } else {
                eprintln!("VM started, waiting for exit...");
                let status = vm.wait().await?;
                eprintln!("VM exited with status: {:?}", status);
            }
        }

        Commands::Backends { json } => {
            if json {
                println!(
                    r#"{{
  "backends": [
    {{
      "name": "vfkit",
      "platform": "macos",
      "available": true,
      "capabilities": {{
        "linux": true,
        "virtio_fs": true,
        "vsock": true
      }}
    }}
  ]
}}"#
                );
            } else {
                println!("Available backends:");
                println!();
                println!("  vfkit (macOS)");
                println!("    Status: Available");
                println!("    Linux support: Yes");
                println!("    virtio-fs: Yes");
                println!("    vsock: Yes");
            }
        }

        Commands::Version => {
            println!("capsa {}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}

#[cfg(unix)]
struct RawTerminalGuard {
    original: Termios,
}

#[cfg(unix)]
impl RawTerminalGuard {
    fn new() -> Option<Self> {
        use std::os::fd::BorrowedFd;

        // SAFETY: stdin fd 0 is valid for the lifetime of the program
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(0) };
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
        use std::os::fd::BorrowedFd;
        let stdin_fd = unsafe { BorrowedFd::borrow_raw(0) };
        let _ = termios::tcsetattr(stdin_fd, SetArg::TCSANOW, &self.original);
    }
}

async fn run_stdio_console(vm: &capsa::VmHandle) -> anyhow::Result<()> {
    let console = vm.console().await?;
    let (mut reader, mut writer) = console.split().await?;

    // Put terminal in raw mode so Ctrl+C etc go to the VM
    #[cfg(unix)]
    let _raw_guard = RawTerminalGuard::new();

    // Ignore SIGINT so Ctrl+C doesn't kill us (we pass it to the VM instead)
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }

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
            let _ = vm.stop().await;
        }
        _ = stdin_task => {
            // EOF on stdin, stop the VM
            let _ = vm.stop().await;
        }
        _ = stdout_task => {
            // Console closed, stop the VM
            let _ = vm.stop().await;
        }
    }

    Ok(())
}
