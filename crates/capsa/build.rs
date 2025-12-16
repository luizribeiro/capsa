use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../test-vms.nix");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let project_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let test_vms_nix = project_root.join("test-vms.nix");
    let result_link = project_root.join("result-vms");

    if !test_vms_nix.exists() {
        return;
    }

    // Nix handles caching - if nothing changed, this returns instantly
    let status = Command::new("nix-build")
        .arg(&test_vms_nix)
        .arg("-o")
        .arg(&result_link)
        .current_dir(project_root)
        .status();

    if let Err(e) = status {
        eprintln!("Failed to run nix-build: {}", e);
    }
}
