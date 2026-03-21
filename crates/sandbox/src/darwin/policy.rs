use std::path::Path;

use crate::SandboxSpec;

use super::paths::PathSets;
use super::seatbelt::SeatbeltPolicy;

pub(super) fn build_policy(
    program: &Path,
    spec: &SandboxSpec,
    private_tmp: &Path,
) -> SeatbeltPolicy {
    let paths = PathSets::from_inputs(program, spec, private_tmp);

    let mut policy = SeatbeltPolicy::new();
    policy.import_system();

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

    for path in &paths.executable_paths {
        policy.allow_literal(&["process-exec"], path);
    }

    for path in &paths.read_only_paths {
        if is_directory(path) {
            policy.allow_subpath(&["file-read*"], path);
            policy.allow_subpath(&["file-map-executable"], path);
        } else {
            policy.allow_literal(&["file-read*"], path);
            policy.allow_literal(&["file-map-executable"], path);
        }
    }

    for path in &paths.read_write_paths {
        if is_directory(path) {
            policy.allow_subpath(&["file-read*"], path);
            policy.allow_subpath(&["file-write*"], path);
        } else {
            policy.allow_literal(&["file-read*"], path);
            policy.allow_literal(&["file-write*"], path);
        }
    }

    for path in &paths.ioctl_paths {
        if is_directory(path) {
            policy.allow_subpath(&["file-read*"], path);
            policy.allow_subpath(&["file-ioctl"], path);
        } else {
            policy.allow_literal(&["file-read*"], path);
            policy.allow_literal(&["file-ioctl"], path);
        }
    }

    if spec.allow_network {
        policy.allow(&["network*"]);
    }

    policy
}

fn is_directory(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::SandboxSpec;

    use super::build_policy;

    #[test]
    fn ioctl_is_only_granted_for_ioctl_paths() {
        let base =
            std::env::temp_dir().join(format!("capsa-sandbox-policy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("create test base dir");

        let rw_file = base.join("rw.dat");
        std::fs::write(&rw_file, b"data").expect("create rw file");

        let ioctl_file = base.join("ioctl.dev");
        std::fs::write(&ioctl_file, b"dev").expect("create ioctl file");

        let private_tmp = base.join("tmp");
        std::fs::create_dir_all(&private_tmp).expect("create private tmp");

        let mut spec = SandboxSpec::new();
        spec.read_write_paths.push(rw_file.clone());
        spec.ioctl_paths.push(ioctl_file.clone());

        let policy = build_policy(PathBuf::from("/bin/ls").as_path(), &spec, &private_tmp);
        let rendered: String = policy.into();

        let rw_ioctl_rule = format!("(allow file-ioctl (literal \"{}\"))", rw_file.display());
        assert!(
            !rendered.contains(&rw_ioctl_rule),
            "rw path unexpectedly granted ioctl: {rw_ioctl_rule}"
        );

        let ioctl_rule = format!("(allow file-ioctl (literal \"{}\"))", ioctl_file.display());
        assert!(
            rendered.contains(&ioctl_rule),
            "ioctl path missing ioctl rule: {ioctl_rule}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
