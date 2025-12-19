use crate::builder::LinuxVmBuilder;
use capsa_core::LinuxDirectBootConfig;

pub struct Capsa;

impl Capsa {
    pub fn linux(config: LinuxDirectBootConfig) -> LinuxVmBuilder {
        LinuxVmBuilder::new(config)
    }

    // TODO: add vm_pool convenience method, which returns a poolable builder
}
