//! Capsa sandbox init process (PID 1).
//!
//! This binary runs as PID 1 in the sandbox VM and handles:
//! - Parsing kernel cmdline for capsa.mount= and capsa.run= parameters
//! - Mounting virtiofs shares
//! - Spawning the capsa-sandbox-agent
//! - Spawning the main process (if specified via capsa.run=)
//! - Signal handling and zombie reaping

use nix::mount::{MsFlags, mount};
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, dup2, execv, fork};
use std::ffi::CString;
use std::fs::{self, File};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;

mod cmdline;

use cmdline::{CapsaConfig, parse_cmdline};

fn main() {
    if let Err(e) = run() {
        eprintln!("init error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Mount basic filesystems first so /dev/console exists
    setup_basic_filesystems()?;

    // Redirect stdout/stderr to console so we can see output
    setup_console()?;

    println!("capsa-sandbox-init starting...");

    let config = parse_cmdline()?;
    println!("config: {:?}", config);

    mount_shares(&config)?;

    let agent_pid = spawn_agent()?;
    println!("agent spawned with pid {}", agent_pid);

    let main_pid = if let Some(ref main) = config.main_process {
        let pid = spawn_main_process(main)?;
        println!("main process spawned with pid {}", pid);
        Some(pid)
    } else {
        None
    };

    event_loop(agent_pid, main_pid)?;

    Ok(())
}

fn mount_and_create(
    source: &str,
    target: &str,
    fstype: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(target)?;
    mount::<str, str, str, str>(Some(source), target, Some(fstype), MsFlags::empty(), None)?;
    Ok(())
}

fn setup_basic_filesystems() -> Result<(), Box<dyn std::error::Error>> {
    mount_and_create("proc", "/proc", "proc")?;
    mount_and_create("sysfs", "/sys", "sysfs")?;
    mount_and_create("devtmpfs", "/dev", "devtmpfs")?;
    mount_and_create("tmpfs", "/tmp", "tmpfs")?;
    Ok(())
}

fn setup_console() -> Result<(), Box<dyn std::error::Error>> {
    let console = File::options().read(true).write(true).open("/dev/hvc0")?;

    let fd = console.as_raw_fd();
    dup2(fd, 0)?;
    dup2(fd, 1)?;
    dup2(fd, 2)?;

    Ok(())
}

fn mount_shares(config: &CapsaConfig) -> Result<(), Box<dyn std::error::Error>> {
    for share in &config.mounts {
        println!("mounting {} at {}", share.tag, share.path);
        fs::create_dir_all(&share.path)?;

        mount::<str, str, str, str>(
            Some(&share.tag),
            &share.path,
            Some("virtiofs"),
            MsFlags::empty(),
            None,
        )
        .map_err(|e| format!("failed to mount {} at {}: {}", share.tag, share.path, e))?;
    }

    Ok(())
}

fn spawn_agent() -> Result<Pid, Box<dyn std::error::Error>> {
    match unsafe { fork()? } {
        ForkResult::Parent { child } => Ok(child),
        ForkResult::Child => {
            let path = CString::new("/capsa-sandbox-agent")?;
            let args: [CString; 1] = [path.clone()];
            execv(&path, &args)?;
            unreachable!()
        }
    }
}

fn spawn_main_process(
    main: &cmdline::MainProcessConfig,
) -> Result<Pid, Box<dyn std::error::Error>> {
    match unsafe { fork()? } {
        ForkResult::Parent { child } => Ok(child),
        ForkResult::Child => {
            let mut cmd = Command::new(&main.path);
            cmd.args(&main.args);

            let err = cmd.exec();
            eprintln!("exec failed: {}", err);
            std::process::exit(1);
        }
    }
}

fn event_loop(agent_pid: Pid, main_pid: Option<Pid>) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::empty())) {
            Ok(WaitStatus::Exited(pid, status)) => {
                println!("process {} exited with status {}", pid, status);

                if Some(pid) == main_pid {
                    println!("main process exited, shutting down");
                    let _ = signal::kill(agent_pid, Signal::SIGTERM);
                    std::process::exit(status);
                }

                if pid == agent_pid {
                    println!("agent exited, shutting down");
                    if let Some(main) = main_pid {
                        let _ = signal::kill(main, Signal::SIGTERM);
                    }
                    std::process::exit(status);
                }
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                println!("process {} killed by signal {:?}", pid, sig);

                if Some(pid) == main_pid {
                    println!("main process killed, shutting down");
                    let _ = signal::kill(agent_pid, Signal::SIGTERM);
                    std::process::exit(128 + sig as i32);
                }
            }
            Ok(_) => {}
            Err(nix::errno::Errno::ECHILD) => {
                println!("no more children, exiting");
                break;
            }
            Err(e) => {
                eprintln!("waitpid error: {}", e);
            }
        }
    }

    Ok(())
}
