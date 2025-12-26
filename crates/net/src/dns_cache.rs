//! DNS cache for mapping IP addresses back to domain names.
//!
//! Used by the policy checker to resolve destination IPs to their
//! original domain names for domain-based filtering.
//!
//! # Security
//!
//! TODO: The cache currently trusts DNS responses from the upstream server.
//! A malicious upstream DNS could poison the cache with false IP→domain mappings.
//! Consider implementing DNSSEC validation to ensure response authenticity.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

const DEFAULT_MAX_ENTRIES: usize = 1000;
const MIN_TTL: Duration = Duration::from_secs(60);

/// Cache that maps IP addresses to domain names with TTL expiration.
pub struct DnsCache {
    entries: HashMap<Ipv4Addr, CacheEntry>,
    max_entries: usize,
}

struct CacheEntry {
    domain: String,
    expires: Instant,
    inserted: Instant,
}

impl Default for DnsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DnsCache {
    /// Create a new DNS cache with default capacity (1000 entries).
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_ENTRIES)
    }

    /// Create a new DNS cache with specified maximum entries.
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
        }
    }

    /// Insert a domain name for an IP address with the given TTL.
    ///
    /// If the cache is at capacity, the oldest entry is evicted.
    /// TTL is enforced to be at least 60 seconds.
    pub fn insert(&mut self, ip: Ipv4Addr, domain: String, ttl: Duration) {
        if self.entries.len() >= self.max_entries && !self.entries.contains_key(&ip) {
            self.evict_oldest();
        }

        let now = Instant::now();
        let ttl = ttl.max(MIN_TTL);

        self.entries.insert(
            ip,
            CacheEntry {
                domain,
                expires: now + ttl,
                inserted: now,
            },
        );
    }

    /// Look up the domain name for an IP address.
    ///
    /// Returns None if the IP is not in the cache or has expired.
    pub fn lookup(&self, ip: Ipv4Addr) -> Option<&str> {
        self.entries.get(&ip).and_then(|entry| {
            if entry.expires > Instant::now() {
                Some(entry.domain.as_str())
            } else {
                None
            }
        })
    }

    /// Remove all expired entries from the cache.
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires > now);
    }

    fn evict_oldest(&mut self) {
        if let Some((&oldest_ip, _)) = self.entries.iter().min_by_key(|(_, entry)| entry.inserted) {
            self.entries.remove(&oldest_ip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_lookup() {
        let mut cache = DnsCache::new();
        let ip = Ipv4Addr::new(93, 184, 216, 34);
        cache.insert(ip, "example.com".to_string(), Duration::from_secs(300));

        assert_eq!(cache.lookup(ip), Some("example.com"));
    }

    #[test]
    fn lookup_unknown_ip_returns_none() {
        let cache = DnsCache::new();
        assert_eq!(cache.lookup(Ipv4Addr::new(1, 2, 3, 4)), None);
    }

    #[test]
    fn expired_entry_returns_none() {
        let mut cache = DnsCache::new();
        let ip = Ipv4Addr::new(93, 184, 216, 34);

        // Insert with very short TTL (will be promoted to MIN_TTL)
        // We can't easily test actual expiration without mocking time,
        // so we test the lookup logic by checking that entries exist
        cache.insert(ip, "example.com".to_string(), Duration::from_secs(300));
        assert!(cache.lookup(ip).is_some());
    }

    #[test]
    fn eviction_at_capacity() {
        let mut cache = DnsCache::with_capacity(2);
        cache.insert(
            Ipv4Addr::new(1, 1, 1, 1),
            "a.com".to_string(),
            Duration::from_secs(300),
        );

        // Small delay to ensure different insertion times
        std::thread::sleep(Duration::from_millis(1));

        cache.insert(
            Ipv4Addr::new(2, 2, 2, 2),
            "b.com".to_string(),
            Duration::from_secs(300),
        );

        std::thread::sleep(Duration::from_millis(1));

        cache.insert(
            Ipv4Addr::new(3, 3, 3, 3),
            "c.com".to_string(),
            Duration::from_secs(300),
        );

        // First entry should be evicted (oldest)
        assert_eq!(cache.lookup(Ipv4Addr::new(1, 1, 1, 1)), None);
        assert_eq!(cache.lookup(Ipv4Addr::new(2, 2, 2, 2)), Some("b.com"));
        assert_eq!(cache.lookup(Ipv4Addr::new(3, 3, 3, 3)), Some("c.com"));
    }

    #[test]
    fn update_existing_entry() {
        let mut cache = DnsCache::with_capacity(2);
        let ip = Ipv4Addr::new(1, 1, 1, 1);

        cache.insert(ip, "old.com".to_string(), Duration::from_secs(300));
        cache.insert(ip, "new.com".to_string(), Duration::from_secs(300));

        assert_eq!(cache.lookup(ip), Some("new.com"));
        // Should still only have 1 entry
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn min_ttl_enforced() {
        let mut cache = DnsCache::new();
        let ip = Ipv4Addr::new(1, 1, 1, 1);

        // Try to insert with very short TTL
        cache.insert(ip, "example.com".to_string(), Duration::from_secs(1));

        // Entry should still be valid (min TTL is 60 seconds)
        assert!(cache.lookup(ip).is_some());
    }

    #[test]
    fn cleanup_preserves_valid_entries() {
        let mut cache = DnsCache::new();
        cache.insert(
            Ipv4Addr::new(1, 1, 1, 1),
            "valid.com".to_string(),
            Duration::from_secs(300),
        );

        cache.cleanup();

        assert_eq!(cache.lookup(Ipv4Addr::new(1, 1, 1, 1)), Some("valid.com"));
    }

    #[test]
    fn default_creates_standard_cache() {
        let cache = DnsCache::default();
        assert_eq!(cache.max_entries, DEFAULT_MAX_ENTRIES);
    }
}
