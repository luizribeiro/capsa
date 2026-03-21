use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::{SandboxSpec, SandboxedChild};

mod seatbelt;

use seatbelt::SeatbeltPolicy;

pub fn spawn_with_sandbox_exec(
    program: &Path,
    args: &[String],
    spec: &SandboxSpec,
) -> Result<SandboxedChild> {
    let mut effective = expand_spec_for_darwin(program, spec);
    let private_tmp = create_private_tmp_dir()?;
    add_read_write(&mut effective, &private_tmp);

    let policy = SeatbeltPolicy::from_parts(
        effective.allow_network,
        &effective.traversal_paths,
        &effective.read_only_paths,
        &effective.read_write_paths,
    );
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

#[derive(Debug, Clone, Default)]
struct ExpandedSpec {
    allow_network: bool,
    /// Explicitly allowlisted read-only targets.
    read_only_paths: Vec<PathBuf>,
    /// Explicitly allowlisted read-write targets.
    read_write_paths: Vec<PathBuf>,
    /// Ancestors needed only for path traversal/lookup.
    traversal_paths: Vec<PathBuf>,
}

fn expand_spec_for_darwin(program: &Path, spec: &SandboxSpec) -> ExpandedSpec {
    let mut out = ExpandedSpec {
        allow_network: spec.allow_network,
        ..ExpandedSpec::default()
    };

    add_read_only(&mut out, program);

    for path in &spec.read_only_paths {
        add_read_only(&mut out, path);
    }

    for path in &spec.read_write_paths {
        add_read_write(&mut out, path);
    }

    // Baseline runtime dependencies on macOS.
    add_read_only(&mut out, Path::new("/usr/lib"));
    add_read_only(&mut out, Path::new("/System"));

    for dylib in linked_dylibs_recursive(program) {
        add_read_only(&mut out, &dylib);
    }

    // Interactive terminal support for libkrun console handling.
    for tty in stdio_tty_paths() {
        add_read_write(&mut out, &tty);
    }
    add_read_write(&mut out, Path::new("/dev/tty"));

    out
}

fn add_read_only(spec: &mut ExpandedSpec, path: &Path) {
    for candidate in path_candidates(path) {
        push_unique(&mut spec.read_only_paths, candidate.clone());
        add_traversal_ancestors(spec, &candidate);
    }
}

fn add_read_write(spec: &mut ExpandedSpec, path: &Path) {
    for candidate in path_candidates(path) {
        push_unique(&mut spec.read_write_paths, candidate.clone());
        add_traversal_ancestors(spec, &candidate);
    }
}

fn path_candidates(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };

    push_unique(&mut out, absolute.clone());

    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        push_unique(&mut out, canonical);
    }

    out
}

fn add_traversal_ancestors(spec: &mut ExpandedSpec, path: &Path) {
    if let Some(parent) = path.parent() {
        for ancestor in parent.ancestors() {
            push_unique(&mut spec.traversal_paths, ancestor.to_path_buf());
        }
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|p| p == &path) {
        paths.push(path);
    }
}

fn linked_dylibs_recursive(exe: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    let mut visited = std::collections::HashSet::new();

    queue.push_back(exe.to_path_buf());
    visited.insert(exe.to_path_buf());

    while let Some(binary) = queue.pop_front() {
        for dep in direct_dylibs(&binary) {
            if dep == binary {
                continue;
            }
            if visited.insert(dep.clone()) {
                discovered.push(dep.clone());
                queue.push_back(dep);
            }
        }
    }

    discovered
}

fn direct_dylibs(binary: &Path) -> Vec<PathBuf> {
    let output = match Command::new("otool").arg("-L").arg(binary).output() {
        Ok(out) if out.status.success() => out,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .filter(|path| path.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

fn stdio_tty_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    for fd in [0, 1, 2] {
        let fd_path = PathBuf::from(format!("/dev/fd/{fd}"));
        if let Ok(target) = std::fs::canonicalize(&fd_path) {
            if target.starts_with("/dev/") {
                push_unique(&mut out, target);
            }
        }
    }

    out
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
