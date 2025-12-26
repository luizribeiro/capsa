//! VM creation and startup logic.

use crate::delegate::{StopSender, VmStateDelegate};
use block2::RcBlock;
use capsa_core::{BootMethod, DiskImage, Error, NetworkMode, Result, VsockConfig};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_virtualization::{
    VZDiskImageStorageDeviceAttachment, VZEFIBootLoader, VZEFIVariableStore,
    VZEFIVariableStoreInitializationOptions, VZFileHandleNetworkDeviceAttachment,
    VZLinuxBootLoader, VZNATNetworkDeviceAttachment, VZStorageDeviceConfiguration,
    VZVirtioBlockDeviceConfiguration, VZVirtioConsoleDeviceSerialPortConfiguration,
    VZVirtioNetworkDeviceConfiguration, VZVirtioSocketDeviceConfiguration, VZVirtualMachine,
    VZVirtualMachineConfiguration,
};
use std::os::fd::OwnedFd;
use std::os::unix::io::RawFd;

pub struct CreateVmConfig {
    pub boot: BootMethod,
    pub cpus: u32,
    pub memory_mb: u64,
    pub root_disk: Option<DiskImage>,
    pub disks: Vec<DiskImage>,
    pub network: NetworkMode,
    /// Guest-side file descriptor for UserNat or Cluster networking.
    ///
    /// When NetworkMode::UserNat or NetworkMode::Cluster is configured, this must
    /// contain the guest-side fd from SocketPairDevice::new().into_raw_fd() or
    /// a SwitchPort. The fd ownership is transferred to NSFileHandle; the caller
    /// must not close it after passing.
    ///
    /// Must be None for other network modes (Nat, None).
    pub network_guest_fd: Option<RawFd>,
    pub vsock: VsockConfig,
    pub console_input_read_fd: Option<RawFd>,
    pub console_output_write_fd: Option<RawFd>,
    pub stop_sender: StopSender,
}

pub fn create_pipe() -> Result<(OwnedFd, OwnedFd)> {
    nix::unistd::pipe().map_err(|e| Error::StartFailed(format!("Failed to create pipe: {}", e)))
}

pub fn create_vm(config: CreateVmConfig) -> Result<(usize, usize)> {
    // SAFETY: This block uses Objective-C FFI via objc2 bindings to Apple's
    // Virtualization.framework. The safety requirements are:
    // 1. All objc2 types (NSURL, NSString, VZ*) are used according to their API contracts
    // 2. The VZVirtualMachine is converted to a raw pointer via Retained::into_raw to
    //    transfer ownership out of this function. The caller (via NativeVmHandle) is
    //    responsible for eventually reclaiming it with Retained::from_raw.
    // 3. File descriptors passed for console I/O must be valid open descriptors.
    //    NSFileHandle takes ownership and will manage their lifetime.
    unsafe {
        let vm_config = VZVirtualMachineConfiguration::new();

        match &config.boot {
            BootMethod::LinuxDirect {
                kernel,
                initrd,
                cmdline,
            } => {
                let kernel_path_str = kernel
                    .to_str()
                    .ok_or_else(|| Error::StartFailed("Invalid kernel path".to_string()))?;
                let initrd_path_str = initrd
                    .to_str()
                    .ok_or_else(|| Error::StartFailed("Invalid initrd path".to_string()))?;

                let kernel_url = NSURL::fileURLWithPath(&NSString::from_str(kernel_path_str));
                let boot_loader =
                    VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url);

                let initrd_url = NSURL::fileURLWithPath(&NSString::from_str(initrd_path_str));
                boot_loader.setInitialRamdiskURL(Some(&initrd_url));
                boot_loader.setCommandLine(&NSString::from_str(cmdline));

                vm_config.setBootLoader(Some(&boot_loader));
            }
            BootMethod::Uefi {
                efi_variable_store,
                create_variable_store,
            } => {
                let store_path_str = efi_variable_store
                    .to_str()
                    .ok_or_else(|| Error::StartFailed("Invalid EFI store path".to_string()))?;
                let store_url = NSURL::fileURLWithPath(&NSString::from_str(store_path_str));

                let variable_store = if *create_variable_store {
                    VZEFIVariableStore::initCreatingVariableStoreAtURL_options_error(
                        VZEFIVariableStore::alloc(),
                        &store_url,
                        VZEFIVariableStoreInitializationOptions::AllowOverwrite,
                    )
                    .map_err(|e| {
                        Error::StartFailed(format!("Failed to create EFI variable store: {}", e))
                    })?
                } else {
                    VZEFIVariableStore::initWithURL(VZEFIVariableStore::alloc(), &store_url)
                };

                let boot_loader = VZEFIBootLoader::init(VZEFIBootLoader::alloc());
                boot_loader.setVariableStore(Some(&variable_store));

                vm_config.setBootLoader(Some(&boot_loader));
            }
        }
        vm_config.setCPUCount(config.cpus as usize);
        vm_config.setMemorySize(config.memory_mb * 1024 * 1024);

        // Collect all disks: root_disk first, then additional disks
        let all_disks: Vec<&DiskImage> =
            config.root_disk.iter().chain(config.disks.iter()).collect();

        if !all_disks.is_empty() {
            let mut block_configs: Vec<Retained<VZStorageDeviceConfiguration>> = Vec::new();

            for disk in all_disks {
                let disk_path_str = disk
                    .path
                    .to_str()
                    .ok_or_else(|| Error::StartFailed("Invalid disk path".to_string()))?;
                let disk_url = NSURL::fileURLWithPath(&NSString::from_str(disk_path_str));

                let disk_attachment =
                    VZDiskImageStorageDeviceAttachment::initWithURL_readOnly_error(
                        VZDiskImageStorageDeviceAttachment::alloc(),
                        &disk_url,
                        disk.read_only,
                    )
                    .map_err(|e| {
                        Error::StartFailed(format!("Failed to create disk attachment: {}", e))
                    })?;

                let block_config = VZVirtioBlockDeviceConfiguration::initWithAttachment(
                    VZVirtioBlockDeviceConfiguration::alloc(),
                    &disk_attachment,
                );

                block_configs.push(Retained::into_super(block_config));
            }

            let storage_configs: Retained<objc2_foundation::NSArray<VZStorageDeviceConfiguration>> =
                objc2_foundation::NSArray::from_retained_slice(&block_configs);
            vm_config.setStorageDevices(&storage_configs);
        }

        match &config.network {
            NetworkMode::Nat => {
                let net_attachment = VZNATNetworkDeviceAttachment::new();
                let net_config = VZVirtioNetworkDeviceConfiguration::new();
                net_config.setAttachment(Some(&net_attachment));

                let net_configs: Retained<
                    objc2_foundation::NSArray<objc2_virtualization::VZNetworkDeviceConfiguration>,
                > = objc2_foundation::NSArray::from_retained_slice(&[
                    objc2::rc::Retained::into_super(net_config),
                ]);
                vm_config.setNetworkDevices(&net_configs);
            }
            NetworkMode::UserNat(_) | NetworkMode::Cluster(_) => {
                let mode_name = match &config.network {
                    NetworkMode::UserNat(_) => "UserNat",
                    NetworkMode::Cluster(_) => "Cluster",
                    _ => unreachable!(),
                };
                let guest_fd = config.network_guest_fd.ok_or_else(|| {
                    Error::StartFailed(format!(
                        "{} network mode requires network_guest_fd",
                        mode_name
                    ))
                })?;

                // SAFETY: NSFileHandle::initWithFileDescriptor takes ownership of the fd.
                // The caller must ensure the fd was created via into_raw_fd() and not close it.
                let file_handle = objc2_foundation::NSFileHandle::initWithFileDescriptor(
                    objc2_foundation::NSFileHandle::alloc(),
                    guest_fd,
                );
                let net_attachment = VZFileHandleNetworkDeviceAttachment::initWithFileHandle(
                    VZFileHandleNetworkDeviceAttachment::alloc(),
                    &file_handle,
                );
                let net_config = VZVirtioNetworkDeviceConfiguration::new();
                net_config.setAttachment(Some(&net_attachment));

                let net_configs: Retained<
                    objc2_foundation::NSArray<objc2_virtualization::VZNetworkDeviceConfiguration>,
                > = objc2_foundation::NSArray::from_retained_slice(&[
                    objc2::rc::Retained::into_super(net_config),
                ]);
                vm_config.setNetworkDevices(&net_configs);
            }
            NetworkMode::None => {}
        }

        if let (Some(read_fd), Some(write_fd)) =
            (config.console_input_read_fd, config.console_output_write_fd)
        {
            let read_handle = objc2_foundation::NSFileHandle::initWithFileDescriptor(
                objc2_foundation::NSFileHandle::alloc(),
                read_fd,
            );
            let write_handle = objc2_foundation::NSFileHandle::initWithFileDescriptor(
                objc2_foundation::NSFileHandle::alloc(),
                write_fd,
            );

            let serial_attachment = objc2_virtualization::VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                objc2_virtualization::VZFileHandleSerialPortAttachment::alloc(),
                Some(&read_handle),
                Some(&write_handle),
            );

            let serial_config = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            serial_config.setAttachment(Some(&serial_attachment));

            let serial_configs: Retained<
                objc2_foundation::NSArray<objc2_virtualization::VZSerialPortConfiguration>,
            > = objc2_foundation::NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                serial_config,
            )]);
            vm_config.setSerialPorts(&serial_configs);
        }

        if !config.vsock.ports.is_empty() {
            let vsock_config = VZVirtioSocketDeviceConfiguration::new();
            let socket_configs: Retained<
                objc2_foundation::NSArray<objc2_virtualization::VZSocketDeviceConfiguration>,
            > = objc2_foundation::NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                vsock_config,
            )]);
            vm_config.setSocketDevices(&socket_configs);
        }

        vm_config
            .validateWithError()
            .map_err(|e| Error::StartFailed(format!("VM config validation failed: {}", e)))?;

        let vm = VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &vm_config);

        let mtm = MainThreadMarker::new().expect("create_vm must run on main thread");
        let delegate = VmStateDelegate::new(mtm, config.stop_sender);
        vm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        let vm_ptr = Retained::into_raw(vm);
        let delegate_ptr = Retained::into_raw(delegate);
        Ok((vm_ptr as usize, delegate_ptr as usize))
    }
}

/// Returns the socket device address from a VM, if vsock is configured.
///
/// # Safety
/// `vm_addr` must be a valid pointer to a VZVirtualMachine.
pub fn get_socket_device_addr(vm_addr: usize) -> usize {
    unsafe {
        let ptr = vm_addr as *const VZVirtualMachine;
        let vm = &*ptr;
        let socket_devices = vm.socketDevices();

        if socket_devices.count() == 0 {
            return 0;
        }

        // Get the first socket device (we only configure one)
        let device = socket_devices.objectAtIndex(0);
        Retained::as_ptr(&device) as usize
    }
}

pub fn start_vm(
    vm_addr: usize,
    result_tx: tokio::sync::oneshot::Sender<std::result::Result<(), String>>,
) {
    // SAFETY: This function reclaims ownership of a VZVirtualMachine from a raw pointer.
    // Requirements:
    // 1. vm_addr must have been created by create_vm via Retained::into_raw
    // 2. This function temporarily takes ownership to call startWithCompletionHandler,
    //    then releases it back via into_raw so the VM continues to exist
    // 3. The completion handler is leaked via mem::forget because the Objective-C runtime
    //    retains it and we cannot know when it will be released
    // 4. The error pointer in the completion handler is only dereferenced if non-null,
    //    as guaranteed by the Virtualization.framework API contract
    unsafe {
        let ptr = vm_addr as *mut VZVirtualMachine;
        let vm = Retained::from_raw(ptr).expect("Invalid VM pointer");

        let result_tx = std::sync::Mutex::new(Some(result_tx));
        let completion_handler = RcBlock::new(move |error: *mut NSError| {
            if let Some(tx) = result_tx.lock().unwrap().take() {
                if error.is_null() {
                    let _ = tx.send(Ok(()));
                } else {
                    // SAFETY: error is non-null, and the Virtualization.framework guarantees
                    // it points to a valid NSError when the start operation fails
                    let err = &*error;
                    let _ = tx.send(Err(err.localizedDescription().to_string()));
                }
            }
        });

        std::mem::forget(completion_handler.clone());

        vm.startWithCompletionHandler(&completion_handler);

        let _ = Retained::into_raw(vm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    mod pipe_creation {
        use super::*;

        #[test]
        fn create_pipe_returns_two_fds() {
            let result = create_pipe();
            assert!(result.is_ok());

            let (read_fd, write_fd) = result.unwrap();
            assert!(read_fd.as_raw_fd() >= 0);
            assert!(write_fd.as_raw_fd() >= 0);
            assert_ne!(read_fd.as_raw_fd(), write_fd.as_raw_fd());
        }

        #[test]
        fn pipe_is_readable_and_writable() {
            let (read_fd, write_fd) = create_pipe().unwrap();

            let test_data = b"hello";
            let written = nix::unistd::write(&write_fd, test_data).unwrap();
            assert_eq!(written, test_data.len());

            let mut buf = [0u8; 5];
            let read = nix::unistd::read(read_fd.as_raw_fd(), &mut buf).unwrap();
            assert_eq!(read, test_data.len());
            assert_eq!(&buf, test_data);
        }
    }
}
