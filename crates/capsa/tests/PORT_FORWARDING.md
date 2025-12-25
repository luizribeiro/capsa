# Port Forwarding Implementation Plan

## Current State

Port forwarding is **incomplete stub code**. The implementation handles TCP handshake but lacks actual data forwarding.

### What Works
- Host TCP/UDP listeners are created on configured ports
- TCP 3-way handshake: SYN → guest, SYN-ACK handling, ACK response
- Guest MAC learning from responses
- Packet crafting (SYN, ACK, UDP frames)

### What's Broken

#### TCP (`port_forward.rs:322-340`)
```rust
// Line 322-323
// Start bidirectional forwarding
// TODO: Spawn forwarding task   // ❌ NOT IMPLEMENTED

// Line 329-335 - Data from guest is received but:
// TODO: Write to host_stream     // ❌ NOT IMPLEMENTED

// Line 338-340
if tcp_packet.fin() {
    // TODO: Close connection     // ❌ NOT IMPLEMENTED
}
```

#### UDP (`port_forward.rs:345-363`)
```rust
// handle_udp_response just logs and returns false
// Responses from guest are NOT forwarded back to host client
```

### Test Issues

Current tests don't verify data flow:
```rust
// test_port_forward_tcp just does:
console.write_line("echo 'HELLO' | nc -l -p 8080 &")
console.wait_for_timeout("PORT_FORWARD_SERVER_STARTED", ...)
// Never connects from host!
```

---

## Key Insight: nat.rs Already Solved This

**`nat.rs` has fully working bidirectional TCP/UDP forwarding** for outbound connections.
The same patterns can be directly applied to `port_forward.rs` for inbound connections.

### What nat.rs Does Right

```rust
// nat.rs:266-319 - Spawns bidirectional forwarding task on TCP connect
let task_handle = tokio::spawn(async move {
    let (mut read_half, mut write_half) = stream.into_split();
    let mut buf = vec![0u8; 4096];

    loop {
        tokio::select! {
            // Data from guest → send to remote
            Some(data) = data_rx.recv() => {
                write_half.write_all(&data).await?;
                guest_ack = guest_ack.wrapping_add(data.len() as u32);
            }

            // Data from remote → send to guest
            result = read_half.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        // Remote closed - send FIN to guest
                        let frame = craft_tcp_fin(...);
                        tx_to_guest.send(frame).await;
                        break;
                    }
                    Ok(n) => {
                        // Send data to guest
                        let frame = craft_tcp_data(..., &buf[..n], ...);
                        tx_to_guest.send(frame).await;
                        our_seq = our_seq.wrapping_add(n as u32);
                    }
                    Err(_) => break,
                }
            }
        }
    }
});
```

### Helper Functions Already in nat.rs

These already exist and work correctly:
- `craft_tcp_data()` - TCP data packet with payload
- `craft_tcp_fin()` - TCP FIN packet
- `craft_tcp_rst()` - TCP RST packet
- `craft_tcp_ack()` - TCP ACK packet
- `craft_tcp_syn_ack()` - TCP SYN-ACK packet

### The Difference: Direction

| | Outbound (nat.rs) | Inbound (port_forward.rs) |
|---|---|---|
| Initiator | Guest sends SYN to remote | Host client connects to listener |
| Host socket | We open to remote | Already connected (from accept) |
| Craft packets | As remote → guest | As gateway → guest |

The forwarding logic is identical - just the connection setup differs.

---

## Implementation Plan (Simplified)

### Option A: Refactor port_forward.rs (Copy nat.rs Patterns)

Follow the exact same structure as `nat.rs`:

1. **Copy `TcpNatEntry` pattern** from nat.rs to port_forward.rs
2. **Copy the spawned task pattern** that does bidirectional forwarding
3. **Reuse or import** the craft_tcp_* helper functions from nat.rs
4. **Apply same UDP pattern** - spawn receive task that forwards responses

### Option B: Consolidate into nat.rs (Recommended)

Since the code is nearly identical, consider merging port forwarding INTO nat.rs:

```rust
// In NatTable, add:
pub struct NatTable {
    // ... existing fields ...
    /// Port forwards: host_port → (guest_port, listener_handle)
    port_forwards: HashMap<u16, PortForwardEntry>,
}

impl NatTable {
    /// Start a TCP port forward listener
    pub async fn start_tcp_forward(&mut self, host_port: u16, guest_port: u16) {
        let listener = TcpListener::bind(("127.0.0.1", host_port)).await?;

        // When connection accepted, treat it like outbound but reversed:
        // - We already have the host socket (from accept)
        // - We need to SYN to guest (like handle_tcp_syn but we initiate)
        // - Use same TcpNatEntry and forwarding task
    }
}
```

**Benefits**:
- Reuses all existing craft_tcp_* functions
- Same connection tracking and cleanup logic
- Single place for all TCP/UDP forwarding code
- Easier to maintain

### TCP Implementation Steps

1. **When host client connects** (in listener task):
   - Accept connection → have TcpStream
   - Generate virtual port for this connection
   - Create `TcpNatEntry` with `data_tx` channel
   - Spawn forwarding task (copy from nat.rs:266-319)
   - Send SYN to guest

2. **When guest sends SYN-ACK**:
   - Update state to Established
   - Forwarding task starts flowing data

3. **Bidirectional flow** (exact copy of nat.rs):
   - Host → Guest: read from socket, craft_tcp_data, send to guest
   - Guest → Host: receive via data_tx channel, write to socket

4. **Teardown** (exact copy of nat.rs):
   - Host closes: send FIN to guest
   - Guest FINs: ACK and close socket

### UDP Implementation Steps

1. **When host client sends UDP**:
   - Track `(virtual_port → client_addr)` mapping
   - Forward to guest

2. **When guest responds**:
   - Look up client_addr from virtual_port
   - Send response back to original client

---

## Real Integration Tests

**Goal**: Tests that verify actual data flows through the port forward.

### TCP Test

```rust
#[apple_main::harness_test]
async fn test_port_forward_tcp() {
    let vm = test_vm("default")
        .network(NetworkMode::user_nat().forward_tcp(18080, 8080).build())
        .build().await.unwrap();

    let console = vm.console().await.unwrap();
    console.wait_for_timeout("Network configured via DHCP", ...).await.unwrap();

    // Start TCP echo server in guest (backgrounded so console returns)
    console.write_line("net-echo --tcp 8080 &").await.unwrap();
    // Wait for ready message instead of arbitrary sleep
    console.wait_for_timeout("net-echo: listening on TCP port 8080", ...).await.unwrap();

    // Connect from host and verify round-trip
    let mut stream = TcpStream::connect("127.0.0.1:18080").await.unwrap();
    stream.write_all(b"PING\n").await.unwrap();

    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"PING\n"); // Echo server returns same data

    vm.kill().await.unwrap();
}
```

### UDP Test

```rust
#[apple_main::harness_test]
async fn test_port_forward_udp() {
    let vm = test_vm("default")
        .network(NetworkMode::user_nat().forward_udp(15353, 5353).build())
        .build().await.unwrap();

    let console = vm.console().await.unwrap();
    console.wait_for_timeout("Network configured via DHCP", ...).await.unwrap();

    // Start UDP echo server in guest
    console.write_line("net-echo --udp 5353 &").await.unwrap();
    console.wait_for_timeout("net-echo: listening on UDP port 5353", ...).await.unwrap();

    // Send from host and verify response
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.send_to(b"PING", "127.0.0.1:15353").await.unwrap();

    let mut buf = [0u8; 64];
    let (n, _) = tokio::time::timeout(
        Duration::from_secs(5),
        socket.recv_from(&mut buf)
    ).await.unwrap().unwrap();

    assert_eq!(&buf[..n], b"PING");

    vm.kill().await.unwrap();
}
```

### Multiple Connections Test

```rust
#[apple_main::harness_test]
async fn test_port_forward_multiple_connections() {
    let vm = test_vm("default")
        .network(NetworkMode::user_nat().forward_tcp(18080, 8080).build())
        .build().await.unwrap();

    let console = vm.console().await.unwrap();
    console.wait_for_timeout("Network configured via DHCP", ...).await.unwrap();

    // Start server (net-echo handles multiple connections)
    console.write_line("net-echo --tcp 8080 &").await.unwrap();
    console.wait_for_timeout("net-echo: listening on TCP port 8080", ...).await.unwrap();

    // Open 3 concurrent connections
    let handles: Vec<_> = (0..3).map(|i| {
        tokio::spawn(async move {
            let mut stream = TcpStream::connect("127.0.0.1:18080").await?;
            let msg = format!("MSG{}\n", i);
            stream.write_all(msg.as_bytes()).await?;

            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).await?;
            assert_eq!(&buf[..n], msg.as_bytes());
            Ok::<_, std::io::Error>(())
        })
    }).collect();

    for h in handles {
        h.await.unwrap().unwrap();
    }

    vm.kill().await.unwrap();
}
```

---

## Implementation Order

### If Option A (refactor port_forward.rs):
1. Copy `TcpNatEntry` struct from nat.rs
2. Copy spawned forwarding task pattern
3. Export/reuse craft_tcp_* helpers (or copy them)
4. Update handle_tcp_response to use channels
5. Fix UDP response forwarding
6. Create net-echo test utility
7. Write real tests with net-echo
8. Remove skip annotations

### If Option B (consolidate into nat.rs) - Recommended:
1. Add port forward state to NatTable
2. Add `start_tcp_forward()` / `start_udp_forward()` methods
3. Reuse existing TcpNatEntry and forwarding logic
4. Delete port_forward.rs
5. Update stack.rs to use NatTable for port forwards
6. Create net-echo test utility
7. Write real tests with net-echo
8. Remove skip annotations

---

## Test Infrastructure: net-echo

Create a minimal TCP/UDP echo server similar to `vsock-pong`:

```
crates/test-utils/net-echo/
├── Cargo.toml
└── src/
    └── main.rs
```

### Usage

```bash
# TCP echo on port 8080
net-echo --tcp 8080

# UDP echo on port 5353
net-echo --udp 5353

# Both TCP and UDP on multiple ports
net-echo --tcp 8080 --tcp 8081 --udp 5353
```

### Implementation

```rust
// crates/test-utils/net-echo/src/main.rs
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::thread;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--tcp" => {
                let port: u16 = args[i + 1].parse().expect("invalid port");
                thread::spawn(move || tcp_echo(port));
                i += 2;
            }
            "--udp" => {
                let port: u16 = args[i + 1].parse().expect("invalid port");
                thread::spawn(move || udp_echo(port));
                i += 2;
            }
            _ => {
                eprintln!("Usage: net-echo [--tcp PORT]... [--udp PORT]...");
                std::process::exit(1);
            }
        }
    }

    // Wait forever
    loop { std::thread::park(); }
}

fn tcp_echo(port: u16) {
    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind");
    // Tests wait for this message before connecting
    println!("net-echo: listening on TCP port {}", port);

    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            thread::spawn(move || handle_tcp(stream));
        }
    }
}

fn handle_tcp(mut stream: TcpStream) {
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => { let _ = stream.write_all(&buf[..n]); }
            Err(_) => break,
        }
    }
}

fn udp_echo(port: u16) {
    let socket = UdpSocket::bind(("0.0.0.0", port)).expect("bind");
    // Tests wait for this message before sending
    println!("net-echo: listening on UDP port {}", port);

    let mut buf = [0u8; 4096];
    loop {
        if let Ok((n, src)) = socket.recv_from(&mut buf) {
            let _ = socket.send_to(&buf[..n], src);
        }
    }
}
```

### Cargo.toml

```toml
[package]
name = "net-echo"
version = "0.1.0"
edition = "2024"

# Standalone crate (not part of workspace) - Linux only
[workspace]

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

### Adding to Test VMs

In `nix/test-vms/lib.nix`, add net-echo to the initramfs like vsock-pong:

```nix
netEcho = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
  name = "net-echo";
  src = ../../crates/test-utils/net-echo;
  cargoLock.lockFile = ../../crates/test-utils/net-echo/Cargo.lock;
};

extraBinaries = [
  "${netEcho}/bin/net-echo"
];
```

**Benefits over busybox nc / socat:**
- Handles multiple concurrent TCP connections reliably
- No shell quoting or EXEC issues
- Consistent behavior across all test runs
- Minimal binary size (static, stripped)

---

## Files to Modify

### Port forwarding implementation:
- `crates/net/src/nat.rs` - add port forward support (Option B recommended)
- `crates/net/src/port_forward.rs` - delete or make thin wrapper
- `crates/net/src/stack.rs` - update to use NatTable

### Test infrastructure:
- `crates/test-utils/net-echo/` - new crate
- `nix/test-vms/lib.nix` - add net-echo to initramfs
- `crates/capsa/tests/network_test.rs` - real tests using net-echo

## Estimated Complexity

Since nat.rs already has all the hard parts working:

| Task | Complexity | Reason |
|------|------------|--------|
| TCP forwarding | **Low** | Copy existing pattern |
| TCP teardown | **Low** | Already implemented in nat.rs |
| UDP responses | **Low** | Track client addr, use existing socket |
| net-echo utility | **Low** | ~50 lines, similar to vsock-pong |
| Tests | **Low** | net-echo makes this trivial |
| Total | **~2-4 hours** | Mostly copy-paste and wire-up |
