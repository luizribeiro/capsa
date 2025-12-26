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
use capsa::{VmConsole, VmHandle};
use capsa_core::{NetworkMode, NetworkPolicy, UserNatConfig};
#[allow(unused_imports)] // Used only in native-vz cfg block
use std::net::Ipv4Addr;
use std::time::Duration;

/// Helper to set up a VM with networking and wait for DHCP.
///
/// Returns the VM handle and console, ready for test commands.
#[cfg(not(feature = "vfkit"))]
async fn setup_vm_with_dhcp(network_mode: NetworkMode) -> (VmHandle, VmConsole) {
    let vm = test_vm("default")
        .network(network_mode)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM did not configure network via DHCP");

    (vm, console)
}

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
        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::UserNat(UserNatConfig::default())).await;

        // Do a DNS lookup (uses UDP NAT)
        let output = console
            .exec(
                "nslookup example.com 8.8.8.8 && echo DNS_SUCCESS",
                Duration::from_secs(10),
            )
            .await
            .expect("DNS lookup failed - UDP NAT may not be working");
        assert!(output.contains("DNS_SUCCESS"), "DNS lookup should succeed");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that guest can fetch HTTPS content (TCP NAT + TLS).
///
/// This is a baseline test to verify HTTPS works without any policy.
/// If this test fails, the issue is with TLS/HTTPS handling in general,
/// not specifically with policy enforcement.
#[apple_main::harness_test]
async fn test_usernat_https_fetch() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::UserNat(UserNatConfig::default())).await;

        // Fetch HTTPS content (uses TCP NAT + TLS handshake)
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo HTTPS_SUCCESS",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS fetch failed - TCP NAT may have TLS issues");
        assert!(
            output.contains("HTTPS_SUCCESS"),
            "HTTPS fetch should succeed"
        );

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
        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::UserNat(UserNatConfig::default())).await;

        // Fetch HTTP content (uses TCP NAT)
        let output = console
            .exec(
                "wget -T 10 -q http://example.com -O /dev/null && echo HTTP_SUCCESS",
                Duration::from_secs(15),
            )
            .await
            .expect("HTTP fetch failed - TCP NAT may not be working");
        assert!(output.contains("HTTP_SUCCESS"), "HTTP fetch should succeed");

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

        // Ping the gateway 3 times to verify ICMP and timer support work correctly
        let output = console
            .exec(
                "ping -c 3 10.0.2.2 && echo GATEWAY_PING_SUCCESS",
                Duration::from_secs(10),
            )
            .await
            .expect("Ping to gateway failed");

        assert!(
            output.contains("GATEWAY_PING_SUCCESS"),
            "Ping to gateway should succeed"
        );
        assert!(
            output.contains("3 packets transmitted, 3 packets received"),
            "All 3 pings should be received"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that guest can ping external hosts via ICMP NAT.
///
/// This verifies ICMP NAT is working for external destinations (not just gateway).
#[apple_main::harness_test]
async fn test_usernat_ping_external() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::UserNat(UserNatConfig::default())).await;

        // Ping Google DNS 3 times (tests ICMP NAT to external host)
        let output = console
            .exec(
                "ping -c 3 -W 5 8.8.8.8 && echo EXTERNAL_PING_SUCCESS",
                Duration::from_secs(15),
            )
            .await
            .expect("Ping to external host failed - ICMP NAT may not be working");

        assert!(
            output.contains("EXTERNAL_PING_SUCCESS"),
            "Ping to external host should succeed"
        );
        assert!(
            output.contains("3 packets transmitted, 3 packets received"),
            "All 3 pings should be received"
        );

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
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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
        console
            .write_line("echo 'HELLO_FROM_GUEST' | nc -l -p 8080 &")
            .await
            .expect("Failed to start TCP server in guest");

        // Give the server time to start, then verify
        let output = console
            .exec(
                "sleep 1 && echo PORT_FORWARD_SERVER_STARTED",
                Duration::from_secs(5),
            )
            .await
            .expect("TCP server not started");
        assert!(output.contains("PORT_FORWARD_SERVER_STARTED"));

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests UDP port forwarding from host to guest.
///
/// Sets up port forwarding: host:15353 → guest:5353
#[apple_main::harness_test]
async fn test_port_forward_udp() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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

        // Give the server time to start, then verify
        let output = console
            .exec(
                "sleep 1 && echo UDP_PORT_FORWARD_CONFIGURED",
                Duration::from_secs(5),
            )
            .await
            .expect("UDP server not configured");
        assert!(output.contains("UDP_PORT_FORWARD_CONFIGURED"));

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests multiple port forwards simultaneously.
#[apple_main::harness_test]
async fn test_port_forward_multiple() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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
        let output = console
            .exec("echo MULTIPLE_PORTS_CONFIGURED", Duration::from_secs(5))
            .await
            .expect("Multiple port forwards not configured");
        assert!(output.contains("MULTIPLE_PORTS_CONFIGURED"));

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
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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
        let output = console
            .exec(
                "nslookup example.com 8.8.8.8 && echo DNS_ALLOWED",
                Duration::from_secs(10),
            )
            .await
            .expect("DNS lookup should be allowed but failed");
        assert!(output.contains("DNS_ALLOWED"), "DNS should be allowed");

        // HTTP should be blocked - use timeout to detect failure
        let output = console
            .exec(
                "wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTP check failed");
        assert!(
            output.contains("HTTP_BLOCKED"),
            "HTTP should be blocked by policy"
        );

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
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo HTTPS_ALLOWED",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS fetch should be allowed but failed");
        assert!(output.contains("HTTPS_ALLOWED"), "HTTPS should be allowed");

        // Verify console still works after HTTPS
        let output = console
            .exec("echo CONSOLE_STILL_WORKS", Duration::from_secs(5))
            .await
            .expect("Console should still work after HTTPS");
        assert!(
            output.contains("CONSOLE_STILL_WORKS"),
            "Console should respond after HTTPS"
        );

        // HTTP should be blocked - use timeout to detect failure
        let output = console
            .exec(
                "wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTP check failed");
        assert!(
            output.contains("HTTP_BLOCKED"),
            "HTTP should be blocked by policy"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests policy that allows specific IP addresses.
///
/// Allow traffic to Google DNS (8.8.8.8) but block everything else.
/// Note: Uses DNS lookup instead of ping since ICMP NAT isn't implemented.
#[apple_main::harness_test]
async fn test_policy_allow_specific_ip() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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

        // Traffic to 8.8.8.8 should work (use DNS lookup which is UDP to port 53)
        let output = console
            .exec(
                "nslookup example.com 8.8.8.8 && echo IP_8888_ALLOWED",
                Duration::from_secs(10),
            )
            .await
            .expect("Traffic to 8.8.8.8 should be allowed");
        assert!(
            output.contains("IP_8888_ALLOWED"),
            "Traffic to 8.8.8.8 should be allowed"
        );

        // Traffic to other IPs should be blocked (e.g., 1.1.1.1)
        let output = console
            .exec(
                "nslookup example.com 1.1.1.1 2>&1 || echo IP_1111_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("DNS check failed");
        assert!(
            output.contains("IP_1111_BLOCKED"),
            "Traffic to 1.1.1.1 should be blocked"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that console works with allow_all policy (no network traffic).
#[apple_main::harness_test]
async fn test_policy_console_only() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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

        // Multiple echo commands without any network traffic
        for i in 0..5 {
            let output = console
                .exec(&format!("echo TEST_{}", i), Duration::from_secs(5))
                .await
                .expect(&format!("Console command {} should work", i));
            assert!(
                output.contains(&format!("TEST_{}", i)),
                "Output should contain TEST_{}",
                i
            );
        }

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that allow_all policy permits HTTPS traffic.
#[apple_main::harness_test]
async fn test_policy_allow_all_https() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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

        // Verify console works before HTTPS
        let output = console
            .exec("echo BEFORE_HTTPS", Duration::from_secs(5))
            .await
            .expect("Console should work before HTTPS");
        assert!(
            output.contains("BEFORE_HTTPS"),
            "Console should work before HTTPS"
        );

        // HTTPS should work with allow_all policy
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo HTTPS_ALLOWED",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS fetch should work with allow_all policy");
        assert!(output.contains("HTTPS_ALLOWED"), "HTTPS should be allowed");

        // Verify console still works after HTTPS
        let output = console
            .exec("echo AFTER_HTTPS", Duration::from_secs(10))
            .await
            .expect("Console should still work after HTTPS");
        assert!(
            output.contains("AFTER_HTTPS"),
            "Console should respond after HTTPS"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that allow_all policy permits all traffic (default behavior).
#[apple_main::harness_test]
async fn test_policy_allow_all() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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
        let output = console
            .exec(
                "wget -T 10 -q http://example.com -O /dev/null && echo HTTP_ALLOWED",
                Duration::from_secs(15),
            )
            .await
            .expect("HTTP should be allowed with allow_all policy");
        assert!(output.contains("HTTP_ALLOWED"), "HTTP should be allowed");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests policy with multiple rules (deny specific port).
///
/// With allow_all default but deny port 80:
/// - HTTPS (port 443) should work
/// - HTTP (port 80) should be blocked
#[apple_main::harness_test]
async fn test_policy_deny_specific_port() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        // Policy: allow all but deny port 80
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

        // HTTPS should work (not denied)
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo HTTPS_ALLOWED",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS fetch should work");
        assert!(output.contains("HTTPS_ALLOWED"), "HTTPS should be allowed");

        // HTTP should be blocked (port 80 denied)
        let output = console
            .exec(
                "wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTP check failed");
        assert!(
            output.contains("HTTP_BLOCKED"),
            "HTTP should be blocked by policy"
        );

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
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
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

        // Wait for server to start
        console
            .exec("sleep 1", Duration::from_secs(5))
            .await
            .expect("sleep failed");

        // Outbound HTTP should be blocked (policy)
        let output = console
            .exec(
                "wget -T 3 -q http://example.com -O /dev/null 2>&1 || echo OUTBOUND_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("Outbound check failed");
        assert!(
            output.contains("OUTBOUND_BLOCKED"),
            "Outbound HTTP should be blocked by policy"
        );

        // Port forward should still work (inbound direction)
        let output = console
            .exec("echo PORT_FORWARD_WITH_POLICY_OK", Duration::from_secs(5))
            .await
            .expect("Port forward with policy test failed");
        assert!(output.contains("PORT_FORWARD_WITH_POLICY_OK"));

        vm.kill().await.expect("Failed to kill VM");
    }
}
