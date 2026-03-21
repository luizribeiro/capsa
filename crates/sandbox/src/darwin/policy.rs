use std::path::{Path, PathBuf};
use std::process::Command;

use crate::SandboxSpec;

use super::seatbelt::SeatbeltPolicy;

pub(super) fn build_policy(
    program: &Path,
    spec: &SandboxSpec,
    private_tmp: &Path,
) -> SeatbeltPolicy {
    let paths = PathSets::from_inputs(program, spec, private_tmp);

    let mut policy = SeatbeltPolicy::new();

    policy.allow(&["process*"]);
    policy.allow(&["pseudo-tty"]);
    policy.allow_literal(
        &["file-read*", "file-write*", "file-ioctl"],
        Path::new("/dev/tty"),
    );
    policy.allow_regex(
        &["file-read*", "file-write*", "file-ioctl"],
        "^/dev/ttys[0-9]*",
    );

    for path in &paths.traversal_paths {
        policy.allow_literal(&["file-read-metadata"], path);
    }

    for path in &paths.read_only_paths {
        policy.allow_literal(&["file-read*"], path);
        policy.allow_subpath(&["file-read*"], path);
        policy.allow_literal(&["process-exec"], path);
        policy.allow_subpath(&["process-exec"], path);
        policy.allow_literal(&["file-map-executable"], path);
        policy.allow_subpath(&["file-map-executable"], path);
    }

    for path in &paths.read_write_paths {
        policy.allow_literal(&["file-read*"], path);
        policy.allow_subpath(&["file-read*"], path);
        policy.allow_literal(&["file-write*"], path);
        policy.allow_subpath(&["file-write*"], path);
        policy.allow_literal(&["file-ioctl"], path);
        policy.allow_subpath(&["file-ioctl"], path);
        policy.allow_literal(&["process-exec"], path);
        policy.allow_subpath(&["process-exec"], path);
        policy.allow_literal(&["file-map-executable"], path);
        policy.allow_subpath(&["file-map-executable"], path);
    }

    if spec.allow_network {
        policy.allow(&["network*"]);
    }

    policy
}

#[derive(Debug, Default)]
struct PathSets {
    read_only_paths: Vec<PathBuf>,
    read_write_paths: Vec<PathBuf>,
    traversal_paths: Vec<PathBuf>,
}

impl PathSets {
    fn from_inputs(program: &Path, spec: &SandboxSpec, private_tmp: &Path) -> Self {
        let mut paths = Self::default();

        paths.add_read_only(program);

        for path in &spec.read_only_paths {
            paths.add_read_only(path);
        }

        for path in &spec.read_write_paths {
            paths.add_read_write(path);
        }

        // Baseline runtime dependencies on macOS.
        paths.add_read_only(Path::new("/usr/lib"));
        paths.add_read_only(Path::new("/System"));

        for dylib in linked_dylibs_recursive(program) {
            paths.add_read_only(&dylib);
        }

        // Interactive terminal support for libkrun console handling.
        for tty in stdio_tty_paths() {
            paths.add_read_write(&tty);
        }
        paths.add_read_write(Path::new("/dev/tty"));

        paths.add_read_write(private_tmp);

        paths
    }

    fn add_read_only(&mut self, path: &Path) {
        for candidate in Self::path_candidates(path) {
            Self::push_unique(&mut self.read_only_paths, candidate.clone());
            self.add_traversal_ancestors(&candidate);
        }
    }

    fn add_read_write(&mut self, path: &Path) {
        for candidate in Self::path_candidates(path) {
            Self::push_unique(&mut self.read_write_paths, candidate.clone());
            self.add_traversal_ancestors(&candidate);
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

        Self::push_unique(&mut out, absolute.clone());

        if let Ok(canonical) = std::fs::canonicalize(&absolute) {
            Self::push_unique(&mut out, canonical);
        }

        out
    }

    fn add_traversal_ancestors(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            for ancestor in parent.ancestors() {
                Self::push_unique(&mut self.traversal_paths, ancestor.to_path_buf());
            }
        }
    }

    fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
        if !paths.iter().any(|p| p == &path) {
            paths.push(path);
        }
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
                PathSets::push_unique(&mut out, target);
            }
        }
    }

    out
}
