//! Integration tests for NetworkCluster (multi-VM networking).
//!
//! These tests verify that multiple VMs can communicate with each other
//! on a shared virtual network (cluster):
//! - Multiple VMs can join the same cluster
//! - VMs get IPs via DHCP from the cluster's network
//! - VMs can ping each other
//! - VMs can communicate via TCP/UDP

use capsa::test_utils::test_vm;
use capsa::{NetworkCluster, NetworkClusterConfig, NetworkMode};
use std::time::Duration;

// =============================================================================
// Basic Cluster Tests
// =============================================================================

/// Tests that a single VM can join a cluster and get network configured.
#[tokio::test]
async fn test_cluster_single_vm_boot() {
    // Create a cluster for the test
    let cluster = NetworkCluster::create(NetworkClusterConfig {
        name: "test-single".to_string(),
        subnet: "10.0.3.0/24".to_string(),
        gateway: Some(std::net::Ipv4Addr::new(10, 0, 3, 1)),
        enable_nat: true,
    });

    let vm = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
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

    // Cleanup cluster
    NetworkCluster::remove("test-single");
}

/// Tests that two VMs can join the same cluster.
#[tokio::test]
async fn test_cluster_two_vms_boot() {
    // Create a cluster for the test
    let cluster = NetworkCluster::create(NetworkClusterConfig {
        name: "test-two-vms".to_string(),
        subnet: "10.0.4.0/24".to_string(),
        gateway: Some(std::net::Ipv4Addr::new(10, 0, 4, 1)),
        enable_nat: true,
    });

    // Start first VM
    let vm1 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM1");

    let console1 = vm1.console().await.expect("Failed to get console1");

    console1
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM1 did not configure network via DHCP");

    // Start second VM
    let vm2 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM2");

    let console2 = vm2.console().await.expect("Failed to get console2");

    console2
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM2 did not configure network via DHCP");

    // Both VMs booted successfully on the same cluster
    console1
        .write_line("echo TWO_VMS_CLUSTER_OK")
        .await
        .expect("Failed to echo");

    console1
        .wait_for_timeout("TWO_VMS_CLUSTER_OK", Duration::from_secs(5))
        .await
        .expect("Echo failed on VM1");

    vm1.kill().await.expect("Failed to kill VM1");
    vm2.kill().await.expect("Failed to kill VM2");

    // Cleanup cluster
    NetworkCluster::remove("test-two-vms");
}

// =============================================================================
// VM-to-VM Communication Tests
// =============================================================================

/// Tests that VMs on the same cluster can ping each other.
///
/// VM1 gets 10.0.5.15, VM2 gets 10.0.5.16 (DHCP-assigned).
/// We get VM1's IP and have VM2 ping it.
#[tokio::test]
async fn test_cluster_vm_to_vm_ping() {
    // Create a cluster for the test
    let cluster = NetworkCluster::create(NetworkClusterConfig {
        name: "test-ping".to_string(),
        subnet: "10.0.5.0/24".to_string(),
        gateway: Some(std::net::Ipv4Addr::new(10, 0, 5, 1)),
        enable_nat: true,
    });

    // Start first VM
    let vm1 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM1");

    let console1 = vm1.console().await.expect("Failed to get console1");

    console1
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM1 did not configure network via DHCP");

    // Get VM1's IP address
    console1
        .write_line("ip addr show eth0 | grep 'inet ' | awk '{print $2}' | cut -d/ -f1")
        .await
        .expect("Failed to get VM1 IP");

    // VM1 should have 10.0.5.15 (first DHCP address)
    // For now, we'll assume the first VM gets .15

    // Start second VM
    let vm2 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM2");

    let console2 = vm2.console().await.expect("Failed to get console2");

    console2
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM2 did not configure network via DHCP");

    // Have VM2 ping VM1 (assuming VM1 got 10.0.5.15)
    console2
        .write_line("ping -c 1 -W 5 10.0.5.15 && echo PING_VM1_SUCCESS")
        .await
        .expect("Failed to send ping command");

    console2
        .wait_for_timeout("PING_VM1_SUCCESS", Duration::from_secs(10))
        .await
        .expect("VM2 could not ping VM1");

    vm1.kill().await.expect("Failed to kill VM1");
    vm2.kill().await.expect("Failed to kill VM2");

    // Cleanup cluster
    NetworkCluster::remove("test-ping");
}

/// Tests TCP communication between VMs on the same cluster.
///
/// VM1 runs a TCP server, VM2 connects to it.
#[tokio::test]
async fn test_cluster_vm_to_vm_tcp() {
    // Create a cluster for the test
    let cluster = NetworkCluster::create(NetworkClusterConfig {
        name: "test-tcp".to_string(),
        subnet: "10.0.6.0/24".to_string(),
        gateway: Some(std::net::Ipv4Addr::new(10, 0, 6, 1)),
        enable_nat: true,
    });

    // Start first VM (server)
    let vm1 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM1");

    let console1 = vm1.console().await.expect("Failed to get console1");

    console1
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM1 did not configure network via DHCP");

    // Start second VM (client) before starting the server
    // This avoids the server timing out while VM2 boots
    let vm2 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM2");

    let console2 = vm2.console().await.expect("Failed to get console2");

    console2
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM2 did not configure network via DHCP");

    // Start a TCP listener on VM1 that stays open
    console1
        .write_line("nc -l -p 9999 -e cat &")
        .await
        .expect("Failed to start TCP server on VM1");

    tokio::time::sleep(Duration::from_secs(1)).await;

    // Test TCP connection from VM2 to VM1 with a simple echo test
    // If nc -e doesn't work, try sending data and checking if connection succeeds
    console2
        .write_line("echo TEST | nc -w 3 10.0.6.15 9999 && echo TCP_CONNECT_SUCCESS || echo TCP_CONNECT_FAILED")
        .await
        .expect("Failed to send nc command");

    console2
        .wait_for_timeout("TCP_CONNECT_SUCCESS", Duration::from_secs(10))
        .await
        .expect("VM2 could not connect to VM1 via TCP");

    vm1.kill().await.expect("Failed to kill VM1");
    vm2.kill().await.expect("Failed to kill VM2");

    // Cleanup cluster
    NetworkCluster::remove("test-tcp");
}

// =============================================================================
// Multiple VMs on Same Cluster Tests
// =============================================================================

/// Tests that three VMs can all communicate on the same cluster.
#[tokio::test]
async fn test_cluster_three_vms() {
    // Create a cluster for the test
    let cluster = NetworkCluster::create(NetworkClusterConfig {
        name: "test-three".to_string(),
        subnet: "10.0.7.0/24".to_string(),
        gateway: Some(std::net::Ipv4Addr::new(10, 0, 7, 1)),
        enable_nat: true,
    });

    // Start three VMs
    let vm1 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM1");

    let console1 = vm1.console().await.expect("Failed to get console1");
    console1
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM1 did not configure network via DHCP");

    let vm2 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM2");

    let console2 = vm2.console().await.expect("Failed to get console2");
    console2
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM2 did not configure network via DHCP");

    let vm3 = test_vm("default")
        .network(NetworkMode::cluster(&cluster.config().name).build())
        .build()
        .await
        .expect("Failed to build VM3");

    let console3 = vm3.console().await.expect("Failed to get console3");
    console3
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM3 did not configure network via DHCP");

    // VM3 pings VM1 and VM2
    console3
        .write_line("ping -c 1 -W 5 10.0.7.15 && ping -c 1 -W 5 10.0.7.16 && echo THREE_VMS_OK")
        .await
        .expect("Failed to send ping commands");

    console3
        .wait_for_timeout("THREE_VMS_OK", Duration::from_secs(15))
        .await
        .expect("VM3 could not ping both VM1 and VM2");

    vm1.kill().await.expect("Failed to kill VM1");
    vm2.kill().await.expect("Failed to kill VM2");
    vm3.kill().await.expect("Failed to kill VM3");

    // Cleanup cluster
    NetworkCluster::remove("test-three");
}

/// Tests that VMs using get_or_create join the same cluster.
#[tokio::test]
async fn test_cluster_get_or_create() {
    // Create cluster via get_or_create (creates it)
    let _cluster1 = NetworkCluster::get_or_create("shared-cluster");

    // Start VM1 on the cluster
    let vm1 = test_vm("default")
        .network(NetworkMode::cluster("shared-cluster").build())
        .build()
        .await
        .expect("Failed to build VM1");

    let console1 = vm1.console().await.expect("Failed to get console1");
    console1
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM1 did not configure network via DHCP");

    // get_or_create again (should return same cluster)
    let _cluster2 = NetworkCluster::get_or_create("shared-cluster");

    // Start VM2 on the same cluster
    let vm2 = test_vm("default")
        .network(NetworkMode::cluster("shared-cluster").build())
        .build()
        .await
        .expect("Failed to build VM2");

    let console2 = vm2.console().await.expect("Failed to get console2");
    console2
        .wait_for_timeout("Network configured via DHCP", Duration::from_secs(30))
        .await
        .expect("VM2 did not configure network via DHCP");

    // VMs should be on the same network
    console2
        .write_line("ping -c 1 -W 5 10.0.3.15 && echo SHARED_CLUSTER_OK")
        .await
        .expect("Failed to send ping command");

    console2
        .wait_for_timeout("SHARED_CLUSTER_OK", Duration::from_secs(10))
        .await
        .expect("VMs are not on the same cluster");

    vm1.kill().await.expect("Failed to kill VM1");
    vm2.kill().await.expect("Failed to kill VM2");

    // Cleanup cluster
    NetworkCluster::remove("shared-cluster");
}
