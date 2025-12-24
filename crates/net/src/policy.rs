//! Network policy enforcement for filtering guest traffic.
//!
//! This module provides packet matching against NetworkPolicy rules,
//! allowing traffic to be allowed, denied, or logged.

use smoltcp::wire::{
    EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket,
};
use std::net::Ipv4Addr;

/// Extracted packet information for policy matching.
#[derive(Debug, Clone)]
pub struct PacketInfo {
    pub src_ip: Ipv4Addr,
    pub dst_ip: Ipv4Addr,
    pub protocol: PacketProtocol,
    pub src_port: Option<u16>,
    pub dst_port: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketProtocol {
    Tcp,
    Udp,
    Icmp,
    Other(u8),
}

/// Result of policy check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyResult {
    Allow,
    Deny,
    Log,
}

/// Policy checker that evaluates packets against rules.
pub struct PolicyChecker {
    default_action: PolicyResult,
    rules: Vec<CompiledRule>,
}

struct CompiledRule {
    action: PolicyResult,
    matcher: CompiledMatcher,
}

enum CompiledMatcher {
    Any,
    Ip(Ipv4Addr),
    IpRange { network: u32, mask: u32 },
    Port(u16),
    PortRange { start: u16, end: u16 },
    Protocol(PacketProtocol),
    All(Vec<CompiledMatcher>),
}

impl PolicyChecker {
    /// Create a policy checker from the given configuration.
    pub fn new(default_action: capsa_core::PolicyAction, rules: &[capsa_core::PolicyRule]) -> Self {
        let default_action = convert_action(default_action);
        let rules = rules.iter().map(compile_rule).collect();

        Self {
            default_action,
            rules,
        }
    }

    /// Check a packet against the policy.
    /// Returns the action to take.
    pub fn check(&self, info: &PacketInfo) -> PolicyResult {
        for rule in &self.rules {
            if rule.matcher.matches(info) {
                return rule.action;
            }
        }
        self.default_action
    }

    /// Extract packet info from an ethernet frame.
    pub fn extract_packet_info(frame: &[u8]) -> Option<PacketInfo> {
        let eth_frame = EthernetFrame::new_checked(frame).ok()?;

        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return None;
        }

        let ip_packet = Ipv4Packet::new_checked(eth_frame.payload()).ok()?;

        let src_ip: Ipv4Addr = ip_packet.src_addr().into();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr().into();

        let (protocol, src_port, dst_port) = match ip_packet.next_header() {
            IpProtocol::Tcp => {
                let tcp = TcpPacket::new_checked(ip_packet.payload()).ok()?;
                (
                    PacketProtocol::Tcp,
                    Some(tcp.src_port()),
                    Some(tcp.dst_port()),
                )
            }
            IpProtocol::Udp => {
                let udp = UdpPacket::new_checked(ip_packet.payload()).ok()?;
                (
                    PacketProtocol::Udp,
                    Some(udp.src_port()),
                    Some(udp.dst_port()),
                )
            }
            IpProtocol::Icmp => (PacketProtocol::Icmp, None, None),
            other => (PacketProtocol::Other(other.into()), None, None),
        };

        Some(PacketInfo {
            src_ip,
            dst_ip,
            protocol,
            src_port,
            dst_port,
        })
    }
}

fn convert_action(action: capsa_core::PolicyAction) -> PolicyResult {
    match action {
        capsa_core::PolicyAction::Allow => PolicyResult::Allow,
        capsa_core::PolicyAction::Deny => PolicyResult::Deny,
        capsa_core::PolicyAction::Log => PolicyResult::Log,
    }
}

fn compile_rule(rule: &capsa_core::PolicyRule) -> CompiledRule {
    CompiledRule {
        action: convert_action(rule.action),
        matcher: compile_matcher(&rule.matcher),
    }
}

fn compile_matcher(matcher: &capsa_core::RuleMatcher) -> CompiledMatcher {
    match matcher {
        capsa_core::RuleMatcher::Any => CompiledMatcher::Any,
        capsa_core::RuleMatcher::Ip(ip) => CompiledMatcher::Ip(*ip),
        capsa_core::RuleMatcher::IpRange { network, prefix } => {
            let mask = if *prefix == 0 {
                0
            } else {
                !0u32 << (32 - prefix)
            };
            CompiledMatcher::IpRange {
                network: u32::from_be_bytes(network.octets()) & mask,
                mask,
            }
        }
        capsa_core::RuleMatcher::Port(port) => CompiledMatcher::Port(*port),
        capsa_core::RuleMatcher::PortRange { start, end } => CompiledMatcher::PortRange {
            start: *start,
            end: *end,
        },
        capsa_core::RuleMatcher::Protocol(proto) => {
            let proto = match proto {
                capsa_core::Protocol::Tcp => PacketProtocol::Tcp,
                capsa_core::Protocol::Udp => PacketProtocol::Udp,
            };
            CompiledMatcher::Protocol(proto)
        }
        capsa_core::RuleMatcher::Domain(_domain) => {
            // Domain matching requires DNS interception - not yet implemented
            // For now, treat as "never matches"
            CompiledMatcher::All(vec![])
        }
        capsa_core::RuleMatcher::All(matchers) => {
            CompiledMatcher::All(matchers.iter().map(compile_matcher).collect())
        }
    }
}

impl CompiledMatcher {
    fn matches(&self, info: &PacketInfo) -> bool {
        match self {
            CompiledMatcher::Any => true,
            CompiledMatcher::Ip(ip) => info.dst_ip == *ip,
            CompiledMatcher::IpRange { network, mask } => {
                let dst = u32::from_be_bytes(info.dst_ip.octets());
                (dst & mask) == *network
            }
            CompiledMatcher::Port(port) => info.dst_port == Some(*port),
            CompiledMatcher::PortRange { start, end } => {
                info.dst_port.map_or(false, |p| p >= *start && p <= *end)
            }
            CompiledMatcher::Protocol(proto) => info.protocol == *proto,
            CompiledMatcher::All(matchers) => matchers.iter().all(|m| m.matches(info)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsa_core::{NetworkPolicy, PolicyAction, RuleMatcher};

    fn make_packet_info(dst_ip: Ipv4Addr, dst_port: u16, proto: PacketProtocol) -> PacketInfo {
        PacketInfo {
            src_ip: Ipv4Addr::new(10, 0, 2, 15),
            dst_ip,
            protocol: proto,
            src_port: Some(12345),
            dst_port: Some(dst_port),
        }
    }

    #[test]
    fn allow_all_policy() {
        let policy = NetworkPolicy::allow_all();
        let checker = PolicyChecker::new(policy.default_action, &policy.rules);

        let info = make_packet_info(Ipv4Addr::new(8, 8, 8, 8), 53, PacketProtocol::Udp);
        assert_eq!(checker.check(&info), PolicyResult::Allow);
    }

    #[test]
    fn deny_all_policy() {
        let policy = NetworkPolicy::deny_all();
        let checker = PolicyChecker::new(policy.default_action, &policy.rules);

        let info = make_packet_info(Ipv4Addr::new(8, 8, 8, 8), 53, PacketProtocol::Udp);
        assert_eq!(checker.check(&info), PolicyResult::Deny);
    }

    #[test]
    fn deny_all_allow_port() {
        let policy = NetworkPolicy::deny_all().allow_port(443);
        let checker = PolicyChecker::new(policy.default_action, &policy.rules);

        // HTTPS allowed
        let info = make_packet_info(Ipv4Addr::new(1, 2, 3, 4), 443, PacketProtocol::Tcp);
        assert_eq!(checker.check(&info), PolicyResult::Allow);

        // HTTP denied
        let info = make_packet_info(Ipv4Addr::new(1, 2, 3, 4), 80, PacketProtocol::Tcp);
        assert_eq!(checker.check(&info), PolicyResult::Deny);
    }

    #[test]
    fn deny_all_allow_ip() {
        let policy = NetworkPolicy::deny_all().allow_ip(Ipv4Addr::new(8, 8, 8, 8));
        let checker = PolicyChecker::new(policy.default_action, &policy.rules);

        // Allowed IP
        let info = make_packet_info(Ipv4Addr::new(8, 8, 8, 8), 53, PacketProtocol::Udp);
        assert_eq!(checker.check(&info), PolicyResult::Allow);

        // Denied IP
        let info = make_packet_info(Ipv4Addr::new(1, 1, 1, 1), 53, PacketProtocol::Udp);
        assert_eq!(checker.check(&info), PolicyResult::Deny);
    }

    #[test]
    fn ip_range_matching() {
        let policy = NetworkPolicy::deny_all().rule(
            PolicyAction::Allow,
            RuleMatcher::IpRange {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix: 8,
            },
        );
        let checker = PolicyChecker::new(policy.default_action, &policy.rules);

        // In range
        let info = make_packet_info(Ipv4Addr::new(10, 1, 2, 3), 80, PacketProtocol::Tcp);
        assert_eq!(checker.check(&info), PolicyResult::Allow);

        // Out of range
        let info = make_packet_info(Ipv4Addr::new(192, 168, 1, 1), 80, PacketProtocol::Tcp);
        assert_eq!(checker.check(&info), PolicyResult::Deny);
    }

    #[test]
    fn composite_matcher() {
        // Allow DNS (port 53 + UDP)
        let policy = NetworkPolicy::deny_all().allow_dns();
        let checker = PolicyChecker::new(policy.default_action, &policy.rules);

        // UDP DNS allowed
        let info = make_packet_info(Ipv4Addr::new(8, 8, 8, 8), 53, PacketProtocol::Udp);
        assert_eq!(checker.check(&info), PolicyResult::Allow);

        // TCP to port 53 denied (not UDP)
        let info = make_packet_info(Ipv4Addr::new(8, 8, 8, 8), 53, PacketProtocol::Tcp);
        assert_eq!(checker.check(&info), PolicyResult::Deny);
    }
}
