pub(crate) mod config;
mod dns;
pub mod frame;
mod gateway;
mod nat;
pub mod switch;
mod util;

pub use gateway::{GatewayStack, GatewayStackConfig};
pub use switch::{SwitchPort, VirtualSwitch};

pub use switch::bridge::bridge_to_switch;
pub use switch::socketpair::SocketPairDevice;
