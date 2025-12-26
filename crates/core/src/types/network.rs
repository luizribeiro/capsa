use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::net::Ipv4Addr;

/// Pattern for matching domain names in network policies.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainPattern {
    /// Exact match: "api.anthropic.com"
    Exact(String),
    /// Wildcard match: "*.github.com" matches "api.github.com"
    /// Does NOT match "github.com" itself (must have subdomain)
    Wildcard(String),
}

impl Serialize for DomainPattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            DomainPattern::Exact(s) => serializer.serialize_str(s),
            DomainPattern::Wildcard(s) => serializer.serialize_str(&format!("*.{}", s)),
        }
    }
}

impl DomainPattern {
    /// Parse a pattern string into a DomainPattern.
    /// Patterns starting with "*." are treated as wildcards.
    pub fn parse(pattern: &str) -> Self {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            DomainPattern::Wildcard(suffix.to_lowercase())
        } else {
            DomainPattern::Exact(pattern.to_lowercase())
        }
    }

    /// Check if a domain matches this pattern.
    pub fn matches(&self, domain: &str) -> bool {
        // Validate domain length per DNS spec (max 253 characters)
        if domain.is_empty() || domain.len() > 253 {
            return false;
        }

        let domain_lower = domain.to_lowercase();
        match self {
            DomainPattern::Exact(pattern) => domain_lower == *pattern,
            DomainPattern::Wildcard(suffix) => {
                // Reject empty suffixes (would match everything)
                if suffix.is_empty() {
                    return false;
                }
                // Must end with ".suffix" (must have at least one subdomain level)
                domain_lower.ends_with(&format!(".{}", suffix))
            }
        }
    }
}

impl<'de> Deserialize<'de> for DomainPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(DomainPattern::parse(&s))
    }
}

/// Network protocol for port forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

/// Port forwarding rule: host_port → guest_port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortForward {
    pub protocol: Protocol,
    pub host_port: u16,
    pub guest_port: u16,
}

/// Action to take when a policy rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyAction {
    /// Allow the traffic
    Allow,
    /// Block the traffic
    Deny,
    /// Allow but log the traffic
    Log,
}

/// Criteria for matching network traffic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatcher {
    /// Match any traffic
    Any,
    /// Match traffic to specific IP address
    Ip(Ipv4Addr),
    /// Match traffic to IP range (CIDR notation)
    IpRange { network: Ipv4Addr, prefix: u8 },
    /// Match traffic to specific port
    Port(u16),
    /// Match traffic to port range (inclusive)
    PortRange { start: u16, end: u16 },
    /// Match traffic by protocol
    Protocol(Protocol),
    /// Match traffic to domain (requires DNS interception for IP→domain lookup)
    Domain(DomainPattern),
    /// All matchers must match
    All(Vec<RuleMatcher>),
}

/// A single policy rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRule {
    pub action: PolicyAction,
    pub matcher: RuleMatcher,
}

/// Network filtering policy for controlling guest traffic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Default action when no rule matches
    pub default_action: PolicyAction,
    /// Rules evaluated in order (first match wins)
    pub rules: Vec<PolicyRule>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            default_action: PolicyAction::Allow,
            rules: Vec::new(),
        }
    }
}

impl NetworkPolicy {
    /// Create a policy that allows all traffic (default).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Create a policy that denies all traffic.
    pub fn deny_all() -> Self {
        Self {
            default_action: PolicyAction::Deny,
            rules: Vec::new(),
        }
    }

    /// Add a rule to allow traffic to a specific IP.
    pub fn allow_ip(mut self, ip: Ipv4Addr) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Allow,
            matcher: RuleMatcher::Ip(ip),
        });
        self
    }

    /// Add a rule to allow traffic to a port.
    pub fn allow_port(mut self, port: u16) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Allow,
            matcher: RuleMatcher::Port(port),
        });
        self
    }

    /// Add a rule to deny traffic to a specific IP.
    pub fn deny_ip(mut self, ip: Ipv4Addr) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Deny,
            matcher: RuleMatcher::Ip(ip),
        });
        self
    }

    /// Add a rule to deny traffic to a port.
    pub fn deny_port(mut self, port: u16) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Deny,
            matcher: RuleMatcher::Port(port),
        });
        self
    }

    /// Add a rule to allow HTTPS traffic (port 443).
    pub fn allow_https(mut self) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Allow,
            matcher: RuleMatcher::Port(443),
        });
        self
    }

    /// Add a rule to allow DNS traffic (port 53 UDP).
    pub fn allow_dns(mut self) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Allow,
            matcher: RuleMatcher::All(vec![
                RuleMatcher::Port(53),
                RuleMatcher::Protocol(Protocol::Udp),
            ]),
        });
        self
    }

    /// Add a custom rule.
    pub fn rule(mut self, action: PolicyAction, matcher: RuleMatcher) -> Self {
        self.rules.push(PolicyRule { action, matcher });
        self
    }

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

    /// Add a rule to log traffic to a domain (continues evaluation).
    pub fn log_domain(mut self, pattern: &str) -> Self {
        self.rules.push(PolicyRule {
            action: PolicyAction::Log,
            matcher: RuleMatcher::Domain(DomainPattern::parse(pattern)),
        });
        self
    }
}

/// Network configuration for VMs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network access.
    None,
    /// Platform-native NAT networking (macOS VZ built-in).
    #[default]
    Nat,
    /// Userspace NAT via capsa-net (cross-platform, supports filtering).
    #[serde(rename = "user_nat")]
    UserNat(UserNatConfig),
    /// Cluster mode - VM joins a shared virtual switch for multi-VM networking.
    #[serde(rename = "cluster")]
    Cluster(ClusterPortConfig),
}

/// Configuration for a VM port connected to a network cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterPortConfig {
    /// Name of the cluster to join.
    pub cluster_name: String,
    /// Optional static IP address (otherwise assigned via DHCP).
    pub static_ip: Option<Ipv4Addr>,
}

impl ClusterPortConfig {
    /// Set a static IP address.
    pub fn with_ip(mut self, ip: Ipv4Addr) -> Self {
        self.static_ip = Some(ip);
        self
    }

    /// Convert to NetworkMode.
    pub fn build(self) -> NetworkMode {
        NetworkMode::Cluster(self)
    }
}

impl From<ClusterPortConfig> for NetworkMode {
    fn from(config: ClusterPortConfig) -> Self {
        NetworkMode::Cluster(config)
    }
}

impl NetworkMode {
    /// Create a userspace NAT configuration with default settings.
    pub fn user_nat() -> UserNatConfigBuilder {
        UserNatConfigBuilder::default()
    }

    /// Create a cluster port configuration.
    pub fn cluster(cluster_name: &str) -> ClusterPortConfig {
        ClusterPortConfig {
            cluster_name: cluster_name.to_string(),
            static_ip: None,
        }
    }
}

/// Configuration for userspace NAT networking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserNatConfig {
    /// Subnet for the guest network (e.g., "10.0.2.0/24").
    /// Default: 10.0.2.0/24
    pub subnet: String,
    /// Gateway IP within the subnet.
    /// Default: 10.0.2.2
    pub gateway: Ipv4Addr,
    /// First IP to assign via DHCP.
    /// Default: 10.0.2.15
    pub dhcp_start: Ipv4Addr,
    /// Last IP to assign via DHCP.
    /// Default: 10.0.2.254
    pub dhcp_end: Ipv4Addr,
    /// Port forwarding rules (host → guest).
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
    /// Network filtering policy.
    #[serde(default)]
    pub policy: Option<NetworkPolicy>,
}

impl Default for UserNatConfig {
    fn default() -> Self {
        Self {
            subnet: "10.0.2.0/24".to_string(),
            gateway: Ipv4Addr::new(10, 0, 2, 2),
            dhcp_start: Ipv4Addr::new(10, 0, 2, 15),
            dhcp_end: Ipv4Addr::new(10, 0, 2, 254),
            port_forwards: Vec::new(),
            policy: None,
        }
    }
}

/// Builder for UserNatConfig.
#[derive(Debug, Clone, Default)]
pub struct UserNatConfigBuilder {
    config: UserNatConfig,
}

impl UserNatConfigBuilder {
    /// Set the subnet (e.g., "192.168.100.0/24").
    pub fn subnet(mut self, subnet: &str) -> Self {
        self.config.subnet = subnet.to_string();
        // Parse to update gateway and DHCP range
        if let Some((base, _prefix)) = subnet.split_once('/')
            && let Ok(base_ip) = base.parse::<Ipv4Addr>()
        {
            let octets = base_ip.octets();
            self.config.gateway = Ipv4Addr::new(octets[0], octets[1], octets[2], 2);
            self.config.dhcp_start = Ipv4Addr::new(octets[0], octets[1], octets[2], 15);
            self.config.dhcp_end = Ipv4Addr::new(octets[0], octets[1], octets[2], 254);
        }
        self
    }

    /// Forward a TCP port from host to guest.
    pub fn forward_tcp(mut self, host_port: u16, guest_port: u16) -> Self {
        self.config.port_forwards.push(PortForward {
            protocol: Protocol::Tcp,
            host_port,
            guest_port,
        });
        self
    }

    /// Forward a UDP port from host to guest.
    pub fn forward_udp(mut self, host_port: u16, guest_port: u16) -> Self {
        self.config.port_forwards.push(PortForward {
            protocol: Protocol::Udp,
            host_port,
            guest_port,
        });
        self
    }

    /// Set the network filtering policy.
    pub fn policy(mut self, policy: NetworkPolicy) -> Self {
        self.config.policy = Some(policy);
        self
    }

    /// Build the NetworkMode.
    pub fn build(self) -> NetworkMode {
        NetworkMode::UserNat(self.config)
    }
}

impl From<UserNatConfigBuilder> for NetworkMode {
    fn from(builder: UserNatConfigBuilder) -> Self {
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_nat() {
        assert_eq!(NetworkMode::default(), NetworkMode::Nat);
    }

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&NetworkMode::None).unwrap(),
            "\"none\""
        );
        assert_eq!(serde_json::to_string(&NetworkMode::Nat).unwrap(), "\"nat\"");
    }

    #[test]
    fn deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<NetworkMode>("\"none\"").unwrap(),
            NetworkMode::None
        );
        assert_eq!(
            serde_json::from_str::<NetworkMode>("\"nat\"").unwrap(),
            NetworkMode::Nat
        );
    }

    #[test]
    fn user_nat_default_config() {
        let config = UserNatConfig::default();
        assert_eq!(config.subnet, "10.0.2.0/24");
        assert_eq!(config.gateway, Ipv4Addr::new(10, 0, 2, 2));
        assert_eq!(config.dhcp_start, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(config.dhcp_end, Ipv4Addr::new(10, 0, 2, 254));
    }

    #[test]
    fn user_nat_builder() {
        let mode = NetworkMode::user_nat().subnet("192.168.1.0/24").build();
        match mode {
            NetworkMode::UserNat(config) => {
                assert_eq!(config.subnet, "192.168.1.0/24");
                assert_eq!(config.gateway, Ipv4Addr::new(192, 168, 1, 2));
            }
            _ => panic!("Expected UserNat"),
        }
    }

    #[test]
    fn user_nat_into_network_mode() {
        let mode: NetworkMode = NetworkMode::user_nat().into();
        assert!(matches!(mode, NetworkMode::UserNat(_)));
    }

    #[test]
    fn user_nat_port_forwards() {
        let mode = NetworkMode::user_nat()
            .forward_tcp(8080, 80)
            .forward_udp(5353, 53)
            .build();
        match mode {
            NetworkMode::UserNat(config) => {
                assert_eq!(config.port_forwards.len(), 2);
                assert_eq!(config.port_forwards[0].protocol, Protocol::Tcp);
                assert_eq!(config.port_forwards[0].host_port, 8080);
                assert_eq!(config.port_forwards[0].guest_port, 80);
                assert_eq!(config.port_forwards[1].protocol, Protocol::Udp);
                assert_eq!(config.port_forwards[1].host_port, 5353);
                assert_eq!(config.port_forwards[1].guest_port, 53);
            }
            _ => panic!("Expected UserNat"),
        }
    }

    #[test]
    fn network_policy_builders() {
        let policy = NetworkPolicy::deny_all()
            .allow_port(443)
            .allow_dns()
            .allow_ip(Ipv4Addr::new(8, 8, 8, 8));

        assert_eq!(policy.default_action, PolicyAction::Deny);
        assert_eq!(policy.rules.len(), 3);
        assert_eq!(policy.rules[0].action, PolicyAction::Allow);
        assert!(matches!(policy.rules[0].matcher, RuleMatcher::Port(443)));
    }

    #[test]
    fn user_nat_with_policy() {
        let mode = NetworkMode::user_nat()
            .policy(NetworkPolicy::deny_all().allow_https())
            .build();
        match mode {
            NetworkMode::UserNat(config) => {
                let policy = config.policy.expect("Expected policy");
                assert_eq!(policy.default_action, PolicyAction::Deny);
                assert_eq!(policy.rules.len(), 1);
            }
            _ => panic!("Expected UserNat"),
        }
    }

    #[test]
    fn domain_pattern_parse_exact() {
        let pattern = DomainPattern::parse("api.anthropic.com");
        assert!(matches!(pattern, DomainPattern::Exact(_)));
        if let DomainPattern::Exact(s) = pattern {
            assert_eq!(s, "api.anthropic.com");
        }
    }

    #[test]
    fn domain_pattern_parse_wildcard() {
        let pattern = DomainPattern::parse("*.github.com");
        assert!(matches!(pattern, DomainPattern::Wildcard(_)));
        if let DomainPattern::Wildcard(s) = pattern {
            assert_eq!(s, "github.com");
        }
    }

    #[test]
    fn domain_pattern_parse_lowercases() {
        let exact = DomainPattern::parse("API.Anthropic.COM");
        if let DomainPattern::Exact(s) = exact {
            assert_eq!(s, "api.anthropic.com");
        }

        let wildcard = DomainPattern::parse("*.GITHUB.COM");
        if let DomainPattern::Wildcard(s) = wildcard {
            assert_eq!(s, "github.com");
        }
    }

    #[test]
    fn domain_pattern_exact_match() {
        let pattern = DomainPattern::parse("api.anthropic.com");
        assert!(pattern.matches("api.anthropic.com"));
        assert!(pattern.matches("API.ANTHROPIC.COM"));
        assert!(pattern.matches("Api.Anthropic.Com"));
        assert!(!pattern.matches("other.anthropic.com"));
        assert!(!pattern.matches("evil.com"));
        assert!(!pattern.matches("api.anthropic.com.evil.com"));
    }

    #[test]
    fn domain_pattern_wildcard_match() {
        let pattern = DomainPattern::parse("*.github.com");
        assert!(pattern.matches("api.github.com"));
        assert!(pattern.matches("raw.github.com"));
        assert!(pattern.matches("deep.sub.github.com"));
        assert!(pattern.matches("API.GITHUB.COM"));
        assert!(!pattern.matches("github.com"));
        assert!(!pattern.matches("evil-github.com"));
        assert!(!pattern.matches("notgithub.com"));
    }

    #[test]
    fn domain_pattern_serde_roundtrip() {
        let exact = DomainPattern::parse("example.com");
        let json = serde_json::to_string(&exact).unwrap();
        assert_eq!(json, "\"example.com\"");
        let parsed: DomainPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, exact);

        let wildcard = DomainPattern::parse("*.example.com");
        let json = serde_json::to_string(&wildcard).unwrap();
        assert_eq!(json, "\"*.example.com\"");
        let parsed: DomainPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, wildcard);
    }

    #[test]
    fn domain_pattern_wildcard_serialization_preserves_prefix() {
        let wildcard = DomainPattern::Wildcard("github.com".to_string());
        let json = serde_json::to_string(&wildcard).unwrap();
        assert_eq!(json, "\"*.github.com\"");

        let parsed: DomainPattern = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, DomainPattern::Wildcard(ref s) if s == "github.com"));
        assert!(parsed.matches("api.github.com"));
        assert!(!parsed.matches("github.com"));
    }

    #[test]
    fn network_policy_with_wildcard_domain_roundtrip() {
        let policy = NetworkPolicy::deny_all().allow_domain("*.example.com");

        let json = serde_json::to_string(&policy).unwrap();
        let parsed: NetworkPolicy = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.default_action, PolicyAction::Deny);
        assert_eq!(parsed.rules.len(), 1);

        if let RuleMatcher::Domain(pattern) = &parsed.rules[0].matcher {
            assert!(
                matches!(pattern, DomainPattern::Wildcard(s) if s == "example.com"),
                "Expected Wildcard(\"example.com\"), got {:?}",
                pattern
            );
            assert!(pattern.matches("www.example.com"));
            assert!(!pattern.matches("example.com"));
        } else {
            panic!("Expected Domain matcher");
        }
    }

    #[test]
    fn network_policy_domain_builders() {
        let policy = NetworkPolicy::deny_all()
            .allow_domain("api.anthropic.com")
            .deny_domain("*.evil.com")
            .log_domain("*.monitor.com");

        assert_eq!(policy.rules.len(), 3);
        assert_eq!(policy.rules[0].action, PolicyAction::Allow);
        assert!(matches!(
            &policy.rules[0].matcher,
            RuleMatcher::Domain(DomainPattern::Exact(s)) if s == "api.anthropic.com"
        ));
        assert_eq!(policy.rules[1].action, PolicyAction::Deny);
        assert!(matches!(
            &policy.rules[1].matcher,
            RuleMatcher::Domain(DomainPattern::Wildcard(s)) if s == "evil.com"
        ));
        assert_eq!(policy.rules[2].action, PolicyAction::Log);
        assert!(matches!(
            &policy.rules[2].matcher,
            RuleMatcher::Domain(DomainPattern::Wildcard(s)) if s == "monitor.com"
        ));
    }

    #[test]
    fn domain_pattern_rejects_empty_domain() {
        let pattern = DomainPattern::parse("example.com");
        assert!(!pattern.matches(""));
    }

    #[test]
    fn domain_pattern_rejects_oversized_domain() {
        let pattern = DomainPattern::parse("example.com");
        // DNS spec limits domains to 253 characters
        let long_domain = "a".repeat(254);
        assert!(!pattern.matches(&long_domain));
    }

    #[test]
    fn domain_pattern_empty_wildcard_suffix_never_matches() {
        // If someone creates a wildcard with empty suffix (pathological case),
        // it should never match to avoid security issues
        let pattern = DomainPattern::Wildcard(String::new());
        assert!(!pattern.matches("anything.com"));
        assert!(!pattern.matches("evil.example.com"));
    }
}
