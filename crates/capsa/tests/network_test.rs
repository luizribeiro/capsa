#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for UserNat networking.
//!
//! These tests verify that the userspace NAT networking stack works correctly:
//! - Guest gets IP via DHCP from our DHCP server
//! - Guest can ping the gateway
//!
//! NOTE: The vfkit backend doesn't support UserNat (VZFileHandleNetworkDeviceAttachment).

use capsa::test_utils::test_vm;
use capsa_core::{NetworkMode, UserNatConfig};
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
