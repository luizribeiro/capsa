mod cache;
mod proxy;

pub use cache::DnsCache;
#[cfg(fuzzing)]
pub use proxy::validate_dns_response;
pub use proxy::DnsProxy;
