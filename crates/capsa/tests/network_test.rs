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

// =============================================================================
// Domain-Based Policy Tests
// =============================================================================

/// Tests that exact domain matching works.
///
/// With deny_all + allow_domain("example.com"):
/// - HTTPS to example.com should work
/// - HTTPS to other domains should be blocked
#[apple_main::harness_test]
async fn test_policy_allow_domain_exact() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        // Policy: deny all except example.com (DNS is allowed via our proxy)
        let policy = NetworkPolicy::deny_all().allow_domain("example.com");

        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::user_nat().policy(policy).build()).await;

        // HTTPS to example.com should work
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo DOMAIN_ALLOWED",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS to example.com should be allowed");
        assert!(
            output.contains("DOMAIN_ALLOWED"),
            "Traffic to example.com should be allowed"
        );

        // HTTPS to other domains should be blocked
        let output = console
            .exec(
                "wget -T 5 -q https://httpbin.org/get -O /dev/null 2>&1 || echo OTHER_DOMAIN_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTPS to other domain check failed");
        assert!(
            output.contains("OTHER_DOMAIN_BLOCKED"),
            "Traffic to other domains should be blocked"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that wildcard domain matching works.
///
/// With deny_all + allow_domain("*.example.com"):
/// - Traffic to subdomain.example.com should work
/// - Traffic to example.com (base domain) should be blocked
#[apple_main::harness_test]
async fn test_policy_allow_domain_wildcard() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        // Policy: deny all except *.example.com subdomains
        let policy = NetworkPolicy::deny_all().allow_domain("*.example.com");

        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::user_nat().policy(policy).build()).await;

        // Traffic to www.example.com (subdomain) should work
        let output = console
            .exec(
                "wget -T 15 -q https://www.example.com -O /dev/null && echo SUBDOMAIN_ALLOWED",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS to www.example.com should be allowed");
        assert!(
            output.contains("SUBDOMAIN_ALLOWED"),
            "Traffic to subdomain should be allowed"
        );

        // Traffic to example.com (base domain, not a subdomain) should be blocked
        let output = console
            .exec(
                "wget -T 5 -q https://example.com -O /dev/null 2>&1 || echo BASE_DOMAIN_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTPS to base domain check failed");
        assert!(
            output.contains("BASE_DOMAIN_BLOCKED"),
            "Traffic to base domain should be blocked (wildcard only matches subdomains)"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests combining domain and port rules.
///
/// With deny_all + allow(example.com AND port 443):
/// - HTTPS to example.com should work
/// - HTTP to example.com should be blocked (wrong port)
#[apple_main::harness_test]
async fn test_policy_domain_with_port() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        use capsa_core::{PolicyAction, RuleMatcher};

        // Policy: deny all except (example.com AND port 443)
        let policy = NetworkPolicy::deny_all().rule(
            PolicyAction::Allow,
            RuleMatcher::All(vec![
                RuleMatcher::Domain(capsa_core::DomainPattern::parse("example.com")),
                RuleMatcher::Port(443),
            ]),
        );

        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::user_nat().policy(policy).build()).await;

        // HTTPS to example.com should work (matches domain AND port 443)
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo HTTPS_DOMAIN_ALLOWED",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS to example.com should be allowed");
        assert!(
            output.contains("HTTPS_DOMAIN_ALLOWED"),
            "HTTPS to example.com should be allowed"
        );

        // HTTP to example.com should be blocked (wrong port)
        let output = console
            .exec(
                "wget -T 5 -q http://example.com -O /dev/null 2>&1 || echo HTTP_DOMAIN_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTP to example.com check failed");
        assert!(
            output.contains("HTTP_DOMAIN_BLOCKED"),
            "HTTP to example.com should be blocked (port 80 not allowed)"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests that Log action is non-terminal (logs but continues evaluation).
///
/// With deny_all + log(all) + allow_port(443):
/// - HTTPS should work (log matches, continues, allow_port matches)
/// - HTTP should be blocked (log matches, continues, no other match, deny)
#[apple_main::harness_test]
async fn test_policy_log_then_allow() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        use capsa_core::{PolicyAction, RuleMatcher};

        // Policy: deny_all, log everything, then allow port 443
        let policy = NetworkPolicy::deny_all()
            .rule(PolicyAction::Log, RuleMatcher::Any)
            .allow_port(443);

        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::user_nat().policy(policy).build()).await;

        // HTTPS should work (Log is non-terminal, then allow_port(443) matches)
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo HTTPS_AFTER_LOG",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS should be allowed after log");
        assert!(
            output.contains("HTTPS_AFTER_LOG"),
            "HTTPS should be allowed (log is non-terminal)"
        );

        // HTTP should be blocked (Log matches, continues, no allow match, default deny)
        let output = console
            .exec(
                "wget -T 5 -q http://example.com -O /dev/null 2>&1 || echo HTTP_BLOCKED_AFTER_LOG",
                Duration::from_secs(10),
            )
            .await
            .expect("HTTP should be blocked after log");
        assert!(
            output.contains("HTTP_BLOCKED_AFTER_LOG"),
            "HTTP should be blocked (log continues, then default deny)"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests mixing IP-based and domain-based rules.
///
/// With deny_all + allow_ip(8.8.8.8) + allow_domain("example.com"):
/// - DNS to 8.8.8.8 should work (IP rule)
/// - HTTPS to example.com should work (domain rule)
/// - HTTPS to other domains should be blocked
#[apple_main::harness_test]
async fn test_policy_mixed_ip_and_domain() {
    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit backend doesn't support VZFileHandleNetworkDeviceAttachment");
        return;
    }

    #[cfg(not(feature = "vfkit"))]
    {
        // Policy: deny all except 8.8.8.8 (IP) and example.com (domain)
        let policy = NetworkPolicy::deny_all()
            .allow_ip(Ipv4Addr::new(8, 8, 8, 8))
            .allow_domain("example.com");

        let (vm, console) =
            setup_vm_with_dhcp(NetworkMode::user_nat().policy(policy).build()).await;

        // DNS to 8.8.8.8 should work (IP-based rule)
        let output = console
            .exec(
                "nslookup example.com 8.8.8.8 && echo IP_RULE_WORKS",
                Duration::from_secs(10),
            )
            .await
            .expect("DNS to 8.8.8.8 should work");
        assert!(
            output.contains("IP_RULE_WORKS"),
            "IP-based allow rule should work"
        );

        // HTTPS to example.com should work (domain-based rule)
        let output = console
            .exec(
                "wget -T 15 -q https://example.com -O /dev/null && echo DOMAIN_RULE_WORKS",
                Duration::from_secs(20),
            )
            .await
            .expect("HTTPS to example.com should work");
        assert!(
            output.contains("DOMAIN_RULE_WORKS"),
            "Domain-based allow rule should work"
        );

        // DNS to 1.1.1.1 should be blocked (not in allowed IPs or domains)
        let output = console
            .exec(
                "nslookup example.com 1.1.1.1 2>&1 || echo OTHER_IP_BLOCKED",
                Duration::from_secs(10),
            )
            .await
            .expect("DNS to 1.1.1.1 check failed");
        assert!(
            output.contains("OTHER_IP_BLOCKED"),
            "Traffic to other IPs should be blocked"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}
