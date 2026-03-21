use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};

use anyhow::{Context, Result};

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "linux")]
mod linux;

/// Cross-platform sandbox configuration.
#[derive(Debug, Clone, Default)]
pub struct SandboxSpec {
    /// Allow outbound/inbound networking from the sandboxed process.
    pub allow_network: bool,
    /// Paths that should be readable from inside the sandbox.
    pub read_only_paths: Vec<PathBuf>,
    /// Paths that should be writable from inside the sandbox.
    pub read_write_paths: Vec<PathBuf>,
    /// Paths that should be allowed to perform ioctl operations.
    ///
    /// Backends may apply this with different precision depending on platform
    /// sandbox capabilities.
    pub ioctl_paths: Vec<PathBuf>,
}

impl SandboxSpec {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
}

pub struct SandboxedChild {
    child: Child,
    cleanup_paths: Option<Vec<PathBuf>>,
}

impl SandboxedChild {
    pub(crate) fn new(child: Child, cleanup_paths: Vec<PathBuf>) -> Self {
        Self {
            child,
            cleanup_paths: Some(cleanup_paths),
        }
    }

    pub fn wait(mut self) -> std::io::Result<ExitStatus> {
        let status = self.child.wait();
        self.cleanup_now();
        status
    }

    fn cleanup_now(&mut self) {
        let Some(paths) = self.cleanup_paths.take() else {
            return;
        };

        for path in paths {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

impl Drop for SandboxedChild {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

/// Spawn `program` with `args` inside the platform sandbox.
///
/// - macOS: seatbelt profile via `sandbox-exec`
/// - Linux: `syd` backend (fail-closed by default)
///
/// Set `CAPSA_DISABLE_SANDBOX=1` (or `true`/`yes`/`on`) to bypass sandboxing.
#[cfg(target_os = "macos")]
pub fn spawn_sandboxed(
    program: &Path,
    args: &[String],
    spec: &SandboxSpec,
) -> Result<SandboxedChild> {
    if sandbox_disabled() {
        eprintln!("warning: sandbox disabled via CAPSA_DISABLE_SANDBOX; running without sandbox");
        return spawn_direct(program, args);
    }

    darwin::spawn_with_sandbox_exec(program, args, spec)
}

#[cfg(target_os = "linux")]
pub fn spawn_sandboxed(
    program: &Path,
    args: &[String],
    spec: &SandboxSpec,
) -> Result<SandboxedChild> {
    if sandbox_disabled() {
        eprintln!("warning: sandbox disabled via CAPSA_DISABLE_SANDBOX; running without sandbox");
        return spawn_direct(program, args);
    }

    linux::spawn_with_syd(program, args, spec)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn spawn_sandboxed(
    _program: &Path,
    _args: &[String],
    _spec: &SandboxSpec,
) -> Result<SandboxedChild> {
    anyhow::bail!("sandboxing is not implemented for this platform")
}

fn sandbox_disabled() -> bool {
    matches!(
        std::env::var("CAPSA_DISABLE_SANDBOX").as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

fn spawn_direct(program: &Path, args: &[String]) -> Result<SandboxedChild> {
    let child = Command::new(program)
        .args(args)
        .spawn()
        .with_context(|| format!("failed to spawn {}", program.display()))?;

    Ok(SandboxedChild::new(child, vec![]))
}
