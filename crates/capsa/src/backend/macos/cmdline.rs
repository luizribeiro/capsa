use capsa_core::KernelCmdline;

pub fn macos_cmdline_defaults() -> KernelCmdline {
    let mut cmdline = KernelCmdline::new();
    cmdline.console("hvc0");
    cmdline.arg("reboot", "t");
    cmdline.arg("panic", "-1");
    cmdline
}

pub const DEFAULT_ROOT_DEVICE: &str = "/dev/vda";
