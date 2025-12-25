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

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          UserNatStack                               │
│                                                                     │
│  ┌─────────────┐   ┌─────────────┐   ┌──────────────────────────┐  │
│  │ DHCP Server │   │  DNS Proxy  │   │     PolicyChecker        │  │
│  │             │   │             │   │                          │  │
│  │ Assigns:    │   │ Intercepts  │   │  ┌────────────────────┐  │  │
│  │ DNS=gateway │   │ UDP:53      │   │  │    DnsCache        │  │  │
│  └─────────────┘   │             │   │  │                    │  │  │
│                    │ Forwards to │   │  │ IP → (domain, ttl) │  │  │
│                    │ system DNS  │   │  └────────────────────┘  │  │
│                    │             │   │                          │  │
│                    │ Caches      │   │  check(packet, cache)    │  │
│                    │ responses   │───│  → Allow/Deny/Log        │  │
│                    └─────────────┘   └──────────────────────────┘  │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Flow: DNS Query

```
Guest                    DNS Proxy                 System DNS
  │                          │                          │
  │──── DNS query ──────────►│                          │
  │     "api.anthropic.com"  │                          │
  │                          │                          │
  │                          │──── Forward query ──────►│
  │                          │                          │
  │                          │◄─── Response: 1.2.3.4 ───│
  │                          │                          │
  │                          │ Cache: 1.2.3.4 →         │
  │                          │   ("api.anthropic.com",  │
  │                          │    TTL from response)    │
  │                          │                          │
  │◄─── Response: 1.2.3.4 ───│                          │
  │                          │                          │
```

### Flow: Connection Check

```
Guest connects to 1.2.3.4:443
          │
          ▼
┌─────────────────────────────────────────┐
│ PolicyChecker.check(packet_info, cache) │
└─────────────────────────────────────────┘
          │
          ▼
    ┌───────────┐
    │  Rule 1   │ Port(53) + Protocol(Udp) → No match
    └───────────┘
          │
          ▼
    ┌───────────┐
    │  Rule 2   │ Domain("api.anthropic.com")
    │           │   └─► cache.lookup(1.2.3.4)
    │           │   └─► Found: "api.anthropic.com"
    │           │   └─► Pattern matches → ALLOW
    └───────────┘
```

## API Design

### Existing Types (unchanged)

```rust
pub enum PolicyAction {
    Allow,
    Deny,
    Log,  // Allow but log
}

pub struct PolicyRule {
    pub action: PolicyAction,
    pub matcher: RuleMatcher,
}

pub struct NetworkPolicy {
    pub default_action: PolicyAction,
    pub rules: Vec<PolicyRule>,
}
```

### Enhanced RuleMatcher

```rust
pub enum RuleMatcher {
    Any,
    Ip(Ipv4Addr),
    IpRange { network: Ipv4Addr, prefix: u8 },
    Port(u16),
    PortRange { start: u16, end: u16 },
    Protocol(Protocol),
    Domain(DomainPattern),  // CHANGED: was Domain(String)
    All(Vec<RuleMatcher>),
}
```

### New: DomainPattern

```rust
/// Pattern for matching domain names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DomainPattern {
    /// Exact match: "api.anthropic.com"
    Exact(String),
    /// Wildcard match: "*.github.com" matches "api.github.com"
    Wildcard(String),
}

impl DomainPattern {
    /// Parse a pattern string into a DomainPattern.
    /// - "api.anthropic.com" → Exact
    /// - "*.github.com" → Wildcard
    pub fn parse(pattern: &str) -> Self {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            DomainPattern::Wildcard(suffix.to_lowercase())
        } else {
            DomainPattern::Exact(pattern.to_lowercase())
        }
    }

    /// Check if a domain matches this pattern.
    pub fn matches(&self, domain: &str) -> bool {
        let domain = domain.to_lowercase();
        match self {
            DomainPattern::Exact(expected) => domain == *expected,
            DomainPattern::Wildcard(suffix) => {
                // "*.github.com" matches "api.github.com", "raw.github.com"
                // Does NOT match "github.com" itself
                if domain.len() <= suffix.len() {
                    return false;
                }
                if !domain.ends_with(suffix) {
                    return false;
                }
                // Check for dot before suffix
                domain.as_bytes()[domain.len() - suffix.len() - 1] == b'.'
            }
        }
    }
}
```

### Builder Methods

```rust
impl NetworkPolicy {
    /// Add a rule to allow traffic to a domain.
    pub fn allow_domain(mut self, pattern: &str) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Allow,
            matcher: RuleMatcher::Domain(DomainPattern::parse(pattern)),
        });
        self
    }

    /// Add a rule to deny traffic to a domain.
    pub fn deny_domain(mut self, pattern: &str) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Deny,
            matcher: RuleMatcher::Domain(DomainPattern::parse(pattern)),
        });
        self
    }
}
```

## Usage Examples

### Example 1: AI Sandbox (Allowlist)

```rust
// Only allow specific API endpoints
let policy = NetworkPolicy::deny_all()
    .allow_dns()  // Required for domain resolution
    .allow_domain("api.anthropic.com")
    .allow_domain("api.openai.com")
    .allow_domain("*.github.com");

let vm = VmBuilder::new()
    .network(NetworkMode::user_nat().policy(policy).build())
    .build().await?;
```

**Behavior:**
- `api.anthropic.com:443` → ✅ Allowed
- `api.anthropic.com:80` → ✅ Allowed (all ports)
- `evil.com:443` → ❌ Denied (not in allowlist)
- `api.github.com:22` → ✅ Allowed (wildcard match)

### Example 2: HTTPS-Only Sandbox

```rust
// Only allow specific domains on HTTPS
let policy = NetworkPolicy::deny_all()
    .allow_dns()
    .rule(PolicyAction::Allow, RuleMatcher::All(vec![
        RuleMatcher::Domain(DomainPattern::parse("api.anthropic.com")),
        RuleMatcher::Port(443),
    ]))
    .rule(PolicyAction::Allow, RuleMatcher::All(vec![
        RuleMatcher::Domain(DomainPattern::parse("*.github.com")),
        RuleMatcher::Port(443),
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
    .allow_dns()
    .allow_ip_range("10.0.0.0", 8)     // Internal network
    .allow_ip(Ipv4Addr::new(192, 168, 1, 100))  // Specific server
    .allow_domain("api.anthropic.com")  // External API
    .allow_domain("*.amazonaws.com");   // AWS services
```

### Example 5: Logging Suspicious Traffic

```rust
let policy = NetworkPolicy::allow_all()
    .rule(PolicyAction::Deny, RuleMatcher::Domain(DomainPattern::parse("*.malware.example")))
    .rule(PolicyAction::Log, RuleMatcher::Domain(DomainPattern::parse("*.ru")))
    .rule(PolicyAction::Log, RuleMatcher::PortRange { start: 6660, end: 6669 });
```

## Implementation Plan

### Phase 1: Core Types (capsa-core)

**File: `crates/core/src/types/network.rs`**

1. Add `DomainPattern` enum with `Exact` and `Wildcard` variants
2. Change `RuleMatcher::Domain(String)` to `RuleMatcher::Domain(DomainPattern)`
3. Add `DomainPattern::parse()` and `DomainPattern::matches()`
4. Add builder methods: `allow_domain()`, `deny_domain()`
5. Add serialization support for `DomainPattern`

**Estimated changes:** ~80 lines

### Phase 2: DNS Cache (capsa-net)

**New file: `crates/net/src/dns_cache.rs`**

```rust
pub struct DnsCache {
    entries: HashMap<Ipv4Addr, CacheEntry>,
}

struct CacheEntry {
    domain: String,
    expires: Instant,
}

impl DnsCache {
    pub fn new() -> Self;
    pub fn insert(&mut self, ip: Ipv4Addr, domain: String, ttl: Duration);
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<&str>;
    pub fn cleanup(&mut self);
}
```

**Estimated changes:** ~60 lines

### Phase 3: DNS Proxy (capsa-net)

**New file: `crates/net/src/dns_proxy.rs`**

```rust
pub struct DnsProxy {
    cache: Arc<RwLock<DnsCache>>,
    upstream: SocketAddr,  // System DNS or 8.8.8.8
}

impl DnsProxy {
    pub fn new(cache: Arc<RwLock<DnsCache>>) -> Self;

    /// Handle a DNS query from guest.
    /// Returns the response to send back.
    pub async fn handle_query(&self, query: &[u8]) -> Result<Vec<u8>, DnsError>;
}
```

DNS parsing will use the `dns-parser` crate or manual parsing of the simple cases (A records).

**Estimated changes:** ~150 lines

### Phase 4: Policy Checker Updates (capsa-net)

**File: `crates/net/src/policy.rs`**

1. Update `compile_matcher()` to handle `DomainPattern`
2. Add `CompiledMatcher::Domain(DomainPattern)`
3. Update `CompiledMatcher::matches()` to accept DNS cache reference
4. Change signature: `check(&self, info: &PacketInfo, cache: &DnsCache) -> PolicyResult`

**Key change:**
```rust
impl CompiledMatcher {
    fn matches(&self, info: &PacketInfo, dns_cache: &DnsCache) -> bool {
        match self {
            // ... existing matchers ...
            CompiledMatcher::Domain(pattern) => {
                // Lookup the destination IP in DNS cache
                if let Some(domain) = dns_cache.lookup(info.dst_ip) {
                    pattern.matches(domain)
                } else {
                    false  // Unknown IP, no domain match
                }
            }
            CompiledMatcher::All(matchers) => {
                matchers.iter().all(|m| m.matches(info, dns_cache))
            }
        }
    }
}
```

**Estimated changes:** ~50 lines

### Phase 5: Stack Integration (capsa-net)

**File: `crates/net/src/stack.rs`**

1. Add `DnsCache` field to `UserNatStack`
2. Add `DnsProxy` field to `UserNatStack`
3. Intercept DNS packets (UDP port 53) in the main loop
4. Route DNS through `DnsProxy` instead of NAT
5. Pass DNS cache to policy checker

**Key changes to main loop:**
```rust
// In UserNatStack::run()
loop {
    // ... existing code ...

    // Check for DNS query
    if let Some(frame) = self.device.peek_rx() {
        if self.is_dns_query(frame) {
            let frame_copy = frame.to_vec();
            self.device.discard_rx();

            // Handle via DNS proxy (updates cache automatically)
            if let Some(response) = self.dns_proxy.handle_query(&frame_copy).await {
                self.device.send_frame(&response)?;
            }
            continue;
        }
    }

    // Existing policy check now includes cache
    if let Some(ref checker) = self.policy_checker
        && let Some(info) = PolicyChecker::extract_packet_info(frame)
    {
        match checker.check(&info, &self.dns_cache) {
            // ... existing handling ...
        }
    }
}
```

**Estimated changes:** ~100 lines

### Phase 6: Tests

**File: `crates/net/src/dns_cache.rs`** (unit tests)
- Test cache insertion and lookup
- Test TTL expiration
- Test cleanup

**File: `crates/net/src/policy.rs`** (unit tests)
- Test domain pattern matching (exact, wildcard)
- Test policy evaluation with mock DNS cache

**File: `crates/capsa/tests/network_test.rs`** (integration tests)
- Test domain allowlist (guest can reach allowed domains)
- Test domain denylist (guest cannot reach denied domains)
- Test wildcard matching
- Test combined Domain + Port rules

**Estimated changes:** ~200 lines

## Changes to Existing Infrastructure

### 1. DHCP Server (minimal change)

The DHCP server already assigns our gateway IP as the DNS server. No changes needed - guests already send DNS queries to us.

**Verify in:** `crates/net/src/dhcp.rs`

### 2. PolicyChecker Signature Change

Current:
```rust
pub fn check(&self, info: &PacketInfo) -> PolicyResult
```

New:
```rust
pub fn check(&self, info: &PacketInfo, dns_cache: &DnsCache) -> PolicyResult
```

This is a breaking change to the internal API. All call sites in `stack.rs` need updating.

### 3. RuleMatcher Serialization

`RuleMatcher::Domain` currently holds a `String`. Changing to `DomainPattern` requires updating serialization:

```rust
// Old
Domain(String)

// New
Domain(DomainPattern)

// DomainPattern serializes as:
// - "api.anthropic.com" for Exact
// - "*.github.com" for Wildcard
```

For backwards compatibility, we can deserialize a plain string as `DomainPattern::Exact`.

### 4. NAT Table (no change)

The NAT table handles TCP/UDP connection tracking. DNS queries will be intercepted *before* reaching NAT, so no changes needed.

## Dependencies

### New Crate: `dns-parser` (optional)

For parsing DNS packets. Alternatively, we can do minimal manual parsing since we only need:
- Query: extract domain name
- Response: extract A record IPs and TTL

Manual parsing is ~100 lines and avoids a dependency.

**Recommendation:** Start with manual parsing, add `dns-parser` if we need more features.

## Security Considerations

### 1. DNS Cache Poisoning

**Risk:** Attacker sends fake DNS responses to populate cache with wrong IPs.

**Mitigation:** We only accept DNS responses that match pending queries. The guest can't inject arbitrary cache entries.

### 2. DNS Bypass via Hardcoded IPs

**Risk:** Guest code uses hardcoded IPs instead of DNS.

**Mitigation:** With `deny_all()` + domain allowlist, hardcoded IPs won't be in the DNS cache and will be denied. This is the intended behavior.

### 3. DNS over HTTPS (DoH)

**Risk:** Guest uses DoH (port 443) to bypass our DNS proxy.

**Mitigation:** If policy is `deny_all()` + specific domains, DoH servers won't be in the allowlist. If policy is more permissive, this is a known limitation.

### 4. Cache Memory Growth

**Risk:** Unbounded cache growth from many DNS queries.

**Mitigation:**
- Respect TTL from DNS responses
- Periodic cleanup of expired entries (already done for NAT table)
- Optional: cap maximum entries

## Future Enhancements

1. **Regex patterns:** `DomainPattern::Regex(regex::Regex)` for complex matching
2. **SNI verification:** Double-check TLS connections against DNS cache
3. **DNS query logging:** Log all DNS queries for auditing
4. **Custom upstream DNS:** Configure DNS server instead of using system default
5. **Negative caching:** Remember denied domains to avoid repeated lookups

## Summary

| Component | File | Changes |
|-----------|------|---------|
| DomainPattern type | `capsa-core/types/network.rs` | ~80 lines |
| DNS Cache | `capsa-net/dns_cache.rs` | ~60 lines (new) |
| DNS Proxy | `capsa-net/dns_proxy.rs` | ~150 lines (new) |
| Policy updates | `capsa-net/policy.rs` | ~50 lines |
| Stack integration | `capsa-net/stack.rs` | ~100 lines |
| Tests | various | ~200 lines |
| **Total** | | **~640 lines** |

The implementation builds on our existing nftables-like policy model. The key additions are DNS interception and a cache that bridges domain names to IPs for policy evaluation.
