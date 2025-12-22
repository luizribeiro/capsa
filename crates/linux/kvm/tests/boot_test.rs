use capsa_core::{
    BootMethod, Error, HypervisorBackend, NetworkMode, ResourceConfig, VmConfig, VsockConfig,
};
use capsa_linux_kvm::KvmBackend;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncReadExt;

fn create_config(kernel: PathBuf, initrd: PathBuf, cpus: u32, console_enabled: bool) -> VmConfig {
    let backend = KvmBackend::new();
    let cmdline = backend.kernel_cmdline_defaults().build();

    VmConfig {
        boot: BootMethod::LinuxDirect {
            kernel,
            initrd,
            cmdline,
        },
        resources: ResourceConfig {
            cpus,
            memory_mb: 256,
        },
        network: NetworkMode::None,
        root_disk: None,
        disks: vec![],
        shares: vec![],
        vsock: VsockConfig::default(),
        console_enabled,
    }
}

fn test_vm_paths() -> (PathBuf, PathBuf) {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("result-vms");

    let kernel = base.join("default/kernel");
    let initrd = base.join("default/initrd");
    (kernel, initrd)
}

async fn wait_for_boot(vm: &dyn capsa_core::BackendVmHandle) -> String {
    let mut stream = vm
        .console_stream()
        .await
        .expect("Failed to get console stream")
        .expect("Console stream should be present");

    let mut output = String::new();
    let start = std::time::Instant::now();
    let mut buf = [0u8; 1024];

    while start.elapsed() < Duration::from_secs(30) {
        match tokio::time::timeout(Duration::from_millis(100), stream.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                output.push_str(&chunk);
                eprint!("{}", chunk);

                if output.contains("Boot successful") {
                    eprintln!("\n\nBoot successful detected!");
                    return output;
                }
            }
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(e)) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }
    }
    output
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

    let config = create_config(kernel, initrd, 1, true);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    assert!(vm.is_running().await, "VM should be running");

    let output = wait_for_boot(vm.as_ref()).await;
    vm.kill().await.expect("Failed to kill VM");

    assert!(
        output.contains("Boot successful"),
        "Boot successful not found in output"
    );
}

#[tokio::test]
async fn test_kvm_boot_multi_cpu() {
    let (kernel, initrd) = test_vm_paths();

    if !kernel.exists() {
        eprintln!("Test VMs not built, skipping test");
        return;
    }

    let backend = KvmBackend::new();
    if !backend.is_available() {
        eprintln!("KVM not available, skipping test");
        return;
    }

    let config = create_config(kernel, initrd, 4, true);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    assert!(vm.is_running().await, "VM should be running with 4 CPUs");

    let output = wait_for_boot(vm.as_ref()).await;
    vm.kill().await.expect("Failed to kill VM");

    assert!(
        output.contains("Boot successful"),
        "Boot successful not found with multi-CPU"
    );
}

#[tokio::test]
async fn test_kvm_console_disabled() {
    let (kernel, initrd) = test_vm_paths();

    if !kernel.exists() {
        eprintln!("Test VMs not built, skipping test");
        return;
    }

    let backend = KvmBackend::new();
    if !backend.is_available() {
        eprintln!("KVM not available, skipping test");
        return;
    }

    let config = create_config(kernel, initrd, 1, false);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    let result = vm.console_stream().await;
    assert!(
        matches!(result, Err(Error::ConsoleNotEnabled)),
        "Expected ConsoleNotEnabled error when console is disabled"
    );

    vm.kill().await.expect("Failed to kill VM");
}

#[tokio::test]
async fn test_kvm_uefi_boot_unsupported() {
    let backend = KvmBackend::new();
    if !backend.is_available() {
        eprintln!("KVM not available, skipping test");
        return;
    }

    let config = VmConfig {
        boot: BootMethod::Uefi {
            efi_variable_store: PathBuf::from("/nonexistent/efi_vars"),
            create_variable_store: true,
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

    let result = backend.start(&config).await;
    assert!(
        matches!(result, Err(Error::UnsupportedFeature(_))),
        "Expected UnsupportedFeature error for UEFI boot"
    );
}
