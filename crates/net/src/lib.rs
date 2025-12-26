mod cluster_stack;
mod device;
mod dhcp;
mod dns_cache;
mod error;
mod frame_io;
mod nat;
mod policy;
mod port_forward;
mod stack;
mod switch;

#[cfg(unix)]
mod bridge;
#[cfg(unix)]
mod socketpair;

pub use cluster_stack::{ClusterStack, ClusterStackConfig};
pub use device::SmoltcpDevice;
pub use dhcp::DhcpServer;
pub use dns_cache::DnsCache;
pub use error::NetError;
pub use frame_io::FrameIO;
pub use policy::{PacketInfo, PacketProtocol, PolicyChecker, PolicyResult};
pub use port_forward::{ForwardConfig, PortForwarder};
pub use stack::{PortForwardRule, StackConfig, UserNatStack};
pub use switch::{SwitchPort, VirtualSwitch};

#[cfg(unix)]
pub use bridge::bridge_to_switch;
#[cfg(unix)]
pub use socketpair::SocketPairDevice;
