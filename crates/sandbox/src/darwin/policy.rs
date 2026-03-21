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
