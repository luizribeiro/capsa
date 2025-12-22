use capsa_linux_kvm::KvmBackend;
use capsa_core::{HypervisorBackend, BootMethod, VmConfig, ResourceConfig, NetworkMode, VsockConfig};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;

fn test_vm_paths() -> (PathBuf, PathBuf) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .parent().unwrap()
        .join("result-vms");

    let kernel = base.join("default/kernel");
    let initrd = base.join("default/initrd");
    (kernel, initrd)
}

#[tokio::test]
async fn test_kvm_boot_with_console() {
    let (kernel, initrd) = test_vm_paths();

    if !kernel.exists() {
        eprintln!("Test VMs not built, skipping test");
        return;
    }

    let backend = KvmBackend::new();
    assert!(backend.is_available(), "KVM not available");

    let cmdline = backend.kernel_cmdline_defaults().build();

    let config = VmConfig {
        boot: BootMethod::LinuxDirect {
            kernel,
            initrd,
            cmdline,
        },
        resources: ResourceConfig {
            cpus: 1,
            memory_mb: 256,
        },
        network: NetworkMode::None,
        root_disk: None,
        disks: vec![],
        shares: vec![],
        vsock: VsockConfig::default(),
        console_enabled: true,
    };

    eprintln!("Starting VM...");
    let vm = backend.start(&config).await.expect("Failed to start VM");
    eprintln!("VM started, checking if running...");
    assert!(vm.is_running().await, "VM should be running");
    eprintln!("VM is running, getting console stream...");

    let stream_result = vm.console_stream().await;
    eprintln!("console_stream result: {:?}", stream_result.as_ref().map(|o| o.is_some()));
    let mut stream = stream_result
        .expect("Failed to get console stream")
        .expect("Console stream should be present");
    eprintln!("Got console stream");

    let mut output = String::new();
    let start = std::time::Instant::now();
    let mut buf = [0u8; 1024];

    // Read output for up to 30 seconds or until we see "Boot successful"
    while start.elapsed() < Duration::from_secs(30) {
        match tokio::time::timeout(Duration::from_millis(100), stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                output.push_str(&chunk);
                eprint!("{}", chunk);

                // Look for boot success message from init script
                if output.contains("Boot successful") {
                    eprintln!("\n\nBoot successful detected!");
                    vm.kill().await.expect("Failed to kill VM");
                    return;
                }
            }
            Ok(Ok(_)) => {
                // EOF or no data
            }
            Ok(Err(e)) => {
                eprintln!("Read error: {}", e);
                break;
            }
            Err(_) => {
                // Timeout, continue reading
            }
        }
    }

    eprintln!("\n\nTotal output ({} bytes):\n{}", output.len(), output);
    vm.kill().await.expect("Failed to kill VM");
    assert!(output.contains("Boot successful"), "Boot successful not found in output");
}
