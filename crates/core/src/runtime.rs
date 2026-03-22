use std::collections::HashSet;

use anyhow::{ensure, Result};

use crate::{libkrun, ResolvedNetworkInterface, VmConfig, VmmLaunchSpec};

#[doc(hidden)]
pub fn start_vm(spec: &VmmLaunchSpec) -> Result<()> {
    let config = &spec.vm_config;
    let resolved_interfaces = &spec.resolved_interfaces;

    config.validate()?;
    validate_vmm_launch_spec(config, resolved_interfaces)?;

    libkrun::init_logging(config.verbosity)?;

    let vm = libkrun::KrunVm::new()?
        .configure(config.vcpus, config.memory_mib)?
        .configure_host_tty_console()?;

    let vm = configure_network(vm, resolved_interfaces)?;

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

fn validate_vmm_launch_spec(
    config: &VmConfig,
    resolved: &[ResolvedNetworkInterface],
) -> Result<()> {
    ensure!(
        resolved.len() == config.interfaces.len(),
        "resolved interfaces count ({}) does not match declared interfaces count ({})",
        resolved.len(),
        config.interfaces.len(),
    );

    let mut seen_fds = HashSet::new();
    for (i, iface) in resolved.iter().enumerate() {
        ensure!(
            iface.guest_fd >= 0,
            "interface {i}: invalid guest_fd {}",
            iface.guest_fd
        );
        ensure!(
            seen_fds.insert(iface.guest_fd),
            "interface {i}: duplicate guest_fd {}",
            iface.guest_fd
        );
        ensure!(
            iface.mac != [0u8; 6],
            "interface {i}: MAC address is all zeros"
        );
    }

    Ok(())
}

fn configure_network(
    mut vm: libkrun::KrunVm<libkrun::Configured>,
    interfaces: &[ResolvedNetworkInterface],
) -> Result<libkrun::KrunVm<libkrun::Configured>> {
    for (fd, mut mac, features, flags) in network_call_params(interfaces) {
        vm = vm.add_network_unixstream(fd, &mut mac, features, flags)?;
    }

    Ok(vm)
}

fn network_call_params(interfaces: &[ResolvedNetworkInterface]) -> Vec<(i32, [u8; 6], u32, u32)> {
    interfaces
        .iter()
        .map(|iface| (iface.guest_fd, iface.mac, libkrun::compat_net_features(), 0))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::{network_call_params, validate_envelope};
    use crate::{NetworkInterfaceConfig, ResolvedNetworkInterface, VmConfig};

    fn base_vm_config() -> VmConfig {
        VmConfig {
            root: Some("/tmp/root".into()),
            kernel: None,
            initramfs: None,
            kernel_cmdline: None,
            vcpus: 1,
            memory_mib: 512,
            verbosity: 0,
            interfaces: vec![],
        }
    }

    #[test]
    fn network_call_params_is_empty_for_empty_interfaces() {
        let observed = network_call_params(&[]);
        assert!(observed.is_empty());
    }

    #[test]
    fn network_call_params_matches_single_interface_values() {
        let interfaces = vec![ResolvedNetworkInterface {
            mac: [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
            guest_fd: 42,
        }];

        let observed = network_call_params(&interfaces);

        assert_eq!(
            observed,
            vec![(
                42,
                [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
                crate::libkrun::compat_net_features(),
                0,
            )]
        );
    }

    #[test]
    fn validate_vmm_launch_spec_rejects_mismatched_interface_counts() {
        let config = base_vm_config();
        let resolved = vec![ResolvedNetworkInterface {
            mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            guest_fd: 10,
        }];

        let err = validate_vmm_launch_spec(&config, &resolved).expect_err("validation should fail");
        assert!(err.to_string().contains(
            "resolved interfaces count (1) does not match declared interfaces count (0)"
        ));
    }

    #[test]
    fn validate_vmm_launch_spec_rejects_negative_guest_fd() {
        let mut config = base_vm_config();
        config.interfaces.push(NetworkInterfaceConfig { mac: None });

        let resolved = vec![ResolvedNetworkInterface {
            mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            guest_fd: -1,
        }];

        let err = validate_vmm_launch_spec(&config, &resolved).expect_err("validation should fail");
        assert!(err.to_string().contains("interface 0: invalid guest_fd -1"));
    }

    #[test]
    fn validate_vmm_launch_spec_rejects_duplicate_guest_fd() {
        let mut config = base_vm_config();
        config.interfaces.push(NetworkInterfaceConfig { mac: None });
        config.interfaces.push(NetworkInterfaceConfig { mac: None });

        let resolved = vec![
            ResolvedNetworkInterface {
                mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
                guest_fd: 10,
            },
            ResolvedNetworkInterface {
                mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x02],
                guest_fd: 10,
            },
        ];

        let err = validate_vmm_launch_spec(&config, &resolved).expect_err("validation should fail");
        assert!(err
            .to_string()
            .contains("interface 1: duplicate guest_fd 10"));
    }

    #[test]
    fn validate_vmm_launch_spec_rejects_all_zero_mac() {
        let mut config = base_vm_config();
        config.interfaces.push(NetworkInterfaceConfig { mac: None });

        let resolved = vec![ResolvedNetworkInterface {
            mac: [0, 0, 0, 0, 0, 0],
            guest_fd: 10,
        }];

        let err = validate_vmm_launch_spec(&config, &resolved).expect_err("validation should fail");
        assert!(err
            .to_string()
            .contains("interface 0: MAC address is all zeros"));
    }

    #[test]
    fn validate_vmm_launch_spec_accepts_consistent_data() {
        let mut config = base_vm_config();
        config.interfaces.push(NetworkInterfaceConfig { mac: None });

        let resolved = vec![ResolvedNetworkInterface {
            mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            guest_fd: 10,
        }];

        validate_vmm_launch_spec(&config, &resolved).expect("validation should pass");
    }
}
