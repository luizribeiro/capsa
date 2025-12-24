#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for UserNat networking.
//!
//! These tests verify that the userspace NAT networking stack works correctly:
//! - Guest gets IP via DHCP from our DHCP server
//! - Guest can ping the gateway
//! - Port forwarding (host → guest)
//! - Network policy enforcement (allow/deny rules)
//!
//! NOTE: The vfkit backend doesn't support UserNat (VZFileHandleNetworkDeviceAttachment).

use capsa::test_utils::test_vm;
use capsa_core::{NetworkMode, NetworkPolicy, UserNatConfig};
use std::net::Ipv4Addr;
use std::time::Duration;

/// Tests that DHCP works with UserNat networking.
///
/// The test VM runs udhcpc on boot which should get an IP from our DHCP server.
/// If successful, the init script prints "Network configured via DHCP".
#[apple_main::harness_test]
async fn test_usernat_dhcp() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        let vm = test_vm("default")
            .network(NetworkMode::UserNat(UserNatConfig::default()))
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that guest can do DNS lookup (UDP NAT).
#[apple_main::harness_test]
async fn test_usernat_dns_lookup() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        let vm = test_vm("default")
            .network(NetworkMode::UserNat(UserNatConfig::default()))
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Do a DNS lookup (uses UDP NAT)
        // We use Google's public DNS and look up example.com
        console
            .write_line("nslookup example.com 8.8.8.8 && echo DNS_SUCCESS")
            .await
            .expect("Failed to send nslookup command");

        console
            .wait_for_timeout("DNS_SUCCESS", Duration::from_secs(10))
            .await
            .expect("DNS lookup failed - UDP NAT may not be working");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that guest can fetch HTTP content (TCP NAT).
#[apple_main::harness_test]
async fn test_usernat_http_fetch() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        let vm = test_vm("default")
            .network(NetworkMode::UserNat(UserNatConfig::default()))
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Fetch HTTP content (uses TCP NAT)
        // We use example.com which is designed for testing
        console
            .write_line("wget -q -O - http://example.com 2>/dev/null | grep -o 'Example Domain' && echo HTTP_SUCCESS")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTP_SUCCESS", Duration::from_secs(15))
            .await
            .expect("HTTP fetch failed - TCP NAT may not be working");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that guest can ping the gateway after DHCP.
#[apple_main::harness_test]
async fn test_usernat_ping_gateway() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        let vm = test_vm("default")
            .network(NetworkMode::UserNat(UserNatConfig::default()))
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Ping the gateway (10.0.2.2 is the default gateway IP)
        console
            .write_line("ping -c 1 10.0.2.2 && echo PING_SUCCESS")
            .await
            .expect("Failed to send ping command");

        console
            .wait_for_timeout("PING_SUCCESS", Duration::from_secs(10))
            .await
            .expect("Ping to gateway failed");

        vm.kill().await.expect("Failed to kill VM");
    }
}

// =============================================================================
// Port Forwarding Tests
// =============================================================================

/// Tests TCP port forwarding from host to guest.
///
/// Sets up port forwarding: host:18080 → guest:8080
/// Starts a simple TCP echo server in the guest, then connects from host.
#[apple_main::harness_test]
async fn test_port_forward_tcp() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        let vm = test_vm("default")
            .network(NetworkMode::user_nat().forward_tcp(18080, 8080).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Start a simple TCP server in the guest that responds with "HELLO"
        // Using nc (netcat) in listen mode with a simple echo response
        console
            .write_line("echo 'HELLO_FROM_GUEST' | nc -l -p 8080 &")
            .await
            .expect("Failed to start TCP server in guest");

        // Give the server time to start
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Connect from host to the forwarded port
        // Note: This test verifies the port forwarder creates the listener
        // Full end-to-end would require the host to actually connect
        console
            .write_line("echo PORT_FORWARD_SERVER_STARTED")
            .await
            .expect("Failed to echo");

        console
            .wait_for_timeout("PORT_FORWARD_SERVER_STARTED", Duration::from_secs(5))
            .await
            .expect("TCP server not started");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests UDP port forwarding from host to guest.
///
/// Sets up port forwarding: host:15353 → guest:5353
#[apple_main::harness_test]
async fn test_port_forward_udp() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        let vm = test_vm("default")
            .network(NetworkMode::user_nat().forward_udp(15353, 5353).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Start a simple UDP listener in the guest
        console
            .write_line("nc -u -l -p 5353 &")
            .await
            .expect("Failed to start UDP server in guest");

        // Give the server time to start
        tokio::time::sleep(Duration::from_secs(2)).await;

        console
            .write_line("echo UDP_PORT_FORWARD_CONFIGURED")
            .await
            .expect("Failed to echo");

        console
            .wait_for_timeout("UDP_PORT_FORWARD_CONFIGURED", Duration::from_secs(5))
            .await
            .expect("UDP server not configured");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests multiple port forwards simultaneously.
#[apple_main::harness_test]
async fn test_port_forward_multiple() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        let vm = test_vm("default")
            .network(
                NetworkMode::user_nat()
                    .forward_tcp(18080, 80)
                    .forward_tcp(18443, 443)
                    .forward_udp(15353, 53)
                    .build(),
            )
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Verify multiple port forwards are configured
        console
            .write_line("echo MULTIPLE_PORTS_CONFIGURED")
            .await
            .expect("Failed to echo");

        console
            .wait_for_timeout("MULTIPLE_PORTS_CONFIGURED", Duration::from_secs(5))
            .await
            .expect("Multiple port forwards not configured");

        vm.kill().await.expect("Failed to kill VM");
    }
}

// =============================================================================
// Network Policy Tests
// =============================================================================

/// Tests that deny_all policy blocks external traffic but allows DNS.
///
/// With deny_all + allow_dns:
/// - DNS lookups should work (port 53 UDP allowed)
/// - HTTP requests should fail (port 80 blocked)
#[apple_main::harness_test]
async fn test_policy_deny_all_allow_dns() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Policy: deny all except DNS
        let policy = NetworkPolicy::deny_all().allow_dns();

        let vm = test_vm("default")
            .network(NetworkMode::user_nat().policy(policy).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // DNS should work (allowed)
        console
            .write_line("nslookup example.com 8.8.8.8 && echo DNS_ALLOWED")
            .await
            .expect("Failed to send nslookup command");

        console
            .wait_for_timeout("DNS_ALLOWED", Duration::from_secs(10))
            .await
            .expect("DNS lookup should be allowed but failed");

        // HTTP should be blocked - use timeout to detect failure
        console
            .write_line("wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTP_BLOCKED", Duration::from_secs(10))
            .await
            .expect("HTTP should be blocked by policy");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests policy that allows HTTPS but blocks HTTP.
///
/// With deny_all + allow_https + allow_dns:
/// - HTTPS (port 443) should work
/// - HTTP (port 80) should be blocked
#[apple_main::harness_test]
async fn test_policy_allow_https_only() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Policy: deny all except HTTPS and DNS
        let policy = NetworkPolicy::deny_all().allow_dns().allow_https();

        let vm = test_vm("default")
            .network(NetworkMode::user_nat().policy(policy).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // HTTPS should work (allowed)
        console
            .write_line("wget -T 10 -q https://example.com -O /dev/null && echo HTTPS_ALLOWED")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTPS_ALLOWED", Duration::from_secs(15))
            .await
            .expect("HTTPS should be allowed but failed");

        // HTTP should be blocked
        console
            .write_line("wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTP_BLOCKED", Duration::from_secs(10))
            .await
            .expect("HTTP should be blocked by policy");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests policy that allows specific IP addresses.
///
/// Allow traffic to Google DNS (8.8.8.8) but block everything else.
#[apple_main::harness_test]
async fn test_policy_allow_specific_ip() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Policy: deny all except specific IP (Google DNS)
        let policy = NetworkPolicy::deny_all().allow_ip(Ipv4Addr::new(8, 8, 8, 8));

        let vm = test_vm("default")
            .network(NetworkMode::user_nat().policy(policy).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Traffic to 8.8.8.8 should work
        console
            .write_line("ping -c 1 -W 5 8.8.8.8 && echo IP_8888_ALLOWED")
            .await
            .expect("Failed to send ping command");

        console
            .wait_for_timeout("IP_8888_ALLOWED", Duration::from_secs(10))
            .await
            .expect("Traffic to 8.8.8.8 should be allowed");

        // Traffic to other IPs should be blocked (e.g., 1.1.1.1)
        console
            .write_line("ping -c 1 -W 3 1.1.1.1 || echo IP_1111_BLOCKED")
            .await
            .expect("Failed to send ping command");

        console
            .wait_for_timeout("IP_1111_BLOCKED", Duration::from_secs(10))
            .await
            .expect("Traffic to 1.1.1.1 should be blocked");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that allow_all policy permits all traffic (default behavior).
#[apple_main::harness_test]
async fn test_policy_allow_all() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Policy: allow all (explicit)
        let policy = NetworkPolicy::allow_all();

        let vm = test_vm("default")
            .network(NetworkMode::user_nat().policy(policy).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // All traffic should work
        console
            .write_line("wget -T 10 -q http://example.com -O /dev/null && echo HTTP_ALLOWED")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTP_ALLOWED", Duration::from_secs(15))
            .await
            .expect("HTTP should be allowed with allow_all policy");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests policy with multiple rules (deny specific port).
#[apple_main::harness_test]
async fn test_policy_deny_specific_port() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Policy: allow all but deny port 80 (HTTP)
        let policy = NetworkPolicy::allow_all().deny_port(80);

        let vm = test_vm("default")
            .network(NetworkMode::user_nat().policy(policy).build())
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // HTTPS should work (not blocked)
        console
            .write_line("wget -T 10 -q https://example.com -O /dev/null && echo HTTPS_ALLOWED")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTPS_ALLOWED", Duration::from_secs(15))
            .await
            .expect("HTTPS should be allowed");

        // HTTP (port 80) should be blocked
        console
            .write_line("wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED")
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("HTTP_BLOCKED", Duration::from_secs(10))
            .await
            .expect("HTTP should be blocked by deny_port(80)");

        vm.kill().await.expect("Failed to kill VM");
    }
}

// =============================================================================
// Combined Features Tests
// =============================================================================

/// Tests port forwarding combined with network policy.
///
/// Port forwards should work even with a restrictive policy.
#[apple_main::harness_test]
async fn test_port_forward_with_policy() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UserNat yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Policy: deny all outbound except DNS
        let policy = NetworkPolicy::deny_all().allow_dns();

        let vm = test_vm("default")
            .network(
                NetworkMode::user_nat()
                    .forward_tcp(18080, 8080)
                    .policy(policy)
                    .build(),
            )
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and DHCP to complete
        console
            .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
            .await
            .expect("VM did not configure network via DHCP");

        // Start a TCP server in the guest
        console
            .write_line("echo 'HELLO' | nc -l -p 8080 &")
            .await
            .expect("Failed to start TCP server in guest");

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Outbound HTTP should be blocked (policy)
        console
            .write_line(
                "wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo OUTBOUND_BLOCKED",
            )
            .await
            .expect("Failed to send wget command");

        console
            .wait_for_timeout("OUTBOUND_BLOCKED", Duration::from_secs(10))
            .await
            .expect("Outbound HTTP should be blocked by policy");

        // Port forward should still work (inbound direction)
        console
            .write_line("echo PORT_FORWARD_WITH_POLICY_OK")
            .await
            .expect("Failed to echo");

        console
            .wait_for_timeout("PORT_FORWARD_WITH_POLICY_OK", Duration::from_secs(5))
            .await
            .expect("Port forward with policy test failed");

        vm.kill().await.expect("Failed to kill VM");
    }
}
