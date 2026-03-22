use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::{SandboxSpec, SandboxedChild};

mod paths;
mod policy;
mod seatbelt;

use policy::build_policy;

pub fn spawn_with_sandbox_exec(
    program: &Path,
    args: &[String],
    spec: &SandboxSpec,
) -> Result<SandboxedChild> {
    let private_tmp = create_private_tmp_dir()?;

    let policy = build_policy(program, spec, &private_tmp);
    let profile: String = policy.into();
    let profile_path = write_temp_profile(&profile)?;

    let child = Command::new("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(&profile_path)
        .arg(program)
        .args(args)
        .env("TMPDIR", &private_tmp)
        .env("TMP", &private_tmp)
        .env("TEMP", &private_tmp)
        .spawn()
        .with_context(|| {
            format!(
                "failed to spawn sandbox-exec for program {} (profile: {})",
                program.display(),
                profile_path.display()
            )
        })?;

    Ok(SandboxedChild::new(child, vec![profile_path, private_tmp]))
}

fn create_private_tmp_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join("capsa-sandbox");
    fs::create_dir_all(&base)
        .with_context(|| format!("failed to create sandbox temp base {}", base.display()))?;

    cleanup_stale_private_tmp_dirs(&base);

    let dir = tempfile::Builder::new()
        .prefix("tmp-")
        .tempdir_in(&base)
        .with_context(|| format!("failed to create private temp dir in {}", base.display()))?;

    Ok(dir.keep())
}

fn cleanup_stale_private_tmp_dirs(base: &Path) {
    let now = SystemTime::now();
    let max_age = std::time::Duration::from_secs(24 * 60 * 60);

    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }

        let modified = match meta.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };

        if now.duration_since(modified).unwrap_or_default() > max_age {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn write_temp_profile(profile: &str) -> Result<PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix("capsa-seatbelt-")
        .suffix(".sb")
        .tempfile_in(std::env::temp_dir())
        .context("failed to create temporary seatbelt profile")?;

    file.write_all(profile.as_bytes())
        .context("failed writing seatbelt profile")?;

    let (_persisted_file, path) = file
        .keep()
        .map_err(|err| err.error)
        .context("failed to persist temporary seatbelt profile")?;

    Ok(path)
}
