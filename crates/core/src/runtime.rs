use anyhow::Result;

use crate::{libkrun, VmConfig};

#[doc(hidden)]
pub fn start_vm(config: &VmConfig) -> Result<()> {
    libkrun::init_logging(config.verbosity)?;

    let vm = libkrun::KrunVm::new()?
        .configure(config.vcpus, config.memory_mib)?
        .configure_host_tty_console()?;

    if let Some(kernel) = &config.kernel {
        let kernel_cmdline = effective_kernel_cmdline(config);
        let vm = vm.set_kernel(
            kernel,
            config.initramfs.as_deref(),
            kernel_cmdline.as_deref(),
        )?;
        return vm.start_enter();
    }

    if let Some(root) = &config.root {
        return vm.set_root(root)?.start_enter();
    }

    anyhow::bail!("missing boot source: pass either --root <dir> or --kernel <path>")
}

fn effective_kernel_cmdline(config: &VmConfig) -> Option<String> {
    let mut cmdline = KernelCmdline::new();

    match config.verbosity {
        0 => cmdline.push_segment("quiet loglevel=0"),
        1 => {}
        _ => cmdline.push_segment("ignore_loglevel loglevel=7"),
    }

    if let Some(user_cmdline) = config.kernel_cmdline.as_deref() {
        cmdline.push_segment(user_cmdline);
    }

    Some(cmdline.render())
}

#[derive(Debug, Default)]
struct KernelCmdline {
    segments: Vec<String>,
}

impl KernelCmdline {
    fn new() -> Self {
        Self::default()
    }

    fn push_segment(&mut self, segment: &str) {
        let trimmed = segment.trim();
        if trimmed.is_empty() {
            return;
        }
        self.segments.push(trimmed.to_string());
    }

    fn render(self) -> String {
        self.segments.join(" ")
    }
}
