mod common;

use common::{run_probe, TestDir};

#[test]
fn probe_can_run_under_sandbox_with_basic_read_allow() {
    let temp = TestDir::new("probe-harness");
    let file = temp.join("allowed.txt");
    std::fs::write(&file, b"ok").expect("failed to write fixture file");

    let mut spec = capsa_sandbox::SandboxSpec::new();
    spec.read_only_paths.push(file.clone());

    assert!(run_probe(&spec, &["can-read", &file.display().to_string()]));
}
