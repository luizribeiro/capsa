use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_nanos();

    let private_tmp = base.join(format!("tmp-{}-{}", std::process::id(), ts));
    fs::create_dir_all(&private_tmp).with_context(|| {
        format!(
            "failed to create private sandbox temp dir {}",
            private_tmp.display()
        )
    })?;

    Ok(private_tmp)
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
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_nanos();

    let mut path = std::env::temp_dir();
    path.push(format!("capsa-seatbelt-{}-{}.sb", std::process::id(), ts));

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .with_context(|| {
            format!(
                "failed to create temporary seatbelt profile at {}",
                path.display()
            )
        })?;

    file.write_all(profile.as_bytes())
        .with_context(|| format!("failed writing seatbelt profile to {}", path.display()))?;

    Ok(path)
}
