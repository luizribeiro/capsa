# Known Network Issues

This document tracks known issues with the UserNat networking stack that need investigation.

## 1. HTTPS Fails When Network Policy is Enabled

**Status**: Open
**Severity**: Medium
**Affects**: `test_policy_allow_https_only`, `test_policy_deny_specific_port`

### Description

When a network policy is configured (even one that explicitly allows port 443), HTTPS wget requests fail silently. The wget command hangs until timeout, and subsequent console commands also timeout, suggesting the VM gets into a bad state.

### Reproduction

```rust
let policy = NetworkPolicy::deny_all().allow_dns().allow_https();

let vm = test_vm("default")
    .network(NetworkMode::user_nat().policy(policy).build())
    .build()
    .await?;

// This times out even though port 443 is allowed
console.exec("wget -T 10 https://example.com -O /dev/null", Duration::from_secs(15)).await?;
```

### Observations

- DNS lookups work correctly with policy enabled
- HTTP wget works without any policy
- HTTP wget with `allow_all()` policy works
- HTTPS wget fails as soon as ANY policy is configured
- After HTTPS fails, subsequent console commands also timeout

### Possible Causes

1. Policy enforcement may be blocking something required for TLS (e.g., specific ports for TLS handshake)
2. The policy checker might not be correctly allowing port 443 traffic
3. There may be an issue with how TCP connections are tracked through the NAT when policy is enabled

### Files to Investigate

- `crates/net/src/policy.rs` - Policy enforcement logic
- `crates/net/src/stack.rs` - Where policy is applied (lines 228-262)
- `crates/net/src/nat.rs` - TCP NAT handling

---

## 2. HTTP Fetch Test Broken on KVM

**Status**: Open
**Severity**: High
**Affects**: `test_usernat_http_fetch`

### Description

The `test_usernat_http_fetch` test times out 100% of the time on the Linux KVM backend. The test uses the `exec()` method and always hits the 15-second timeout.

### Reproduction

```bash
cargo test-linux --package capsa --test network_test test_usernat_http_fetch
```

### Observations

- Test fails 100% of the time (not flaky - completely broken)
- Times out at exactly 15 seconds (the configured timeout)
- Uses `exec()` method with unique marker pattern
- No policy configured - uses default UserNatConfig
- Other tests using TCP NAT (like DNS lookup) pass, suggesting the issue may be specific to wget or the exec() method on KVM

### Possible Causes

1. Console `exec()` method broken on KVM (see `docs/console-automation-investigation.md`)
2. wget hanging or producing unexpected output
3. Marker pattern not being detected in console output

### Next Steps

1. Run test with `--nocapture` to see raw console output
2. Compare with macOS backend behavior
3. Check if the `exec()` marker is being written/detected correctly

---

## 3. ICMP NAT Not Implemented

**Status**: Open (by design, but should be documented)
**Severity**: Low
**Affects**: Any test using `ping` to external hosts

### Description

The NAT stack only handles TCP and UDP protocols. ICMP packets to external hosts are passed to the NAT but silently dropped because there's no ICMP handler.

### Evidence

From `crates/net/src/nat.rs`:
```rust
IpProtocol::Udp => self.handle_udp(guest_mac, &ip_packet).await,
IpProtocol::Tcp => self.handle_tcp(guest_mac, &ip_packet).await,
// ICMP is not handled
```

### Current Behavior

- Ping to gateway (10.0.2.2) works - handled by smoltcp
- Ping to external hosts (e.g., 8.8.8.8) silently fails - not NAT'd

### Workaround

Tests that need to verify connectivity to specific IPs should use DNS lookups (UDP) or TCP connections instead of ping:

```rust
// Instead of ping:
// console.exec("ping -c 1 8.8.8.8", ...).await?;

// Use DNS lookup:
console.exec("nslookup example.com 8.8.8.8", ...).await?;
```

### Resolution Options

1. **Document as limitation** - ICMP NAT is complex and may not be needed
2. **Implement ICMP NAT** - Would require:
   - ICMP echo request/reply handling in `nat.rs`
   - Tracking ICMP identifier for response routing
   - Host-side raw socket or ICMP socket support

---

## Related: Console Timing Fix

These issues were discovered while fixing console test flakiness. The original problem was that `wait_for()` patterns would match in command echoes, causing false positives.

**Solution implemented**: Added `VmConsole::exec()` method that uses unique markers (`__DONE_N__`) to reliably detect command completion. See `crates/capsa/src/console.rs`.
