use std::ffi::CString;
use std::marker::PhantomData;
use std::path::Path;
use std::ptr;

use anyhow::{anyhow, bail, Context, Result};

use super::error::{check_rc, os_error_from_neg_errno};
use super::ffi;
use crate::boot::kernel_format::{detect_kernel_image_format, KernelImageFormat};

use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;

pub(crate) fn init_logging(verbosity: u8) -> Result<()> {
    let level = match verbosity {
        0 => ffi::KRUN_LOG_LEVEL_ERROR,
        1 => ffi::KRUN_LOG_LEVEL_INFO,
        _ => ffi::KRUN_LOG_LEVEL_DEBUG,
    };

    // SAFETY: primitive scalar arguments to libkrun.
    check_rc(
        unsafe {
            ffi::krun_init_log(
                std::io::stderr().as_raw_fd(),
                level,
                ffi::KRUN_LOG_STYLE_AUTO,
                ffi::KRUN_LOG_OPTION_NO_ENV,
            )
        },
        "failed to initialize libkrun logging",
    )
}

pub(crate) struct Created;
pub(crate) struct Configured;
pub(crate) struct BootConfigured;

pub(crate) struct KrunVm<State> {
    ctx: KrunContext,
    _state: PhantomData<State>,
}

struct KrunContext {
    id: u32,
}

impl Drop for KrunContext {
    fn drop(&mut self) {
        // SAFETY: id originates from successful krun_create_ctx.
        let rc = unsafe { ffi::krun_free_ctx(self.id) };
        if cfg!(debug_assertions) && rc < 0 {
            eprintln!(
                "warning: krun_free_ctx({}) failed: {}",
                self.id,
                os_error_from_neg_errno(rc)
            );
        }
    }
}

impl KrunVm<Created> {
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: no args.
        let ctx_id = unsafe { ffi::krun_create_ctx() };
        if ctx_id < 0 {
            bail!(
                "failed to create libkrun context: {}",
                os_error_from_neg_errno(ctx_id)
            );
        }

        Ok(Self {
            ctx: KrunContext { id: ctx_id as u32 },
            _state: PhantomData,
        })
    }

    pub(crate) fn configure(self, vcpus: u8, memory_mib: u32) -> Result<KrunVm<Configured>> {
        // SAFETY: primitive scalar arguments.
        check_rc(
            unsafe { ffi::krun_set_vm_config(self.ctx.id, vcpus, memory_mib) },
            "failed to set VM config",
        )?;

        // Mirror krunkit defaults on macOS.
        let virgl_flags = ffi::VIRGLRENDERER_VENUS | ffi::VIRGLRENDERER_NO_VIRGL;
        let rounded_mem_gib = (u64::from(memory_mib) / 1024 + 1) * 1024;
        let vram = (63488u64.saturating_sub(rounded_mem_gib)) * 1024 * 1024;
        let _ = unsafe { ffi::krun_set_gpu_options2(self.ctx.id, virgl_flags, vram) };

        Ok(self.into_state())
    }
}

impl KrunVm<Configured> {
    pub(crate) fn configure_host_tty_console(self) -> Result<Self> {
        let kernel_console = CString::new("hvc0").expect("static string without NUL");

        // SAFETY: pointer valid for duration of call.
        check_rc(
            unsafe { ffi::krun_set_kernel_console(self.ctx.id, kernel_console.as_ptr()) },
            "failed to set kernel console",
        )?;

        // SAFETY: disable implicit console so we only have one deterministic
        // console path for both input and output.
        check_rc(
            unsafe { ffi::krun_disable_implicit_console(self.ctx.id) },
            "failed to disable implicit console",
        )?;

        // SAFETY: pass through host stdio file descriptors for interactive console.
        check_rc(
            unsafe { ffi::krun_add_virtio_console_default(self.ctx.id, 0, 1, 2) },
            "failed to attach virtio console to host stdio",
        )?;

        Ok(self)
    }

    #[allow(dead_code)]
    pub(crate) fn add_network_unixstream(
        self,
        fd: i32,
        mac: &mut [u8; 6],
        features: u32,
        flags: u32,
    ) -> Result<Self> {
        add_network_unixstream_with(
            self.ctx.id,
            fd,
            mac,
            features,
            flags,
            |ctx_id, fd, mac_ptr, features, flags| {
                // We create SOCK_DGRAM endpoints in launcher (`UnixDatagram::pair`),
                // so use the unixgram backend explicitly.
                // SAFETY: passes primitive scalars and caller-provided valid MAC pointer.
                unsafe {
                    ffi::krun_add_net_unixgram(
                        ctx_id,
                        std::ptr::null(), // no path, fd-based
                        fd,
                        mac_ptr,
                        features,
                        flags,
                    )
                }
            },
        )?;

        Ok(self)
    }

    pub(crate) fn set_kernel(
        self,
        kernel: &Path,
        initramfs: Option<&Path>,
        kernel_cmdline: Option<&str>,
    ) -> Result<KrunVm<BootConfigured>> {
        let kernel_format = detect_kernel_image_format(kernel)
            .map(map_kernel_image_format)
            .with_context(|| format!("failed to detect kernel format for {}", kernel.display()))?;
        let kernel = path_to_cstring(kernel).context("kernel path contains NUL")?;
        let initramfs = initramfs
            .map(|path| path_to_cstring(path).context("initramfs path contains NUL"))
            .transpose()?;
        let initramfs_ptr = initramfs.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        let cmdline =
            CString::new(kernel_cmdline.unwrap_or("")).context("kernel cmdline contains NUL")?;

        // SAFETY: pointers are valid for duration of call.
        check_rc(
            unsafe {
                ffi::krun_set_kernel(
                    self.ctx.id,
                    kernel.as_ptr(),
                    kernel_format,
                    initramfs_ptr,
                    cmdline.as_ptr(),
                )
            },
            "failed to configure kernel",
        )?;

        Ok(self.into_state())
    }

    pub(crate) fn set_root(self, root: &Path) -> Result<KrunVm<BootConfigured>> {
        let root = path_to_cstring(root).context("root path contains NUL")?;

        // SAFETY: pointer valid for duration of call.
        check_rc(
            unsafe { ffi::krun_set_root(self.ctx.id, root.as_ptr()) },
            "failed to configure VM root",
        )?;

        Ok(self.into_state())
    }
}

impl KrunVm<BootConfigured> {
    pub(crate) fn start_enter(self) -> Result<()> {
        // SAFETY: optional preflight used by krunkit.
        let _ = unsafe { ffi::krun_get_shutdown_eventfd(self.ctx.id) };

        check_rc(
            unsafe { ffi::krun_start_enter(self.ctx.id) },
            "failed to start VM",
        )
    }
}

impl<State> KrunVm<State> {
    fn into_state<NextState>(self) -> KrunVm<NextState> {
        KrunVm {
            ctx: self.ctx,
            _state: PhantomData,
        }
    }
}

#[allow(dead_code)]
fn add_network_unixstream_with<F>(
    ctx_id: u32,
    fd: i32,
    mac: &mut [u8; 6],
    features: u32,
    flags: u32,
    call: F,
) -> Result<()>
where
    F: FnOnce(u32, i32, *mut u8, u32, u32) -> i32,
{
    let rc = call(ctx_id, fd, mac.as_mut_ptr(), features, flags);
    check_rc(rc, "failed to add network device")
}

fn map_kernel_image_format(format: KernelImageFormat) -> u32 {
    match format {
        KernelImageFormat::Raw => ffi::KRUN_KERNEL_FORMAT_RAW,
        KernelImageFormat::Elf => ffi::KRUN_KERNEL_FORMAT_ELF,
        KernelImageFormat::ImageBz2 => ffi::KRUN_KERNEL_FORMAT_IMAGE_BZ2,
        KernelImageFormat::ImageGz => ffi::KRUN_KERNEL_FORMAT_IMAGE_GZ,
        KernelImageFormat::ImageZstd => ffi::KRUN_KERNEL_FORMAT_IMAGE_ZSTD,
    }
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|e| anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::add_network_unixstream_with;

    #[test]
    fn add_network_unixstream_succeeds_on_non_negative_rc() {
        let mut mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let result = add_network_unixstream_with(7, 9, &mut mac, 1, 2, |_, _, _, _, _| 0);
        assert!(result.is_ok());
    }

    #[test]
    fn add_network_unixstream_forwards_all_arguments() {
        let mut mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let expected_mac_ptr = mac.as_mut_ptr() as usize;
        let mut observed = None;

        add_network_unixstream_with(
            7,
            9,
            &mut mac,
            0x1234,
            0x5678,
            |ctx_id, fd, mac_ptr, features, flags| {
                observed = Some((ctx_id, fd, mac_ptr as usize, features, flags));
                0
            },
        )
        .expect("ffi wrapper should succeed");

        assert_eq!(
            observed,
            Some((7, 9, expected_mac_ptr, 0x1234, 0x5678)),
            "wrapper should forward ctx/fd/mac/features/flags unchanged"
        );
    }

    #[test]
    fn add_network_unixstream_maps_negative_rc_to_contextual_error() {
        let mut mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let err = add_network_unixstream_with(7, 9, &mut mac, 0, 0, |_, _, _, _, _| -22)
            .expect_err("negative rc should fail");

        let msg = err.to_string();
        assert!(msg.contains("failed to add network device"));
        assert!(msg.contains("os error 22"));
    }
}
