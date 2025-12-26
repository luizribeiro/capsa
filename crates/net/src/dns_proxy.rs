//! DNS proxy for intercepting and caching DNS queries.
//!
//! Forwards DNS queries from the guest to system DNS servers,
//! caches A/AAAA record responses for domain-based filtering.

use crate::dns_cache::DnsCache;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::UdpSocket;

const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_DNS: &str = "8.8.8.8:53";

/// DNS proxy that forwards queries and caches responses.
pub struct DnsProxy {
    cache: Arc<RwLock<DnsCache>>,
}

/// Errors that can occur during DNS proxy operations.
#[derive(Debug)]
pub enum DnsError {
    /// Failed to parse DNS packet
    ParseError,
    /// Network I/O error
    IoError(std::io::Error),
    /// Query timed out
    Timeout,
}

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsError::ParseError => write!(f, "failed to parse DNS packet"),
            DnsError::IoError(e) => write!(f, "DNS I/O error: {}", e),
            DnsError::Timeout => write!(f, "DNS query timed out"),
        }
    }
}

impl std::error::Error for DnsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DnsError::IoError(e) => Some(e),
            _ => None,
        }
    }
}

impl DnsProxy {
    /// Create a new DNS proxy with the given cache.
    pub fn new(cache: Arc<RwLock<DnsCache>>) -> Self {
        Self { cache }
    }

    /// Handle a DNS query from the guest.
    ///
    /// Forwards the query to upstream DNS, caches A records from the response,
    /// and returns the response bytes to send back to the guest.
    pub async fn handle_query(&self, query_bytes: &[u8]) -> Result<Vec<u8>, DnsError> {
        // Validate it's a parseable DNS query
        dns_parser::Packet::parse(query_bytes).map_err(|_| DnsError::ParseError)?;

        // Forward to upstream DNS
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(DnsError::IoError)?;

        socket
            .send_to(query_bytes, UPSTREAM_DNS)
            .await
            .map_err(DnsError::IoError)?;

        // Receive response with timeout
        let mut response_buf = vec![0u8; 512];
        let len = tokio::time::timeout(DNS_TIMEOUT, socket.recv(&mut response_buf))
            .await
            .map_err(|_| DnsError::Timeout)?
            .map_err(DnsError::IoError)?;

        let response_bytes = response_buf[..len].to_vec();

        // Parse response and cache A records
        if let Ok(response) = dns_parser::Packet::parse(&response_bytes) {
            self.cache_a_records(&response);
        }

        Ok(response_bytes)
    }

    fn cache_a_records(&self, response: &dns_parser::Packet) {
        let mut cache = self.cache.write().unwrap();

        for answer in &response.answers {
            if let dns_parser::RData::A(addr) = answer.data {
                let ip = addr.0;
                let domain = answer.name.to_string();
                let ttl = Duration::from_secs(answer.ttl as u64);

                tracing::debug!("DNS cache: {} -> {} (TTL {}s)", ip, domain, ttl.as_secs());
                cache.insert(ip, domain, ttl);
            }
            // AAAA records would be handled here for IPv6 support
        }
    }
}

/// Build a minimal DNS query packet for a domain.
/// Used for testing.
#[cfg(test)]
fn build_dns_query(domain: &str, query_id: u16) -> Vec<u8> {
    let mut packet = Vec::new();

    // Header
    packet.extend_from_slice(&query_id.to_be_bytes()); // ID
    packet.extend_from_slice(&[0x01, 0x00]); // Flags: standard query, recursion desired
    packet.extend_from_slice(&[0x00, 0x01]); // QDCOUNT: 1 question
    packet.extend_from_slice(&[0x00, 0x00]); // ANCOUNT: 0
    packet.extend_from_slice(&[0x00, 0x00]); // NSCOUNT: 0
    packet.extend_from_slice(&[0x00, 0x00]); // ARCOUNT: 0

    // Question section
    for label in domain.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0x00); // End of domain name

    packet.extend_from_slice(&[0x00, 0x01]); // QTYPE: A
    packet.extend_from_slice(&[0x00, 0x01]); // QCLASS: IN

    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dns_query_valid() {
        let query = build_dns_query("example.com", 0x1234);
        let parsed = dns_parser::Packet::parse(&query);
        assert!(parsed.is_ok());

        let packet = parsed.unwrap();
        assert_eq!(packet.header.id, 0x1234);
        assert_eq!(packet.questions.len(), 1);
        assert_eq!(packet.questions[0].qname.to_string(), "example.com");
    }

    #[test]
    fn proxy_creation() {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let _proxy = DnsProxy::new(cache);
    }

    #[tokio::test]
    async fn proxy_rejects_invalid_query() {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let proxy = DnsProxy::new(cache);

        let invalid_bytes = vec![0x00, 0x01, 0x02];
        let result = proxy.handle_query(&invalid_bytes).await;
        assert!(matches!(result, Err(DnsError::ParseError)));
    }

    #[test]
    fn dns_error_display() {
        assert_eq!(
            DnsError::ParseError.to_string(),
            "failed to parse DNS packet"
        );
        assert_eq!(DnsError::Timeout.to_string(), "DNS query timed out");
    }

    // Note: Integration test for actual DNS forwarding requires network access
    // and is tested in the integration test suite instead.
}
