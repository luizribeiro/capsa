use crate::boot::LinuxDirectBootConfig;
use crate::builder::LinuxVmBuilder;

// TODO: do we really need both VmConfig and InternalVmConfig? seems a bit odd tbh
pub trait VmConfig {
    type Builder;

    fn into_builder(self) -> Self::Builder;
}

impl VmConfig for LinuxDirectBootConfig {
    type Builder = LinuxVmBuilder;

    fn into_builder(self) -> LinuxVmBuilder {
        LinuxVmBuilder::new(self)
    }
}

pub struct Capsa;

impl Capsa {
    pub fn vm<C: VmConfig>(config: C) -> C::Builder {
        config.into_builder()
    }

    // TODO: add vm_pool convenience method, which returns a poolable builder
}
