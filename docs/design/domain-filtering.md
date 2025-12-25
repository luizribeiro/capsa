# Domain-Based Network Filtering

## Overview

This document describes the design for domain-based network filtering in capsa's userspace NAT stack. The feature allows policies to reference domain names (e.g., `api.anthropic.com`, `*.github.com`) rather than just IP addresses.

## Motivation

For sandboxing AI-generated code, we need to restrict network access to specific API endpoints:
- Allow `api.anthropic.com` but nothing else
- Allow `*.github.com` for repository access
- Block everything by default

IP-based filtering is insufficient because:
- API endpoints use dynamic IPs (CDNs, load balancers)
- Users shouldn't need to maintain IP allowlists
- Domain names are the natural unit of trust

## Design Principles

1. **nftables-like model**: Rules evaluated sequentially, first match wins, default action if no match
2. **Composable matchers**: Combine Domain + Port + Protocol with `All(...)`
3. **DNS as source of truth**: We control the guest's DNS, so we control what IPs domains resolve to
4. **Simple wildcards**: Support `*.example.com`, keep matching logic isolated for future expansion
5. **Log continues evaluation**: `Log` action logs and continues to next rule (not terminal)

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          UserNatStack                               │
│                                                                     │
│  ┌─────────────┐   ┌─────────────┐   ┌──────────────────────────┐  │
│  │ DHCP Server │   │  DNS Proxy  │   │     PolicyChecker        │  │
│  │             │   │             │   │                          │  │
│  │ Assigns:    │   │ Intercepts  │   │  dns_cache: &DnsCache    │  │
│  │ DNS=gateway │   │ UDP:53      │   │                          │  │
│  └─────────────┘   │             │   │  check(packet)           │  │
│                    │ Forwards to │   │  → Allow/Deny/Log        │  │
│                    │ system DNS  │   └──────────────────────────┘  │
│                    │             │               ▲                  │
│                    │ Caches A/   │               │                  │
│                    │ AAAA records│───────────────┘                  │
│                    └─────────────┘   shared DnsCache                │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### DNS Handling

DNS queries to the gateway (UDP port 53) are handled **internally** by the DNS Proxy, **not** subject to policy evaluation. This means:

- No need for `allow_dns()` rules
- DNS resolution "just works" for domain-based policies
- Policy only applies to actual connections (TCP/UDP to external hosts)

### Flow: DNS Query

```
Guest                    DNS Proxy                 System DNS
  │                          │                          │
  │──── DNS query ──────────►│                          │
  │     "api.anthropic.com"  │                          │
  │     (to gateway:53)      │                          │
  │                          │──── Forward query ──────►│
  │                          │                          │
  │                          │◄─── Response: 1.2.3.4 ───│
  │                          │     (A record, TTL=300)  │
  │                          │                          │
  │                          │ Cache: 1.2.3.4 →         │
  │                          │   ("api.anthropic.com",  │
  │                          │    expires in 300s)      │
  │                          │                          │
  │◄─── Response: 1.2.3.4 ───│                          │
```

**DNS Record Type Handling:**

| Record Type | Action |
|-------------|--------|
| A (IPv4) | Cache IP → domain mapping |
| AAAA (IPv6) | Cache IP → domain mapping (if IPv6 supported) |
| CNAME, MX, TXT, etc. | Pass through without caching |

### Flow: Connection Check

```
Guest connects to 1.2.3.4:443
          │
          ▼
┌─────────────────────────────────────────┐
│ PolicyChecker.check(packet_info)        │
│   (has reference to DnsCache)           │
└─────────────────────────────────────────┘
          │
          ▼
    ┌───────────┐
    │  Rule 1   │ Log + Any → LOG, continue ──┐
    └───────────┘                             │
          │                                   │
          ▼                                   │
    ┌───────────┐                             │
    │  Rule 2   │ Domain("api.anthropic.com") │
    │           │   └─► cache.lookup(1.2.3.4) │
    │           │   └─► Found: "api.anthropic.com"
    │           │   └─► Pattern matches → ALLOW
    └───────────┘
```

## PolicyAction Semantics

```rust
pub enum PolicyAction {
    /// Allow the traffic (terminal - stops evaluation)
    Allow,
    /// Deny the traffic (terminal - stops evaluation)
    Deny,
    /// Log and continue to next rule (non-terminal)
    Log,
}
```

**Key insight:** `Log` is **not terminal**. It logs the packet and continues evaluating subsequent rules. This allows adding logging to any policy without changing its behavior:

```rust
// Log all traffic, then apply normal rules
let policy = NetworkPolicy::deny_all()
    .rule(Log, Any)  // Log everything, continue to next rule
    .allow_domain("api.anthropic.com")
    .allow_domain("*.github.com");
```

## API Design

### RuleMatcher

```rust
pub enum RuleMatcher {
    /// Match any traffic (always true)
    Any,
    /// Match traffic to specific IP address
    Ip(Ipv4Addr),
    /// Match traffic to IP range (CIDR notation)
    IpRange { network: Ipv4Addr, prefix: u8 },
    /// Match traffic to specific destination port
    Port(u16),
    /// Match traffic to port range (inclusive)
    PortRange { start: u16, end: u16 },
    /// Match traffic by protocol
    Protocol(Protocol),
    /// Match if destination IP was resolved from matching domain
    Domain(DomainPattern),
    /// All matchers must match (AND logic)
    /// Note: empty All([]) matches everything (vacuous truth)
    All(Vec<RuleMatcher>),
}
```

**Important:** `All([])` (empty list) matches everything because an empty AND is vacuously true. This can be used for "match all" rules:
- `rule(Log, All([]))` - log everything
- `rule(Log, Any)` - equivalent, more readable

### DomainPattern

```rust
/// Pattern for matching domain names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DomainPattern {
    /// Exact match: "api.anthropic.com"
    Exact(String),
    /// Wildcard match: "*.github.com" matches "api.github.com"
    /// Does NOT match "github.com" itself (must have subdomain)
    Wildcard(String),
}

impl DomainPattern {
    /// Parse a pattern string into a DomainPattern.
    pub fn parse(pattern: &str) -> Self;

    /// Check if a domain matches this pattern.
    pub fn matches(&self, domain: &str) -> bool;
}
```

The `matches()` function is isolated for easy future expansion (regex, etc.).

### Builder Methods

```rust
impl NetworkPolicy {
    /// Add a rule to allow traffic to a domain (all ports).
    /// Equivalent to: .rule(Allow, Domain(pattern))
    pub fn allow_domain(self, pattern: &str) -> Self;

    /// Add a rule to deny traffic to a domain (all ports).
    /// Equivalent to: .rule(Deny, Domain(pattern))
    pub fn deny_domain(self, pattern: &str) -> Self;

    /// Add a rule to log traffic to a domain (continues evaluation).
    /// Equivalent to: .rule(Log, Domain(pattern))
    pub fn log_domain(self, pattern: &str) -> Self;
}
```

**Note:** `allow_domain("api.anthropic.com")` allows **all ports** to that domain. To restrict ports, use `All`:

```rust
// Allow only HTTPS to anthropic
.rule(Allow, All(vec![
    Domain(DomainPattern::parse("api.anthropic.com")),
    Port(443),
]))
```

## Usage Examples

### Example 1: AI Sandbox (Allowlist)

```rust
// Only allow specific API endpoints (all ports)
let policy = NetworkPolicy::deny_all()
    .allow_domain("api.anthropic.com")
    .allow_domain("api.openai.com")
    .allow_domain("*.github.com");
```

**Behavior:**
- `api.anthropic.com:443` → ✅ Allowed
- `api.anthropic.com:80` → ✅ Allowed (all ports)
- `evil.com:443` → ❌ Denied (not in allowlist)
- `api.github.com:22` → ✅ Allowed (wildcard match)
- DNS queries → ✅ Handled internally (not subject to policy)

### Example 2: HTTPS-Only Sandbox

```rust
// Only allow specific domains on HTTPS
let policy = NetworkPolicy::deny_all()
    .rule(Allow, All(vec![
        Domain(DomainPattern::parse("api.anthropic.com")),
        Port(443),
    ]))
    .rule(Allow, All(vec![
        Domain(DomainPattern::parse("*.github.com")),
        Port(443),
    ]));
```

**Behavior:**
- `api.anthropic.com:443` → ✅ Allowed
- `api.anthropic.com:80` → ❌ Denied (port 443 required)
- `api.github.com:443` → ✅ Allowed
- `api.github.com:22` → ❌ Denied (port 443 required)

### Example 3: Denylist Mode

```rust
// Allow everything except specific domains
let policy = NetworkPolicy::allow_all()
    .deny_domain("*.facebook.com")
    .deny_domain("*.tiktok.com")
    .deny_domain("malware.example.com");
```

### Example 4: Mixed IPs and Domains

```rust
// Internal services by IP, external by domain
let policy = NetworkPolicy::deny_all()
    .allow_ip_range("10.0.0.0", 8)           // Internal network
    .allow_ip(Ipv4Addr::new(192, 168, 1, 100))  // Specific server
    .allow_domain("api.anthropic.com")       // External API
    .allow_domain("*.amazonaws.com");        // AWS services
```

### Example 5: Log Everything

```rust
// Log all traffic for debugging
let policy = NetworkPolicy::deny_all()
    .rule(Log, Any)  // Log all, continue to next rule
    .allow_domain("api.anthropic.com");

// Or with empty All (equivalent)
let policy = NetworkPolicy::deny_all()
    .rule(Log, All(vec![]))  // Empty AND = always true
    .allow_domain("api.anthropic.com");
```

### Example 6: Selective Logging

```rust
// Log suspicious patterns, allow normal traffic
let policy = NetworkPolicy::allow_all()
    .deny_domain("*.malware.example")        // Block bad domains
    .log_domain("*.ru")                      // Log .ru, continue
    .rule(Log, PortRange { start: 6660, end: 6669 });  // Log IRC ports
```

## Implementation Plan

### Phase 1: Core Types (capsa-core)

**File: `crates/core/src/types/network.rs`**

Changes:
1. Add `DomainPattern` enum with `Exact` and `Wildcard` variants
2. Change `RuleMatcher::Domain(String)` to `RuleMatcher::Domain(DomainPattern)`
3. Implement `DomainPattern::parse()` and `DomainPattern::matches()`
4. Add builder methods: `allow_domain()`, `deny_domain()`, `log_domain()`
5. Add serialization support (plain string deserializes as Exact)

Unit tests (in same file):
- Test `DomainPattern::parse()` for exact and wildcard patterns
- Test `DomainPattern::matches()` for various cases
- Test serialization/deserialization

**Estimated changes:** ~100 lines (including tests)

### Phase 2: DNS Cache (capsa-net)

**New file: `crates/net/src/dns_cache.rs`**

```rust
pub struct DnsCache {
    entries: HashMap<Ipv4Addr, CacheEntry>,
    max_entries: usize,  // Cap to prevent unbounded growth
}

struct CacheEntry {
    domain: String,
    expires: Instant,
}

impl DnsCache {
    pub fn new(max_entries: usize) -> Self;
    pub fn insert(&mut self, ip: Ipv4Addr, domain: String, ttl: Duration);
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<&str>;
    pub fn cleanup(&mut self);  // Remove expired entries
}
```

When `max_entries` is reached, evict oldest entries (LRU-style).

Unit tests (in same file):
- Test insertion and lookup
- Test TTL expiration
- Test max entries eviction
- Test cleanup

**Estimated changes:** ~80 lines (including tests)

### Phase 3: DNS Proxy (capsa-net)

**New file: `crates/net/src/dns_proxy.rs`**

```rust
pub struct DnsProxy {
    cache: Arc<RwLock<DnsCache>>,
}

impl DnsProxy {
    pub fn new(cache: Arc<RwLock<DnsCache>>) -> Self;

    /// Handle a DNS query from guest.
    /// Forwards to system DNS, caches A/AAAA responses.
    pub async fn handle_query(&self, query: &[u8]) -> Result<Vec<u8>, DnsError>;
}
```

Uses `dns-parser` crate for parsing DNS packets.

System DNS resolution:
- Use tokio's DNS resolver which reads system `/etc/resolv.conf`
- Or use `trust-dns-resolver` for more control

Unit tests (in same file):
- Test query parsing
- Test response handling
- Test A record caching
- Test non-A record passthrough

**Estimated changes:** ~180 lines (including tests)

**New dependency:** `dns-parser` in `crates/net/Cargo.toml`

### Phase 4: Policy Checker Updates (capsa-net)

**File: `crates/net/src/policy.rs`**

Changes:
1. `PolicyChecker` holds `Arc<RwLock<DnsCache>>` reference
2. Update constructor: `PolicyChecker::new(..., dns_cache: Arc<RwLock<DnsCache>>)`
3. Update `compile_matcher()` to handle `DomainPattern`
4. Add `CompiledMatcher::Domain(DomainPattern)`
5. Update `matches()` to use DNS cache for domain lookups
6. **Change `Log` behavior**: return `Log` but caller continues evaluation

```rust
pub struct PolicyChecker {
    default_action: PolicyResult,
    rules: Vec<CompiledRule>,
    dns_cache: Arc<RwLock<DnsCache>>,  // NEW
}

impl PolicyChecker {
    pub fn new(
        default_action: PolicyAction,
        rules: &[PolicyRule],
        dns_cache: Arc<RwLock<DnsCache>>,  // NEW
    ) -> Self;

    /// Check a packet against the policy.
    /// For Log actions, logs and continues to next rule.
    pub fn check(&self, info: &PacketInfo) -> PolicyResult;
}

impl CompiledMatcher {
    fn matches(&self, info: &PacketInfo, dns_cache: &DnsCache) -> bool {
        match self {
            // ... existing matchers ...
            CompiledMatcher::Domain(pattern) => {
                if let Some(domain) = dns_cache.lookup(info.dst_ip) {
                    pattern.matches(domain)
                } else {
                    false  // Unknown IP = no domain match
                }
            }
            CompiledMatcher::All(matchers) => {
                // Empty All = always true (vacuous truth)
                matchers.iter().all(|m| m.matches(info, dns_cache))
            }
        }
    }
}
```

Unit tests (in same file):
- Test domain matching with mock cache
- Test `All([])` matches everything
- Test `Log` action continues evaluation

**Estimated changes:** ~80 lines (including tests)

### Phase 5: Stack Integration (capsa-net)

**File: `crates/net/src/stack.rs`**

Changes:
1. Add `dns_cache: Arc<RwLock<DnsCache>>` field to `UserNatStack`
2. Add `dns_proxy: DnsProxy` field to `UserNatStack`
3. Create shared cache in `UserNatStack::new()`
4. Pass cache to `PolicyChecker::new()`
5. Intercept DNS packets (UDP port 53 to gateway) in main loop
6. Route DNS through `DnsProxy` instead of NAT
7. Add periodic DNS cache cleanup (alongside NAT cleanup)

```rust
pub struct UserNatStack<F: FrameIO> {
    // ... existing fields ...
    dns_cache: Arc<RwLock<DnsCache>>,
    dns_proxy: DnsProxy,
}

impl<F: FrameIO> UserNatStack<F> {
    pub fn new(frame_io: F, config: StackConfig) -> Self {
        // ...
        let dns_cache = Arc::new(RwLock::new(DnsCache::new(1000)));  // Max 1000 entries
        let dns_proxy = DnsProxy::new(dns_cache.clone());
        let policy_checker = config.policy.as_ref().map(|p|
            PolicyChecker::new(p.default_action, &p.rules, dns_cache.clone())
        );
        // ...
    }
}
```

Main loop changes:
```rust
// In run() loop, before external destination check:
if let Some(frame) = self.device.peek_rx() {
    if self.is_dns_query_to_gateway(frame) {
        let frame_copy = frame.to_vec();
        self.device.discard_rx();

        if let Ok(response) = self.dns_proxy.handle_query(&frame_copy).await {
            self.device.send_frame(&response)?;
        }
        continue;
    }
}

// DNS cache cleanup alongside NAT cleanup
if cleanup_counter.is_multiple_of(NAT_CLEANUP_INTERVAL_MS) {
    self.nat.cleanup();
    self.dns_cache.write().unwrap().cleanup();
}
```

**Estimated changes:** ~60 lines

### Phase 6: Integration Tests

**File: `crates/capsa/tests/network_test.rs`**

New integration tests:
- `test_domain_allowlist`: Guest can reach allowed domains
- `test_domain_denylist`: Guest cannot reach denied domains
- `test_domain_wildcard`: Wildcard patterns work correctly
- `test_domain_with_port_restriction`: `All(Domain, Port)` works
- `test_domain_logging`: Log action logs but allows if subsequent rule matches
- `test_domain_mixed_ip_and_domain`: Both IP and domain rules in same policy

**Estimated changes:** ~200 lines

## Changes to Existing Infrastructure

### 1. DHCP Server

No changes needed. Already assigns gateway IP as DNS server.

### 2. PolicyChecker Constructor

**Breaking change** to internal API:

```rust
// Old
PolicyChecker::new(default_action, rules)

// New
PolicyChecker::new(default_action, rules, dns_cache)
```

Update call site in `stack.rs`.

### 3. PolicyChecker::check() Return Handling

Current code treats any non-Allow result as terminal. Need to update for `Log`:

```rust
// Old
match checker.check(&info) {
    PolicyResult::Deny => { /* deny */ }
    PolicyResult::Log => { /* log, allow */ }
    PolicyResult::Allow => {}
}

// New: Log is handled inside check(), which continues evaluation
// check() only returns Allow or Deny as final result
```

Actually, cleaner approach: `check()` handles Log internally and returns only `Allow` or `Deny` as the final decision. Logging happens as a side effect during evaluation.

### 4. RuleMatcher Serialization

For backwards compatibility, deserialize plain string as `DomainPattern::Exact`:

```rust
// These should both work:
"domain": "api.anthropic.com"      // Exact
"domain": "*.github.com"           // Wildcard (detected by prefix)
```

### 5. NAT Table

No changes. DNS is intercepted before NAT processing.

## Dependencies

### New Crate: `dns-parser`

Add to `crates/net/Cargo.toml`:

```toml
[dependencies]
dns-parser = "0.8"
```

This crate provides:
- DNS packet parsing (query and response)
- Record type handling (A, AAAA, CNAME, etc.)
- Well-tested, handles edge cases

## Security Considerations

### 1. DNS Cache Poisoning

**Risk:** Attacker sends fake DNS responses.

**Mitigation:** We only accept responses from the upstream DNS server that match pending queries. Guest cannot inject cache entries.

### 2. DNS Bypass via Hardcoded IPs

**Risk:** Guest uses hardcoded IPs instead of DNS.

**Mitigation:** With `deny_all()` + domain allowlist, hardcoded IPs won't be in the DNS cache and will be denied. This is intended behavior.

### 3. DNS over HTTPS (DoH)

**Risk:** Guest uses DoH to bypass our DNS proxy.

**Mitigation:** DoH servers (e.g., `dns.google`) won't be in the domain allowlist, so DoH traffic will be blocked.

### 4. Cache Memory Growth

**Risk:** Unbounded cache growth.

**Mitigation:**
- Cap at `max_entries` (default 1000)
- LRU eviction when cap reached
- Periodic cleanup of expired entries

## Future Enhancements

1. **Regex patterns:** `DomainPattern::Regex` for complex matching
2. **SNI verification:** Double-check TLS connections against DNS cache
3. **DNS query logging:** Log all DNS queries for auditing
4. **Negative caching:** Cache denied domains to avoid repeated lookups
5. **IPv6 support:** Cache AAAA records when IPv6 is supported

## Summary

| Phase | Component | File | Estimated |
|-------|-----------|------|-----------|
| 1 | DomainPattern type + tests | `capsa-core/.../network.rs` | ~100 lines |
| 2 | DNS Cache + tests | `capsa-net/dns_cache.rs` | ~80 lines |
| 3 | DNS Proxy + tests | `capsa-net/dns_proxy.rs` | ~180 lines |
| 4 | Policy updates + tests | `capsa-net/policy.rs` | ~80 lines |
| 5 | Stack integration | `capsa-net/stack.rs` | ~60 lines |
| 6 | Integration tests | `capsa/tests/network_test.rs` | ~200 lines |
| **Total** | | | **~700 lines** |

The implementation builds on our existing nftables-like policy model. Key additions:
- DNS proxy intercepts queries (no `allow_dns()` needed)
- DNS cache bridges domain names to IPs for policy evaluation
- `Log` action is non-terminal (logs and continues)
- `All([])` matches everything (vacuous truth)
- Cache has max entries cap with LRU eviction
