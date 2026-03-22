//! DNS proxy for intercepting and caching DNS queries.
//!
//! Forwards DNS queries from the guest to system DNS servers,
//! caches A/AAAA record responses for domain-based filtering.

use super::cache::DnsCache;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::UdpSocket;

const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const FALLBACK_DNS: &str = "8.8.8.8:53";

/// Extract all IPv4 nameservers from a parsed resolv.conf.
///
/// We only use IPv4 nameservers because our DNS proxy socket binds to 0.0.0.0,
/// which doesn't support sending to IPv6 addresses.
fn ipv4_nameservers(config: &resolv_conf::Config) -> Vec<SocketAddr> {
    config
        .nameservers
        .iter()
        .filter_map(|ns| {
            let ip: std::net::IpAddr = ns.into();
            ip.is_ipv4().then(|| SocketAddr::new(ip, 53))
        })
        .collect()
}

/// Read all IPv4 nameservers from the system's resolv.conf (blocking).
///
/// Only used in tests. Production code should use `get_system_dns_servers_async()`.
#[cfg(test)]
fn get_system_dns_servers() -> Vec<SocketAddr> {
    let Ok(contents) = std::fs::read("/etc/resolv.conf") else {
        return Vec::new();
    };
    let Ok(config) = resolv_conf::Config::parse(&contents) else {
        return Vec::new();
    };
    ipv4_nameservers(&config)
}

/// Read all IPv4 nameservers from the system's resolv.conf asynchronously.
async fn get_system_dns_servers_async() -> Vec<SocketAddr> {
    let Ok(contents) = tokio::fs::read("/etc/resolv.conf").await else {
        return Vec::new();
    };
    let Ok(config) = resolv_conf::Config::parse(&contents) else {
        return Vec::new();
    };
    ipv4_nameservers(&config)
}

/// DNS proxy that forwards queries and caches responses.
///
/// This struct is Clone to allow spawning background DNS query tasks
/// that share the cache but operate independently.
#[derive(Clone)]
pub struct DnsProxy {
    cache: Arc<RwLock<DnsCache>>,
    upstream_servers: Vec<SocketAddr>,
    preferred_server: Arc<AtomicUsize>,
    timeout: Duration,
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
    /// Response transaction ID doesn't match query
    IdMismatch { expected: u16, got: u16 },
    /// Response question section doesn't match query
    QuestionMismatch,
}

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsError::ParseError => write!(f, "failed to parse DNS packet"),
            DnsError::IoError(e) => write!(f, "DNS I/O error: {}", e),
            DnsError::Timeout => write!(f, "DNS query timed out"),
            DnsError::IdMismatch { expected, got } => {
                write!(
                    f,
                    "DNS transaction ID mismatch: expected {}, got {}",
                    expected, got
                )
            }
            DnsError::QuestionMismatch => write!(f, "DNS question section mismatch"),
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

/// Validate that a DNS response matches the original query.
pub fn validate_dns_response(
    query: &dns_parser::Packet,
    response: &dns_parser::Packet,
) -> Result<(), DnsError> {
    if response.header.id != query.header.id {
        return Err(DnsError::IdMismatch {
            expected: query.header.id,
            got: response.header.id,
        });
    }

    let query_q = query.questions.first();
    let response_q = response.questions.first();

    match (query_q, response_q) {
        (Some(q), Some(r)) => {
            if q.qname.to_string() != r.qname.to_string()
                || q.qtype != r.qtype
                || q.qclass != r.qclass
            {
                return Err(DnsError::QuestionMismatch);
            }
        }
        (None, None) => {}
        _ => return Err(DnsError::QuestionMismatch),
    }

    Ok(())
}

impl DnsProxy {
    fn with_servers(cache: Arc<RwLock<DnsCache>>, upstream_servers: Vec<SocketAddr>) -> Self {
        Self {
            cache,
            upstream_servers,
            preferred_server: Arc::new(AtomicUsize::new(0)),
            timeout: DNS_TIMEOUT,
        }
    }

    /// Create a new DNS proxy with an explicit upstream DNS server.
    #[cfg(any(test, fuzzing))]
    pub fn with_upstream(cache: Arc<RwLock<DnsCache>>, upstream_dns: SocketAddr) -> Self {
        Self::with_servers(cache, vec![upstream_dns])
    }

    /// Create a new DNS proxy with the given cache.
    ///
    /// Uses the system's DNS server from `/etc/resolv.conf` if available,
    /// otherwise falls back to Google's public DNS (8.8.8.8).
    ///
    /// This reads `/etc/resolv.conf` asynchronously to avoid blocking the
    /// tokio runtime on slow filesystems.
    pub async fn new(cache: Arc<RwLock<DnsCache>>) -> Self {
        let mut upstream_servers = get_system_dns_servers_async().await;
        if upstream_servers.is_empty() {
            tracing::debug!("Using fallback DNS server: {}", FALLBACK_DNS);
            upstream_servers.push(FALLBACK_DNS.parse().unwrap());
        }

        tracing::debug!(
            "DNS proxy configured with {} upstream server(s), primary: {}",
            upstream_servers.len(),
            upstream_servers[0]
        );

        Self::with_servers(cache, upstream_servers)
    }

    /// Create a new DNS proxy synchronously (for tests only).
    #[cfg(test)]
    pub fn new_sync(cache: Arc<RwLock<DnsCache>>) -> Self {
        let mut upstream_servers = get_system_dns_servers();
        if upstream_servers.is_empty() {
            tracing::debug!("Using fallback DNS server: {}", FALLBACK_DNS);
            upstream_servers.push(FALLBACK_DNS.parse().unwrap());
        }

        Self::with_servers(cache, upstream_servers)
    }

    /// Returns the list of upstream DNS servers.
    #[cfg(any(test, fuzzing))]
    pub fn upstream_servers(&self) -> &[SocketAddr] {
        &self.upstream_servers
    }

    /// Handle a DNS query from the guest.
    ///
    /// Forwards the query to upstream DNS, validates the response matches the query
    /// (transaction ID and question section), caches A records from the response,
    /// and returns the response bytes to send back to the guest.
    ///
    /// On timeout or send failure, retries with the next upstream server in round-robin
    /// order. On success, rotates the preferred server so future queries start with the
    /// server that last worked.
    ///
    /// Security: Uses `connect()` so the kernel rejects packets from other sources,
    /// preventing DNS response spoofing from local processes.
    pub async fn handle_query(&self, query_bytes: &[u8]) -> Result<Vec<u8>, DnsError> {
        let query = dns_parser::Packet::parse(query_bytes).map_err(|_| DnsError::ParseError)?;

        let server_count = self.upstream_servers.len();
        let start = self.preferred_server.load(Ordering::Relaxed) % server_count;
        let mut last_err = DnsError::Timeout;

        for i in 0..server_count {
            let idx = (start + i) % server_count;
            let server = self.upstream_servers[idx];

            match self.try_upstream(query_bytes, server).await {
                Ok(response_bytes) => {
                    let response = dns_parser::Packet::parse(&response_bytes)
                        .map_err(|_| DnsError::ParseError)?;
                    validate_dns_response(&query, &response)?;
                    self.cache_a_records(&response);
                    self.preferred_server.store(idx, Ordering::Relaxed);
                    if response_bytes.len() >= 3 && response_bytes[2] & 0x02 != 0 {
                        tracing::warn!(
                            "DNS response truncated (TC bit set), guest resolver has no TCP fallback path"
                        );
                    }
                    return Ok(response_bytes);
                }
                Err(e @ DnsError::Timeout) | Err(e @ DnsError::IoError(_)) => {
                    tracing::debug!("DNS query to {} failed: {}, trying next server", server, e);
                    last_err = e;
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_err)
    }

    async fn try_upstream(
        &self,
        query_bytes: &[u8],
        server: SocketAddr,
    ) -> Result<Vec<u8>, DnsError> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(DnsError::IoError)?;

        socket.connect(server).await.map_err(DnsError::IoError)?;

        socket.send(query_bytes).await.map_err(DnsError::IoError)?;

        let mut response_buf = vec![0u8; 4096];
        let len = tokio::time::timeout(self.timeout, socket.recv(&mut response_buf))
            .await
            .map_err(|_| DnsError::Timeout)?
            .map_err(DnsError::IoError)?;

        Ok(response_buf[..len].to_vec())
    }

    pub fn cache_a_records(&self, response: &dns_parser::Packet) {
        let mut cache = self
            .cache
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

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
#[cfg(test)]
pub(crate) fn build_dns_query(domain: &str, query_id: u16) -> Vec<u8> {
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
    fn new_sync_loads_at_least_one_server() {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let proxy = DnsProxy::new_sync(cache);
        assert!(!proxy.upstream_servers().is_empty());
    }

    #[tokio::test]
    async fn proxy_rejects_invalid_query() {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let proxy = DnsProxy::new_sync(cache);

        let invalid_bytes = vec![0x00, 0x01, 0x02];
        let result = proxy.handle_query(&invalid_bytes).await;
        assert!(matches!(result, Err(DnsError::ParseError)));
    }

    /// Build a minimal DNS response packet with an A record.
    fn build_dns_response(
        domain: &str,
        ip: std::net::Ipv4Addr,
        ttl: u32,
        query_id: u16,
    ) -> Vec<u8> {
        let mut packet = Vec::new();

        // Header
        packet.extend_from_slice(&query_id.to_be_bytes()); // ID
        packet.extend_from_slice(&[0x81, 0x80]); // Flags: response, recursion available
        packet.extend_from_slice(&[0x00, 0x01]); // QDCOUNT: 1 question
        packet.extend_from_slice(&[0x00, 0x01]); // ANCOUNT: 1 answer
        packet.extend_from_slice(&[0x00, 0x00]); // NSCOUNT: 0
        packet.extend_from_slice(&[0x00, 0x00]); // ARCOUNT: 0

        // Question section (echoed from query)
        for label in domain.split('.') {
            packet.push(label.len() as u8);
            packet.extend_from_slice(label.as_bytes());
        }
        packet.push(0x00); // End of domain name
        packet.extend_from_slice(&[0x00, 0x01]); // QTYPE: A
        packet.extend_from_slice(&[0x00, 0x01]); // QCLASS: IN

        // Answer section
        for label in domain.split('.') {
            packet.push(label.len() as u8);
            packet.extend_from_slice(label.as_bytes());
        }
        packet.push(0x00); // End of domain name
        packet.extend_from_slice(&[0x00, 0x01]); // TYPE: A
        packet.extend_from_slice(&[0x00, 0x01]); // CLASS: IN
        packet.extend_from_slice(&ttl.to_be_bytes()); // TTL
        packet.extend_from_slice(&[0x00, 0x04]); // RDLENGTH: 4 bytes
        packet.extend_from_slice(&ip.octets()); // RDATA: IP address

        packet
    }

    #[test]
    fn build_dns_response_valid() {
        let response = build_dns_response(
            "example.com",
            std::net::Ipv4Addr::new(93, 184, 216, 34),
            300,
            0x1234,
        );
        let parsed = dns_parser::Packet::parse(&response);
        assert!(parsed.is_ok());

        let packet = parsed.unwrap();
        assert_eq!(packet.header.id, 0x1234);
        assert_eq!(packet.answers.len(), 1);
        assert_eq!(packet.answers[0].name.to_string(), "example.com");
    }

    #[test]
    fn cache_a_records_from_response() {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let proxy = DnsProxy::new_sync(cache.clone());

        let response = build_dns_response(
            "example.com",
            std::net::Ipv4Addr::new(93, 184, 216, 34),
            300,
            0x1234,
        );
        let parsed = dns_parser::Packet::parse(&response).unwrap();

        proxy.cache_a_records(&parsed);

        let cache_read = cache.read().unwrap();
        assert_eq!(
            cache_read.lookup(std::net::Ipv4Addr::new(93, 184, 216, 34)),
            Some("example.com")
        );
    }

    #[test]
    fn cache_multiple_a_records() {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let proxy = DnsProxy::new_sync(cache.clone());

        // Cache two different domains
        let response1 = build_dns_response(
            "example.com",
            std::net::Ipv4Addr::new(93, 184, 216, 34),
            300,
            0x1234,
        );
        let response2 = build_dns_response(
            "example.org",
            std::net::Ipv4Addr::new(93, 184, 216, 35),
            300,
            0x1235,
        );

        proxy.cache_a_records(&dns_parser::Packet::parse(&response1).unwrap());
        proxy.cache_a_records(&dns_parser::Packet::parse(&response2).unwrap());

        let cache_read = cache.read().unwrap();
        assert_eq!(
            cache_read.lookup(std::net::Ipv4Addr::new(93, 184, 216, 34)),
            Some("example.com")
        );
        assert_eq!(
            cache_read.lookup(std::net::Ipv4Addr::new(93, 184, 216, 35)),
            Some("example.org")
        );
    }

    #[test]
    fn validate_dns_response_id_mismatch() {
        let query = build_dns_query("example.com", 0x1234);
        let response = build_dns_response(
            "example.com",
            std::net::Ipv4Addr::new(93, 184, 216, 34),
            300,
            0x5678,
        );

        let query_parsed = dns_parser::Packet::parse(&query).unwrap();
        let response_parsed = dns_parser::Packet::parse(&response).unwrap();

        let result = validate_dns_response(&query_parsed, &response_parsed);
        assert!(matches!(
            result,
            Err(DnsError::IdMismatch {
                expected: 0x1234,
                got: 0x5678
            })
        ));
    }

    #[test]
    fn validate_dns_response_question_mismatch() {
        let query = build_dns_query("example.com", 0x1234);
        let response =
            build_dns_response("evil.com", std::net::Ipv4Addr::new(6, 6, 6, 6), 300, 0x1234);

        let query_parsed = dns_parser::Packet::parse(&query).unwrap();
        let response_parsed = dns_parser::Packet::parse(&response).unwrap();

        let result = validate_dns_response(&query_parsed, &response_parsed);
        assert!(matches!(result, Err(DnsError::QuestionMismatch)));
    }

    #[test]
    fn validate_dns_response_valid() {
        let query = build_dns_query("example.com", 0x1234);
        let response = build_dns_response(
            "example.com",
            std::net::Ipv4Addr::new(93, 184, 216, 34),
            300,
            0x1234,
        );

        let query_parsed = dns_parser::Packet::parse(&query).unwrap();
        let response_parsed = dns_parser::Packet::parse(&response).unwrap();

        let result = validate_dns_response(&query_parsed, &response_parsed);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_dns_response_empty_questions() {
        let empty_questions_packet = vec![
            0x12, 0x34, // ID
            0x01, 0x00, // Flags: standard query
            0x00, 0x00, // QDCOUNT: 0
            0x00, 0x00, // ANCOUNT: 0
            0x00, 0x00, // NSCOUNT: 0
            0x00, 0x00, // ARCOUNT: 0
        ];

        let query_parsed = dns_parser::Packet::parse(&empty_questions_packet).unwrap();
        let response_parsed = dns_parser::Packet::parse(&empty_questions_packet).unwrap();

        let result = validate_dns_response(&query_parsed, &response_parsed);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_dns_response_query_has_question_response_empty() {
        let query = build_dns_query("example.com", 0x1234);
        let empty_response = vec![
            0x12, 0x34, // ID
            0x81, 0x80, // Flags: response
            0x00, 0x00, // QDCOUNT: 0
            0x00, 0x00, // ANCOUNT: 0
            0x00, 0x00, // NSCOUNT: 0
            0x00, 0x00, // ARCOUNT: 0
        ];

        let query_parsed = dns_parser::Packet::parse(&query).unwrap();
        let response_parsed = dns_parser::Packet::parse(&empty_response).unwrap();

        let result = validate_dns_response(&query_parsed, &response_parsed);
        assert!(matches!(result, Err(DnsError::QuestionMismatch)));
    }

    #[test]
    fn ipv4_nameservers_filters_ipv6_and_preserves_order() {
        let config = resolv_conf::Config::parse(
            b"nameserver 2600:4040:ae41:ad01::1\nnameserver 10.1.1.1\nnameserver ::1\nnameserver 8.8.4.4\n",
        )
        .expect("valid config");
        let result = ipv4_nameservers(&config);
        assert_eq!(
            result,
            vec![
                SocketAddr::new("10.1.1.1".parse().unwrap(), 53),
                SocketAddr::new("8.8.4.4".parse().unwrap(), 53),
            ]
        );
    }

    #[test]
    fn ipv4_nameservers_empty_config() {
        let config = resolv_conf::Config::parse(b"# empty config\n").expect("valid config");
        let result = ipv4_nameservers(&config);
        assert!(result.is_empty());
    }

    const TEST_TIMEOUT: Duration = Duration::from_millis(100);

    fn test_proxy(servers: Vec<SocketAddr>) -> DnsProxy {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        DnsProxy {
            cache,
            upstream_servers: servers,
            preferred_server: Arc::new(AtomicUsize::new(0)),
            timeout: TEST_TIMEOUT,
        }
    }

    fn test_proxy_with_cache(servers: Vec<SocketAddr>) -> (DnsProxy, Arc<RwLock<DnsCache>>) {
        let cache = Arc::new(RwLock::new(DnsCache::new()));
        let proxy = DnsProxy {
            cache: cache.clone(),
            upstream_servers: servers,
            preferred_server: Arc::new(AtomicUsize::new(0)),
            timeout: TEST_TIMEOUT,
        };
        (proxy, cache)
    }

    /// Bind a UDP socket that never responds — queries to it will time out.
    async fn silent_server() -> (UdpSocket, SocketAddr) {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        (sock, addr)
    }

    #[tokio::test]
    async fn failover_on_primary_timeout() {
        let (_silent, silent_addr) = silent_server().await;

        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let responder_addr = responder.local_addr().unwrap();

        let query_id: u16 = 0xABCD;
        let domain = "failover.test";
        let response_ip = std::net::Ipv4Addr::new(10, 20, 30, 40);

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (len, src) = responder.recv_from(&mut buf).await.unwrap();
            let _ = dns_parser::Packet::parse(&buf[..len]).unwrap();
            let response_bytes = build_dns_response(domain, response_ip, 300, query_id);
            responder.send_to(&response_bytes, src).await.unwrap();
        });

        let (proxy, cache) = test_proxy_with_cache(vec![silent_addr, responder_addr]);

        let query = build_dns_query(domain, query_id);
        let result = proxy.handle_query(&query).await;
        assert!(result.is_ok(), "expected failover to succeed: {:?}", result);

        assert_eq!(
            proxy.preferred_server.load(Ordering::Relaxed),
            1,
            "preferred server should rotate to the secondary"
        );

        let cache_read = cache.read().unwrap();
        assert_eq!(cache_read.lookup(response_ip), Some(domain));
    }

    #[tokio::test]
    async fn preferred_server_persists_across_queries() {
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let responder_addr = responder.local_addr().unwrap();

        let domain = "persist.test";
        let response_ip = std::net::Ipv4Addr::new(10, 0, 0, 1);

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            for _ in 0..2 {
                let (len, src) = responder.recv_from(&mut buf).await.unwrap();
                let parsed = dns_parser::Packet::parse(&buf[..len]).unwrap();
                let resp = build_dns_response(domain, response_ip, 300, parsed.header.id);
                responder.send_to(&resp, src).await.unwrap();
            }
        });

        let (_silent, silent_addr) = silent_server().await;
        let proxy = test_proxy(vec![silent_addr, responder_addr]);

        let query = build_dns_query(domain, 0x1111);
        proxy.handle_query(&query).await.unwrap();
        assert_eq!(proxy.preferred_server.load(Ordering::Relaxed), 1);

        let query2 = build_dns_query(domain, 0x2222);
        let result = proxy.handle_query(&query2).await;
        assert!(
            result.is_ok(),
            "second query should use preferred server directly"
        );
        assert_eq!(proxy.preferred_server.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn all_servers_timeout_returns_error() {
        let (_s1, addr1) = silent_server().await;
        let (_s2, addr2) = silent_server().await;

        let proxy = test_proxy(vec![addr1, addr2]);

        let query = build_dns_query("timeout.test", 0xDEAD);
        let result = proxy.handle_query(&query).await;
        assert!(
            matches!(result, Err(DnsError::Timeout)),
            "expected timeout when all servers are unresponsive: {:?}",
            result
        );
    }

    fn build_dns_response_with_tc(
        domain: &str,
        ip: std::net::Ipv4Addr,
        ttl: u32,
        query_id: u16,
    ) -> Vec<u8> {
        let mut packet = build_dns_response(domain, ip, ttl, query_id);
        // Set TC bit (bit 1 of byte 2)
        packet[2] |= 0x02;
        packet
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn tc_bit_in_response_logs_warning() {
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let responder_addr = responder.local_addr().unwrap();

        let query_id: u16 = 0xBEEF;
        let domain = "truncated.test";
        let response_ip = std::net::Ipv4Addr::new(10, 0, 0, 1);

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (len, src) = responder.recv_from(&mut buf).await.unwrap();
            let _ = dns_parser::Packet::parse(&buf[..len]).unwrap();
            let response_bytes = build_dns_response_with_tc(domain, response_ip, 300, query_id);
            responder.send_to(&response_bytes, src).await.unwrap();
        });

        let proxy = test_proxy(vec![responder_addr]);
        let query = build_dns_query(domain, query_id);
        let result = proxy.handle_query(&query).await;
        assert!(result.is_ok());

        assert!(logs_contain(
            "DNS response truncated (TC bit set), guest resolver has no TCP fallback path"
        ));
    }
}
