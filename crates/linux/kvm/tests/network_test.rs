//! Network integration tests for the Linux KVM backend.
//!
//! These tests verify that UserNat networking works correctly with the KVM hypervisor.

use capsa_core::{
    BootMethod, ConsoleIo, HypervisorBackend, NetworkMode, ResourceConfig, UserNatConfig, VmConfig,
    VsockConfig,
};
use capsa_linux_kvm::KvmBackend;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn create_network_config(kernel: PathBuf, initrd: PathBuf) -> VmConfig {
    let backend = KvmBackend::new();
    let cmdline = backend.kernel_cmdline_defaults().build();

    VmConfig {
        boot: BootMethod::LinuxDirect {
            kernel,
            initrd,
            cmdline,
        },
        resources: ResourceConfig {
            cpus: 1,
            memory_mb: 256,
        },
        network: NetworkMode::UserNat(UserNatConfig::default()),
        root_disk: None,
        disks: vec![],
        shares: vec![],
        vsock: VsockConfig::default(),
        console_enabled: true,
        cluster_network_fd: None,
    }
}

async fn wait_for_and_send(
    console: &mut Box<dyn ConsoleIo + Send>,
    wait_for: &str,
    wait_timeout: Duration,
    send_command: Option<&str>,
) -> Result<String, String> {
    let mut output = String::new();
    let start = std::time::Instant::now();
    let mut buf = [0u8; 1024];

    while start.elapsed() < wait_timeout {
        match tokio::time::timeout(Duration::from_millis(100), console.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                output.push_str(&chunk);
                eprint!("{}", chunk);

                if output.contains(wait_for) {
                    // Found the target string, now send command if specified
                    if let Some(cmd) = send_command {
                        let cmd_with_newline = format!("{}\n", cmd);
                        console
                            .write_all(cmd_with_newline.as_bytes())
                            .await
                            .map_err(|e| format!("Failed to write command: {}", e))?;
                    }
                    return Ok(output);
                }
            }
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(e)) => {
                return Err(format!("Read error: {}", e));
            }
        }
    }
    Err(format!(
        "Timeout waiting for '{}'. Output so far:\n{}",
        wait_for, output
    ))
}

#[tokio::test]
async fn test_usernat_dhcp() {
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

    let config = create_network_config(kernel, initrd);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    let mut console = vm
        .console_stream()
        .await
        .expect("Failed to get console stream")
        .expect("Console stream should be present");

    // Wait for DHCP to complete
    let result = wait_for_and_send(
        &mut console,
        "Network configured via DHCP",
        Duration::from_secs(30),
        None,
    )
    .await;
    vm.kill().await.expect("Failed to kill VM");

    result.expect("VM did not configure network via DHCP");
}

#[tokio::test]
async fn test_usernat_ping_gateway() {
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

    let config = create_network_config(kernel, initrd);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    let mut console = vm
        .console_stream()
        .await
        .expect("Failed to get console stream")
        .expect("Console stream should be present");

    // Wait for boot and DHCP, then send ping command
    wait_for_and_send(
        &mut console,
        "Network configured via DHCP",
        Duration::from_secs(30),
        Some("ping -c 1 10.0.2.2 && echo PING_SUCCESS"),
    )
    .await
    .expect("VM did not configure network via DHCP");

    // Wait for ping result
    let result =
        wait_for_and_send(&mut console, "PING_SUCCESS", Duration::from_secs(10), None).await;
    vm.kill().await.expect("Failed to kill VM");

    result.expect("Ping to gateway failed");
}

#[tokio::test]
async fn test_usernat_dns_lookup() {
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

    let config = create_network_config(kernel, initrd);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    let mut console = vm
        .console_stream()
        .await
        .expect("Failed to get console stream")
        .expect("Console stream should be present");

    // Wait for boot and DHCP, then send DNS lookup command
    wait_for_and_send(
        &mut console,
        "Network configured via DHCP",
        Duration::from_secs(30),
        Some("nslookup example.com 8.8.8.8 && echo DNS_SUCCESS"),
    )
    .await
    .expect("VM did not configure network via DHCP");

    // Wait for DNS result
    let result =
        wait_for_and_send(&mut console, "DNS_SUCCESS", Duration::from_secs(15), None).await;
    vm.kill().await.expect("Failed to kill VM");

    result.expect("DNS lookup failed - UDP NAT may not be working");
}

#[tokio::test]
async fn test_usernat_http_fetch() {
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

    let config = create_network_config(kernel, initrd);
    let vm = backend.start(&config).await.expect("Failed to start VM");

    let mut console = vm
        .console_stream()
        .await
        .expect("Failed to get console stream")
        .expect("Console stream should be present");

    // Wait for boot and DHCP, then send HTTP fetch command
    wait_for_and_send(
        &mut console,
        "Network configured via DHCP",
        Duration::from_secs(30),
        Some("wget -q -O - http://example.com 2>/dev/null | grep -o 'Example Domain' && echo HTTP_SUCCESS"),
    )
    .await
    .expect("VM did not configure network via DHCP");

    // Wait for HTTP result
    let result =
        wait_for_and_send(&mut console, "HTTP_SUCCESS", Duration::from_secs(20), None).await;
    vm.kill().await.expect("Failed to kill VM");

    result.expect("HTTP fetch failed - TCP NAT may not be working");
}
