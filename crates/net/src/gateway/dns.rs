use crate::dns::DnsCache;
use crate::dns::DnsProxy;

use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use smoltcp::wire::EthernetAddress;

use tokio::sync::Semaphore;

/// Maximum concurrent DNS queries to prevent resource exhaustion.
/// Each query spawns a background task and creates a UDP socket.
pub(super) const MAX_DNS_QUERIES: usize = 64;

/// Parsed DNS query information extracted from a frame.
pub(super) struct DnsQueryInfo {
    pub(super) guest_mac: EthernetAddress,
    pub(super) guest_ip: Ipv4Addr,
    pub(super) guest_port: u16,
    pub(super) query_bytes: Vec<u8>,
}

/// DNS response from a background query task.
pub(crate) struct DnsResponse {
    pub(super) guest_mac: EthernetAddress,
    pub(super) guest_ip: Ipv4Addr,
    pub(super) guest_port: u16,
    pub(super) response_bytes: Vec<u8>,
}

/// Handles DNS query dispatching, caching, and rate limiting.
///
/// Owns the sending side of the DNS response channel plus the proxy,
/// cache, and concurrency semaphore. The receiving half stays on
/// `GatewayStack` so it can be polled in the main `select!` loop.
pub(crate) struct DnsDispatcher {
    pub(super) proxy: DnsProxy,
    pub(super) cache: Arc<RwLock<DnsCache>>,
    pub(super) response_tx: tokio::sync::mpsc::Sender<DnsResponse>,
    pub(super) semaphore: Arc<Semaphore>,
}

impl DnsDispatcher {
    pub(super) fn dispatch_query(&self, query_info: DnsQueryInfo) {
        let permit = match self.semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::debug!(
                    "DNS query limit reached ({}), dropping query from {}:{}",
                    MAX_DNS_QUERIES,
                    query_info.guest_ip,
                    query_info.guest_port
                );
                return;
            }
        };

        let dns_proxy = self.proxy.clone();
        let response_tx = self.response_tx.clone();
        let guest_mac = query_info.guest_mac;
        let guest_ip = query_info.guest_ip;
        let guest_port = query_info.guest_port;
        let query_bytes = query_info.query_bytes;

        crate::util::spawn_named("dns-query", async move {
            let _permit = permit;

            match dns_proxy.handle_query(&query_bytes).await {
                Ok(response_bytes) => {
                    if response_tx
                        .send(DnsResponse {
                            guest_mac,
                            guest_ip,
                            guest_port,
                            response_bytes,
                        })
                        .await
                        .is_err()
                    {
                        tracing::debug!(
                            "DNS response channel closed, dropping response for {}:{}",
                            guest_ip,
                            guest_port
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("DNS proxy error for {}:{}: {}", guest_ip, guest_port, e);
                }
            }
        });
    }

    pub(super) fn cleanup_cache(&self) {
        match self.cache.write() {
            Ok(mut cache) => cache.cleanup(),
            Err(poisoned) => {
                tracing::error!(
                    "DNS cache lock poisoned (another thread panicked), resetting cache"
                );
                let mut cache = poisoned.into_inner();
                *cache = DnsCache::new();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_dns_queries_constant() {
        assert!(
            MAX_DNS_QUERIES >= 16,
            "Should allow at least 16 concurrent queries"
        );
        assert!(
            MAX_DNS_QUERIES <= 256,
            "Should not allow too many concurrent queries"
        );
    }

    #[tokio::test]
    async fn test_dns_semaphore_limits_concurrent_tasks() {
        use tokio::sync::Semaphore;

        let semaphore = Arc::new(Semaphore::new(2));

        let permit1 = semaphore.clone().try_acquire_owned();
        let permit2 = semaphore.clone().try_acquire_owned();
        let permit3 = semaphore.clone().try_acquire_owned();

        assert!(permit1.is_ok(), "First permit should succeed");
        assert!(permit2.is_ok(), "Second permit should succeed");
        assert!(permit3.is_err(), "Third permit should fail (at limit)");

        drop(permit1);
        let permit4 = semaphore.clone().try_acquire_owned();
        assert!(permit4.is_ok(), "Should succeed after permit released");
    }

    #[tokio::test]
    async fn test_dns_response_channel_bounded() {
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::channel::<DnsResponse>(2);

        let resp1 = DnsResponse {
            guest_mac: EthernetAddress([0; 6]),
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            guest_port: 12345,
            response_bytes: vec![1, 2, 3],
        };
        let resp2 = DnsResponse {
            guest_mac: EthernetAddress([0; 6]),
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            guest_port: 12346,
            response_bytes: vec![4, 5, 6],
        };

        assert!(tx.try_send(resp1).is_ok());
        assert!(tx.try_send(resp2).is_ok());

        let resp3 = DnsResponse {
            guest_mac: EthernetAddress([0; 6]),
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            guest_port: 12347,
            response_bytes: vec![7, 8, 9],
        };
        assert!(tx.try_send(resp3).is_err());

        let _ = rx.recv().await;
        let resp4 = DnsResponse {
            guest_mac: EthernetAddress([0; 6]),
            guest_ip: Ipv4Addr::new(10, 0, 2, 15),
            guest_port: 12348,
            response_bytes: vec![10, 11, 12],
        };
        assert!(tx.try_send(resp4).is_ok());
    }
}
