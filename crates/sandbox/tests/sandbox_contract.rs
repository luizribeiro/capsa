mod common;

use common::{run_probe, TestDir};

#[test]
fn read_allowlist_on_single_file_does_not_grant_siblings() {
    let temp = TestDir::new("read-contract");
    let allowed = temp.join("allowed.txt");
    let sibling = temp.join("sibling.txt");

    std::fs::write(&allowed, b"allowed").expect("failed to write allowed fixture");
    std::fs::write(&sibling, b"sibling").expect("failed to write sibling fixture");

    let mut spec = capsa_sandbox::SandboxSpec::new();
    spec.read_only_paths.push(allowed.clone());

    assert!(run_probe(
        &spec,
        &["can-read", &allowed.display().to_string()]
    ));
    assert!(!run_probe(
        &spec,
        &["can-read", &sibling.display().to_string()]
    ));
}

#[test]
fn stat_allowlist_on_single_file_does_not_grant_siblings() {
    let temp = TestDir::new("stat-contract");
    let allowed = temp.join("allowed.txt");
    let sibling = temp.join("sibling.txt");

    std::fs::write(&allowed, b"allowed").expect("failed to write allowed fixture");
    std::fs::write(&sibling, b"sibling").expect("failed to write sibling fixture");

    let mut spec = capsa_sandbox::SandboxSpec::new();
    spec.read_only_paths.push(allowed.clone());

    assert!(run_probe(
        &spec,
        &["can-stat", &allowed.display().to_string()]
    ));
    assert!(!run_probe(
        &spec,
        &["can-stat", &sibling.display().to_string()]
    ));
}

#[test]
fn write_allowlist_is_scoped_to_explicit_rw_paths() {
    let temp = TestDir::new("write-contract");
    let allowed_file = temp.join("ok.txt");
    let denied_file = temp.join("nope.txt");
    std::fs::write(&allowed_file, b"seed").expect("failed to seed allowed file");
    std::fs::write(&denied_file, b"seed").expect("failed to seed denied file");

    let mut spec = capsa_sandbox::SandboxSpec::new();
    spec.read_write_paths.push(allowed_file.clone());

    assert!(run_probe(
        &spec,
        &["can-write", &allowed_file.display().to_string()]
    ));
    assert!(!run_probe(
        &spec,
        &["can-write", &denied_file.display().to_string()]
    ));

    // Cross-platform contract: writes must stay scoped to explicit read_write_paths.
}
