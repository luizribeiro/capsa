//! Interactive console support for connecting VM console to stdio.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(unix)]
use nix::sys::termios::{self, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg, Termios};

/// RAII guard that puts the terminal in raw mode and restores on drop.
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
        raw.input_flags.remove(InputFlags::IGNBRK);
        raw.input_flags.remove(InputFlags::BRKINT);
        raw.input_flags.remove(InputFlags::PARMRK);
        raw.input_flags.remove(InputFlags::ISTRIP);
        raw.input_flags.remove(InputFlags::INLCR);
        raw.input_flags.remove(InputFlags::IGNCR);
        raw.input_flags.remove(InputFlags::ICRNL);
        raw.input_flags.remove(InputFlags::IXON);

        raw.output_flags.remove(OutputFlags::OPOST);

        raw.local_flags.remove(LocalFlags::ECHO);
        raw.local_flags.remove(LocalFlags::ECHONL);
        raw.local_flags.remove(LocalFlags::ICANON);
        raw.local_flags.remove(LocalFlags::ISIG);
        raw.local_flags.remove(LocalFlags::IEXTEN);

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

/// Run an interactive console session, connecting VM console to stdin/stdout.
///
/// The terminal is put in raw mode so that control characters (Ctrl+C, etc.)
/// are passed to the VM. Press Ctrl+] to detach and stop the VM.
pub async fn run_stdio_console(vm: &capsa::VmHandle) -> anyhow::Result<()> {
    let console = vm.console().await?;
    let (mut reader, mut writer) = console.split().await?;

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
            let _ = vm.kill().await;
        }
        _ = stdout_task => {
            let _ = vm.kill().await;
        }
    }

    Ok(())
}
