use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::{configure_fd_remaps, FdRemap, SandboxSpec, SandboxedChild};

/// Linux sandbox backend via `syd`.
///
/// This backend is fail-closed by default.
pub fn spawn_with_syd(
    program: &Path,
    args: &[String],
    spec: &SandboxSpec,
    fd_remaps: &[FdRemap],
) -> Result<SandboxedChild> {
    let syd = find_in_path("syd").ok_or_else(|| {
        anyhow::anyhow!(
            "Linux sandbox requires `syd` on PATH. Install it (e.g. via `nix develop`) or set CAPSA_DISABLE_SANDBOX=1 to disable sandboxing"
        )
    })?;

    spawn_with_syd_binary(&syd, program, args, spec, fd_remaps)
}

fn spawn_with_syd_binary(
    syd: &Path,
    program: &Path,
    args: &[String],
    spec: &SandboxSpec,
    fd_remaps: &[FdRemap],
) -> Result<SandboxedChild> {
    let private_tmp = create_private_tmp_dir()?;
    let mut command = Command::new(syd);

    for rule in syd_rules(program, spec, &private_tmp) {
        command.arg("-m").arg(rule);
    }

    command
        .env("TMPDIR", &private_tmp)
        .env("TMP", &private_tmp)
        .env("TEMP", &private_tmp)
        .arg("--")
        .arg(program)
        .args(args);

    configure_fd_remaps(&mut command, fd_remaps);

    let child = command.spawn().with_context(|| {
        format!(
            "failed to spawn `syd` ({}) for program {}",
            syd.display(),
            program.display()
        )
    })?;

    Ok(SandboxedChild::new(child, vec![private_tmp]))
}

fn syd_rules(program: &Path, spec: &SandboxSpec, private_tmp: &Path) -> Vec<String> {
    // Keep policy focused on path-based controls plus fs/ioctl allowlists.
    let mut rules = vec![
        "sandbox/read,stat:on".to_string(),
        "sandbox/exec:on".to_string(),
        "sandbox/fs:on".to_string(),
        "sandbox/ioctl:on".to_string(),
        "sandbox/write,create,truncate,delete:off".to_string(),
        "default/read:deny".to_string(),
        "default/stat:deny".to_string(),
        "default/exec:deny".to_string(),
        "default/write:deny".to_string(),
        "default/create:deny".to_string(),
        "default/truncate:deny".to_string(),
        "default/delete:deny".to_string(),
        "default/ioctl:deny".to_string(),
        "trace/deny_dotdot:on".to_string(),
        "trace/force_cloexec:on".to_string(),
    ];

    if !spec.allow_network {
        rules.push("sandbox/net:on".to_string());
        rules.push("default/net:deny".to_string());
    }

    rules.push("sandbox/lock:on".to_string());

    // Allow common filesystem types touched by capsa-vmm and host runtime.
    for fs in ["ext", "tmpfs", "proc", "sysfs", "cgroup"] {
        rules.push(format!("allow/fs+{fs}"));
    }

    // Allow terminal ioctls and the KVM ioctls used by libkrun's VMM path.
    for ioctl in [
        "TCGETS",
        "TCGETS2",
        "TCSETS",
        "TCSETS2",
        "TCSETSW",
        "TCSETSF",
        "TIOCGWINSZ",
        "TIOCSWINSZ",
        "FIONREAD",
        "KVM_GET_API_VERSION",
        "KVM_CHECK_EXTENSION",
        "KVM_GET_VCPU_MMAP_SIZE",
        "KVM_CREATE_VM",
        "KVM_CREATE_VCPU",
        "KVM_SET_TSS_ADDR",
        "KVM_CREATE_IRQCHIP",
        "KVM_CREATE_PIT2",
        "KVM_IOEVENTFD",
        "KVM_IRQFD",
        "KVM_SET_USER_MEMORY_REGION",
        "KVM_SET_CPUID2",
        "KVM_GET_SUPPORTED_CPUID",
        "KVM_GET_MSR_INDEX_LIST",
        "KVM_SET_MSRS",
        "KVM_GET_MSRS",
        "KVM_SET_REGS",
        "KVM_GET_REGS",
        "KVM_SET_SREGS",
        "KVM_GET_SREGS",
        "KVM_SET_FPU",
        "KVM_GET_FPU",
        "KVM_GET_LAPIC",
        "KVM_SET_LAPIC",
        "KVM_SET_SIGNAL_MASK",
        "KVM_SET_GSI_ROUTING",
        "KVM_SET_CLOCK",
        "KVM_RUN",
    ] {
        rules.push(format!("allow/ioctl+{ioctl}"));
    }

    let mut read_paths = Vec::new();
    let mut read_recursive_paths = Vec::new();
    let mut exec_paths = Vec::new();

    for candidate in path_candidates(program) {
        push_with_ancestors(&mut read_paths, &candidate);
        push_unique(&mut exec_paths, candidate);
    }

    for path in &spec.read_only_paths {
        for candidate in path_candidates(path) {
            push_with_ancestors(&mut read_paths, &candidate);
            if candidate.is_dir() {
                push_unique(&mut read_recursive_paths, candidate);
            }
        }
    }

    for path in &spec.read_write_paths {
        for candidate in path_candidates(path) {
            push_with_ancestors(&mut read_paths, &candidate);
            if candidate.is_dir() {
                push_unique(&mut read_recursive_paths, candidate);
            }
        }
    }

    for path in &spec.ioctl_paths {
        for candidate in path_candidates(path) {
            push_with_ancestors(&mut read_paths, &candidate);
        }
    }

    for dylib in linked_dylibs(program) {
        for candidate in path_candidates(&dylib) {
            push_with_ancestors(&mut read_paths, &candidate);
            push_unique(&mut exec_paths, candidate);
        }
    }

    for path in [
        PathBuf::from("/proc/self/maps"),
        PathBuf::from("/etc/ld.so.cache"),
        PathBuf::from("/etc/ld.so.preload"),
    ] {
        for candidate in path_candidates(&path) {
            push_with_ancestors(&mut read_paths, &candidate);
        }
    }

    for path in stdio_tty_paths() {
        for candidate in path_candidates(&path) {
            push_with_ancestors(&mut read_paths, &candidate);
        }
    }

    for candidate in path_candidates(Path::new("/dev/kvm")) {
        push_with_ancestors(&mut read_paths, &candidate);
    }

    for candidate in path_candidates(private_tmp) {
        push_with_ancestors(&mut read_paths, &candidate);
        if candidate.is_dir() {
            push_unique(&mut read_recursive_paths, candidate);
        }
    }

    for path in read_paths {
        add_allow_rule(&mut rules, "allow/read,stat", &path);
        add_lock_allow_rule(&mut rules, "allow/lock/read", &path);
    }

    for path in read_recursive_paths {
        add_allow_recursive_rule(&mut rules, "allow/read,stat", &path);
    }

    for path in exec_paths {
        add_allow_rule(&mut rules, "allow/exec", &path);
        add_lock_allow_rule(&mut rules, "allow/lock/exec", &path);
    }

    let mut lock_ioctl_paths = vec![PathBuf::from("/dev/kvm")];
    lock_ioctl_paths.extend(spec.ioctl_paths.iter().cloned());

    for path in lock_ioctl_paths {
        for candidate in path_candidates(&path) {
            add_lock_allow_rule(&mut rules, "allow/lock/ioctl", &candidate);
        }
    }

    rules.push("sandbox/write,create,truncate,delete:on".to_string());

    let mut write_paths = vec![PathBuf::from("/dev/kvm"), private_tmp.to_path_buf()];
    write_paths.extend(spec.read_write_paths.iter().cloned());

    for path in write_paths {
        for candidate in path_candidates(&path) {
            if candidate.is_dir() {
                add_allow_recursive_rule(
                    &mut rules,
                    "allow/write,create,truncate,delete",
                    &candidate,
                );
                add_lock_allow_rule(&mut rules, "allow/lock/write,create", &candidate);
            } else {
                add_allow_rule(&mut rules, "allow/write,create,truncate,delete", &candidate);
                add_lock_allow_rule(&mut rules, "allow/lock/write", &candidate);
            }
        }
    }

    rules
}

fn add_allow_rule(rules: &mut Vec<String>, prefix: &str, path: &Path) {
    let escaped = escape_syd_path(path);
    rules.push(format!("{prefix}+{escaped}"));
}

fn add_allow_recursive_rule(rules: &mut Vec<String>, prefix: &str, path: &Path) {
    add_allow_rule(rules, prefix, path);

    if path.is_dir() {
        let escaped = escape_syd_path(path);
        rules.push(format!("{prefix}+{escaped}/***"));
    }
}

fn add_lock_allow_rule(rules: &mut Vec<String>, prefix: &str, path: &Path) {
    if !path.exists() {
        return;
    }

    let escaped = escape_syd_path(path);
    rules.push(format!("{prefix}+{escaped}"));
}

fn linked_dylibs(binary: &Path) -> Vec<PathBuf> {
    let output = match Command::new("ldd").arg(binary).output() {
        Ok(out) if out.status.success() => out,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut libs = Vec::new();

    for line in stdout.lines() {
        let trimmed = line.trim();

        if let Some((_, right)) = trimmed.split_once("=>") {
            if let Some(path) = right
                .split_whitespace()
                .next()
                .filter(|p| p.starts_with('/'))
            {
                push_unique(&mut libs, PathBuf::from(path));
            }
            continue;
        }

        if let Some(path) = trimmed
            .split_whitespace()
            .next()
            .filter(|p| p.starts_with('/'))
        {
            push_unique(&mut libs, PathBuf::from(path));
        }
    }

    libs
}

fn stdio_tty_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();

    for fd in [0, 1, 2] {
        let fd_path = PathBuf::from(format!("/proc/self/fd/{fd}"));
        if let Ok(target) = std::fs::canonicalize(&fd_path) {
            if target.starts_with("/dev/") {
                push_unique(&mut out, target);
            }
        }
    }

    out
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

fn push_with_ancestors(paths: &mut Vec<PathBuf>, path: &Path) {
    for ancestor in path.ancestors() {
        if ancestor == Path::new("/") {
            break;
        }
        push_unique(paths, ancestor.to_path_buf());
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|p| p == &path) {
        paths.push(path);
    }
}

fn escape_syd_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace(',', "\\,")
}

fn create_private_tmp_dir() -> Result<PathBuf> {
    let base = std::env::temp_dir().join("capsa-sandbox");
    std::fs::create_dir_all(&base)
        .with_context(|| format!("failed to create sandbox temp base {}", base.display()))?;

    let dir = tempfile::Builder::new()
        .prefix("linux-")
        .tempdir_in(&base)
        .with_context(|| format!("failed to create private temp dir in {}", base.display()))?;

    Ok(dir.keep())
}

fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
