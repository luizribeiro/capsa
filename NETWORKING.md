# Capsa Userspace Networking

Cross-platform userspace NAT networking for capsa using smoltcp.

---

## Overview

This document describes the implementation of userspace NAT networking for capsa. The goal is to provide:

- **Unprivileged networking**: No root/CAP_NET_ADMIN required
- **Cross-platform**: Identical behavior on macOS and Linux
- **Policy control**: Built-in connection filtering (future phase)
- **Multi-VM support**: Virtual switch for VM-to-VM communication (future phase)

This is critical for capsa's AI agent sandboxing use case where we need strict control over what the guest VM can access.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Guest VM                             │
│   Applications → Kernel TCP/IP → virtio-net driver          │
└─────────────────────────┬───────────────────────────────────┘
                          │ ethernet frames
┌─────────────────────────▼───────────────────────────────────┐
│                    capsa VMM (Rust)                         │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │              Frame I/O Abstraction                    │  │
│  │                                                       │  │
│  │   Linux: TAP fd          macOS: socketpair fd         │  │
│  │   (via virtio-net)       (via VZFileHandle...)        │  │
│  └───────────────────────────┬───────────────────────────┘  │
│                              │                              │
│  ┌───────────────────────────▼───────────────────────────┐  │
│  │                 smoltcp Interface                     │  │
│  │                                                       │  │
│  │   - Parses ethernet frames                            │  │
│  │   - Terminates TCP/UDP connections                    │  │
│  │   - Handles ARP, ICMP echo, DHCP                      │  │
│  └───────────────────────────┬───────────────────────────┘  │
│                              │                              │
│  ┌───────────────────────────▼───────────────────────────┐  │
│  │                 Connection Tracker                    │  │
│  │                                                       │  │
│  │   - Maps guest connections → host sockets             │  │
│  │   - Enforces network policy (future)                  │  │
│  └───────────────────────────┬───────────────────────────┘  │
│                              │                              │
│  ┌───────────────────────────▼───────────────────────────┐  │
│  │                   Host Sockets                        │  │
│  │                                                       │  │
│  │   - Async TCP/UDP via tokio                           │  │
│  │   - Bidirectional data shuttling                      │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

---

## Public API

### NetworkMode Enum

```rust
// In capsa-core/src/types/network.rs

#[derive(Debug, Clone, Default)]
pub enum NetworkMode {
    /// No networking
    None,
    /// Platform-native NAT (macOS VZ built-in, not available on KVM)
    #[default]
    Nat,
    /// Userspace NAT via capsa-net (cross-platform, supports filtering)
    UserNat(UserNatConfig),
}

impl NetworkMode {
    /// Create userspace NAT with default configuration
    pub fn user_nat() -> UserNatConfigBuilder {
        UserNatConfigBuilder::default()
    }
}
```

### UserNatConfig

```rust
// In capsa-core/src/types/network.rs

#[derive(Debug, Clone)]
pub struct UserNatConfig {
    /// Subnet for the guest network (default: 10.0.2.0/24)
    pub subnet: Ipv4Network,
    /// Network policy for filtering (Phase 5, None = allow all)
    pub policy: Option<NetworkPolicy>,
    /// Port forwards from host to guest (Phase 4)
    pub port_forwards: Vec<PortForward>,
}

impl Default for UserNatConfig {
    fn default() -> Self {
        Self {
            subnet: "10.0.2.0/24".parse().unwrap(),
            policy: None,
            port_forwards: vec![],
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct UserNatConfigBuilder {
    config: UserNatConfig,
}

impl UserNatConfigBuilder {
    pub fn subnet(mut self, subnet: &str) -> Self {
        self.config.subnet = subnet.parse().expect("invalid subnet");
        self
    }

    // Phase 4
    pub fn forward(mut self, host_port: u16, guest_port: u16) -> Self {
        self.config.port_forwards.push(PortForward {
            protocol: Protocol::Tcp,
            host_port,
            guest_port,
        });
        self
    }

    // Phase 5
    pub fn policy(mut self, policy: NetworkPolicy) -> Self {
        self.config.policy = Some(policy);
        self
    }

    pub fn build(self) -> NetworkMode {
        NetworkMode::UserNat(self.config)
    }
}

// Allow implicit conversion for ergonomic API
impl From<UserNatConfigBuilder> for NetworkMode {
    fn from(builder: UserNatConfigBuilder) -> Self {
        builder.build()
    }
}
```

### Usage Examples

```rust
// Simple: userspace NAT with defaults
let vm = Vm::builder()
    .linux_direct(&kernel, &initrd)
    .rootfs(&disk)
    .network(NetworkMode::user_nat())
    .build()?
    .start()
    .await?;

// With custom subnet
let vm = Vm::builder()
    .linux_direct(&kernel, &initrd)
    .network(NetworkMode::user_nat().subnet("192.168.100.0/24"))
    .build()?
    .start()
    .await?;

// Phase 4: With port forwarding
let vm = Vm::builder()
    .network(
        NetworkMode::user_nat()
            .forward(8080, 80)
            .forward(2222, 22)
    )
    .build()?
    .start()
    .await?;

// Phase 5: With filtering policy
let vm = Vm::builder()
    .network(
        NetworkMode::user_nat()
            .policy(
                NetworkPolicy::deny_all()
                    .allow_https("api.anthropic.com")
                    .allow_https("github.com")
            )
    )
    .build()?
    .start()
    .await?;
```

### Multi-VM API (Phase 3)

```rust
// Create a shared network cluster
let cluster = NetworkCluster::builder()
    .name("agent-cluster")
    .subnet("10.0.2.0/24")
    .build()?;

// Each VM gets a port on the cluster
let vm1 = Vm::builder()
    .network(cluster.port())  // Auto-assigned IP (10.0.2.2)
    .build()?;

let vm2 = Vm::builder()
    .network(cluster.port().ip("10.0.2.10"))  // Explicit IP
    .build()?;

// Network lifecycle is automatic:
// - Starts when first VM starts
// - Stops when last VM stops
```

---

## Crate Structure

```
crates/
└── capsa-net/
    ├── Cargo.toml
    └── src/
        ├── lib.rs              # Public API: UserNatStack
        ├── frame_io.rs         # FrameIO trait
        ├── socketpair.rs       # macOS: socketpair device
        ├── tap.rs              # Linux: TAP device (Phase 2)
        ├── stack.rs            # smoltcp Device wrapper
        ├── nat.rs              # Connection tracking, TCP/UDP NAT
        ├── dhcp.rs             # DHCP server
        ├── switch.rs           # Virtual L2 switch (Phase 3)
        ├── port_forward.rs     # Host→guest forwarding (Phase 4)
        └── policy.rs           # Network filtering (Phase 5)
```

### Dependencies

```toml
[dependencies]
smoltcp = { version = "0.12", default-features = false, features = [
    "medium-ethernet",
    "proto-ipv4",
    "socket-tcp",
    "socket-udp",
    "socket-dhcpv4",
    "async",
] }
tokio = { version = "1", features = ["net", "sync", "time", "io-util"] }
ipnetwork = "0.20"
tracing = "0.1"

[target.'cfg(target_os = "linux")'.dependencies]
nix = { version = "0.29", features = ["ioctl", "net"] }
```

---

## Internal Components

### FrameIO Trait

```rust
// src/frame_io.rs

use std::io;
use std::task::{Context, Poll};

/// Abstraction for ethernet frame transport
pub trait FrameIO: Send + 'static {
    /// Maximum transmission unit (typically 1500 for ethernet)
    fn mtu(&self) -> usize;

    /// Poll for incoming frame
    fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>>;

    /// Send an ethernet frame
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
}
```

### SocketPairDevice (macOS)

```rust
// src/socketpair.rs

use std::os::fd::OwnedFd;
use tokio::io::unix::AsyncFd;

/// Frame I/O via Unix socketpair for Virtualization.framework
pub struct SocketPairDevice {
    fd: AsyncFd<OwnedFd>,
}

impl SocketPairDevice {
    /// Create socketpair, returns (host_device, guest_fd)
    /// The guest_fd should be passed to VZFileHandleNetworkDeviceAttachment
    pub fn new() -> io::Result<(Self, OwnedFd)> {
        // socketpair(AF_UNIX, SOCK_DGRAM, 0)
        // Each sendmsg/recvmsg is one ethernet frame
    }
}

impl FrameIO for SocketPairDevice { ... }
```

### UserNatStack

```rust
// src/lib.rs

/// The main userspace NAT stack
pub struct UserNatStack<F: FrameIO> {
    device: SmoltcpDevice<F>,
    iface: smoltcp::iface::Interface,
    sockets: smoltcp::iface::SocketSet<'static>,
    nat: ConnectionTracker,
    dhcp: DhcpServer,
    config: StackConfig,
}

impl<F: FrameIO> UserNatStack<F> {
    pub fn new(frame_io: F, config: UserNatConfig) -> Self { ... }

    /// Run the network stack (spawn as tokio task)
    pub async fn run(mut self) -> Result<(), NetError> {
        let mut interval = tokio::time::interval(Duration::from_millis(1));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.poll_interface();
                    self.process_connections().await?;
                }
                // TODO: Optimize with proper async wakeups instead of polling
            }
        }
    }
}
```

### DHCP Server

The test VM uses `udhcpc` at boot to request an IP via DHCP. We implement a minimal DHCP server:

- Responds to DISCOVER with OFFER
- Responds to REQUEST with ACK
- Assigns IPs from subnet (e.g., 10.0.2.15 for first guest)
- Provides gateway (10.0.2.2) and DNS (uses host's resolver)

### Connection Tracker / NAT

```rust
// src/nat.rs

pub struct ConnectionTracker {
    tcp_connections: HashMap<ConnectionKey, TcpConnection>,
    udp_bindings: HashMap<ConnectionKey, UdpSocket>,
    // Phase 5: policy filter here
}

impl ConnectionTracker {
    /// Handle outbound TCP SYN from guest
    pub async fn handle_tcp_connect(&mut self, key: ConnectionKey) -> Result<()> {
        // 1. [Phase 5] Check policy - stub for now, always allow
        // 2. Open host TCP socket to destination
        // 3. Track the connection
    }

    /// Handle outbound UDP packet from guest
    pub async fn handle_udp(&mut self, key: ConnectionKey, data: &[u8]) -> Result<()> {
        // 1. [Phase 5] Check policy - stub for now, always allow
        // 2. Get or create host UDP socket
        // 3. Forward packet
    }
}
```

---

## Backend Integration

### macOS (capsa-apple-vz)

When `NetworkMode::UserNat` is configured:

```rust
// In VM setup code
match &config.network {
    NetworkMode::UserNat(user_config) => {
        // 1. Create socketpair
        let (host_device, guest_fd) = SocketPairDevice::new()?;

        // 2. Configure VZ to use guest_fd
        let file_handle = unsafe { FileHandle::from_fd(guest_fd) };
        let attachment = VZFileHandleNetworkDeviceAttachment::new(file_handle);
        let network_device = VZVirtioNetworkDeviceConfiguration::new();
        network_device.set_attachment(attachment);
        vz_config.add_network_device(network_device);

        // 3. Spawn userspace NAT stack
        let stack = UserNatStack::new(host_device, user_config.clone());
        tokio::spawn(async move {
            if let Err(e) = stack.run().await {
                tracing::error!("Network stack error: {}", e);
            }
        });
    }
    // ... other modes
}
```

### Linux KVM (capsa-linux-kvm) - Phase 2

When `NetworkMode::UserNat` is configured:

```rust
// In VM setup code
match &config.network {
    NetworkMode::UserNat(user_config) => {
        // 1. Create TAP device
        let tap = TapDevice::new("capsa%d")?;

        // 2. Wire TAP to virtio-net device
        self.virtio_net.set_tap_fd(tap.as_raw_fd());

        // 3. Spawn userspace NAT stack
        let stack = UserNatStack::new(tap, user_config.clone());
        tokio::spawn(async move {
            if let Err(e) = stack.run().await {
                tracing::error!("Network stack error: {}", e);
            }
        });
    }
    // ... other modes
}
```

---

## Implementation Phases

### Phase 1: Basic NAT + DHCP + macOS Integration

**Goal**: Guest VM can make outbound TCP/UDP connections on macOS.

**Scope**:
- Create `capsa-net` crate
- Implement `FrameIO` trait
- Implement `SocketPairDevice` for macOS
- Integrate smoltcp for packet processing
- Implement DHCP server (guest gets IP automatically)
- Implement ARP responder
- Implement ICMP echo (ping) responder
- Implement TCP NAT (outbound connections)
- Implement UDP NAT (for DNS and general UDP)
- Integrate with `capsa-apple-vz` (native and subprocess strategies)
- Add `NetworkMode::UserNat` to API
- Integration tests: ping, wget

**Deliverables**:
- `crates/capsa-net/` with core functionality
- Updated `capsa-core` with `UserNat` network mode
- Updated `capsa-apple-vz` with integration
- Integration test in `crates/capsa/tests/network_test.rs`

### Phase 2: Linux KVM Integration

**Goal**: Same functionality works on Linux with KVM backend.

**Scope**:
- Implement `TapDevice` for Linux
- Integrate with `capsa-linux-kvm`
- Ensure tests pass on Linux

**Deliverables**:
- `crates/capsa-net/src/tap.rs`
- Updated `capsa-linux-kvm` with integration
- Same integration tests passing on Linux

### Phase 3: Virtual Switch (Multi-VM)

**Goal**: Multiple VMs can communicate with each other.

**Scope**:
- Implement `VirtualSwitch` with MAC learning
- Implement `SwitchedPort` FrameIO
- Implement `NetworkCluster` API
- Automatic network lifecycle management
- Integration test: two VMs can ping each other

**Deliverables**:
- `crates/capsa-net/src/switch.rs`
- `NetworkCluster` API in `capsa-core`
- Multi-VM integration test

### Phase 4: Port Forwarding

**Goal**: Host can connect to services running in guest.

**Scope**:
- Implement TCP port forwarding (host listener → guest)
- Implement UDP port forwarding
- Builder API: `.forward(host_port, guest_port)`
- Integration test: host curls guest web server

**Deliverables**:
- `crates/capsa-net/src/port_forward.rs`
- Updated builder API
- Port forwarding integration test

### Phase 5: Network Filtering

**Goal**: Control what network destinations guest can access.

**Scope**:
- Implement `NetworkPolicy` with allow/deny rules
- IP-based filtering
- DNS interception for domain-based filtering
- Audit logging of connection attempts
- Builder API: `.policy(...)`
- Integration test: blocked connections fail

**Deliverables**:
- `crates/capsa-net/src/policy.rs`
- DNS interception in DHCP/stack
- Policy enforcement in connection tracker
- Filtering integration test

---

## Testing Strategy

### Integration Tests

All phases require integration tests in `crates/capsa/tests/network_test.rs`.

Tests should work across backends:
```bash
cargo test-macos             # macOS backend
cargo test-linux             # Linux KVM backend
```

Example test structure:

```rust
// crates/capsa/tests/network_test.rs

#[tokio::test]
async fn test_usernat_ping_gateway() {
    let vm = test_vm("default")
        .network(NetworkMode::user_nat())
        .build()
        .unwrap()
        .start()
        .await
        .unwrap();

    let console = vm.console();
    console.wait_for("Network configured via DHCP").await.unwrap();

    // Ping the gateway (our NAT stack)
    console.write_line("ping -c 1 10.0.2.2").await.unwrap();
    console.wait_for("1 packets transmitted, 1 received").await.unwrap();

    vm.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_usernat_external_connectivity() {
    let vm = test_vm("default")
        .network(NetworkMode::user_nat())
        .build()
        .unwrap()
        .start()
        .await
        .unwrap();

    let console = vm.console();
    console.wait_for("Network configured via DHCP").await.unwrap();

    // Fetch a known URL
    console.write_line("wget -q -O /dev/null http://example.com && echo OK").await.unwrap();
    console.wait_for("OK").await.unwrap();

    vm.shutdown().await.unwrap();
}
```

### Unit Tests

Add unit tests for complex logic:
- DHCP packet parsing/generation
- Policy rule matching
- MAC address learning in switch
- Connection tracking state machine

---

## Development Workflow

### Commit Guidelines

1. **Small commits**: Each commit should represent one cohesive change
2. **Tests with implementation**: Include tests in the same commit as the code they test
3. **Working state**: Each commit should leave the codebase in a working state
4. **Review before commit**: Use the code-reviewer agent to review changes

### Commit Sequence Example (Phase 1)

```
1. "Add capsa-net crate with FrameIO trait"
2. "Add SocketPairDevice for macOS"
3. "Add smoltcp Device wrapper"
4. "Add DHCP server implementation"
5. "Add ARP and ICMP responders"
6. "Add TCP NAT with connection tracking"
7. "Add UDP NAT"
8. "Add UserNat network mode to capsa-core"
9. "Integrate UserNat with capsa-apple-vz native backend"
10. "Integrate UserNat with capsa-apple-vz subprocess backend"
11. "Add network integration tests"
```

### Running Tests

```bash
# macOS
cargo test-macos

# Linux
cargo test-linux

# All tests
cargo test
```

### Code Review

Before each commit, run the code-reviewer agent:
```
/code-review
```

---

## Network Configuration Details

### Default Subnet Layout

For subnet `10.0.2.0/24`:
- `10.0.2.1` - Reserved (not used)
- `10.0.2.2` - Gateway (NAT stack's IP)
- `10.0.2.15` - First guest IP (DHCP assigned)
- `10.0.2.16+` - Additional guests

### DHCP Lease Details

- Lease time: 1 hour (sufficient for typical VM lifetime)
- Gateway: Subnet's .2 address
- DNS: Host's resolver (or 8.8.8.8 as fallback)
- Subnet mask: From configured subnet

### MAC Address Generation

Auto-generated MACs use prefix `52:54:00` (QEMU convention) followed by bytes derived from the guest IP.

---

## Future Considerations

### Performance Optimizations

The initial implementation uses polling (1ms interval). Future optimizations:
- Use proper async wakeups instead of fixed polling
- Batch frame processing
- Consider zero-copy frame handling

### IPv6 Support

Initial implementation is IPv4 only. IPv6 can be added later:
- NDP instead of ARP
- DHCPv6 or SLAAC
- IPv6 NAT or prefix delegation

### Additional Features

- Connection metrics/statistics
- Bandwidth limiting
- Traffic shaping
- SOCKS/HTTP proxy support
