fn main() {
    #[cfg(all(target_os = "macos", feature = "macos-subprocess"))]
    {
        use std::path::PathBuf;

        if let Ok(vzd_bin) = std::env::var("CARGO_BIN_FILE_CAPSA_APPLE_VZD_capsa-apple-vzd") {
            let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
            let dest = out_dir.join("capsa-apple-vzd");

            std::fs::copy(&vzd_bin, &dest).unwrap_or_else(|e| {
                panic!(
                    "Failed to copy vzd binary from '{}' to '{}': {}",
                    vzd_bin,
                    dest.display(),
                    e
                )
            });

            let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
            let workspace_root = manifest_dir
                .parent()
                .and_then(|p| p.parent())
                .unwrap_or_else(|| {
                    panic!(
                        "Failed to find workspace root from manifest dir: {}",
                        manifest_dir.display()
                    )
                });
            let entitlements = workspace_root.join("entitlements.plist");

            if !entitlements.exists() {
                panic!(
                    "entitlements.plist not found at '{}'. This file is required for codesigning.",
                    entitlements.display()
                );
            }

            let status = std::process::Command::new("codesign")
                .args([
                    "-s",
                    "-",
                    "--entitlements",
                    entitlements.to_str().unwrap(),
                    "--force",
                    dest.to_str().unwrap(),
                ])
                .status()
                .unwrap_or_else(|e| {
                    panic!(
                        "Failed to execute codesign command: {}. Is Xcode command line tools installed?",
                        e
                    )
                });

            if !status.success() {
                panic!(
                    "codesign failed with status {} while signing '{}' with entitlements '{}'",
                    status,
                    dest.display(),
                    entitlements.display()
                );
            }

            println!("cargo:rustc-env=CAPSA_VZD_BUNDLED={}", dest.display());
        }
    }
}
