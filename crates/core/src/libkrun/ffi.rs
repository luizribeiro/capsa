use std::ffi::c_char;

pub(super) const KRUN_LOG_LEVEL_ERROR: u32 = 1;
pub(super) const KRUN_LOG_LEVEL_INFO: u32 = 3;
pub(super) const KRUN_LOG_LEVEL_DEBUG: u32 = 4;
pub(super) const KRUN_LOG_STYLE_AUTO: u32 = 0;
pub(super) const KRUN_LOG_OPTION_NO_ENV: u32 = 1;
pub(super) const KRUN_KERNEL_FORMAT_RAW: u32 = 0;
pub(super) const KRUN_KERNEL_FORMAT_ELF: u32 = 1;
pub(super) const KRUN_KERNEL_FORMAT_IMAGE_BZ2: u32 = 3;
pub(super) const KRUN_KERNEL_FORMAT_IMAGE_GZ: u32 = 4;
pub(super) const KRUN_KERNEL_FORMAT_IMAGE_ZSTD: u32 = 5;
pub(super) const VIRGLRENDERER_VENUS: u32 = 1 << 6;
pub(super) const VIRGLRENDERER_NO_VIRGL: u32 = 1 << 7;

#[allow(dead_code)]
pub(super) const NET_FEATURE_CSUM: u32 = 1 << 0;
#[allow(dead_code)]
pub(super) const NET_FEATURE_GUEST_CSUM: u32 = 1 << 1;
#[allow(dead_code)]
pub(super) const NET_FEATURE_GUEST_TSO4: u32 = 1 << 7;
#[allow(dead_code)]
pub(super) const NET_FEATURE_GUEST_UFO: u32 = 1 << 10;
#[allow(dead_code)]
pub(super) const NET_FEATURE_HOST_TSO4: u32 = 1 << 11;
#[allow(dead_code)]
pub(super) const NET_FEATURE_HOST_UFO: u32 = 1 << 14;

#[allow(dead_code)]
pub(super) const COMPAT_NET_FEATURES: u32 = NET_FEATURE_CSUM
    | NET_FEATURE_GUEST_CSUM
    | NET_FEATURE_GUEST_TSO4
    | NET_FEATURE_GUEST_UFO
    | NET_FEATURE_HOST_TSO4
    | NET_FEATURE_HOST_UFO;

unsafe extern "C" {
    pub(super) fn krun_create_ctx() -> i32;
    pub(super) fn krun_free_ctx(ctx_id: u32) -> i32;
    pub(super) fn krun_init_log(target_fd: i32, level: u32, style: u32, options: u32) -> i32;
    pub(super) fn krun_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    pub(super) fn krun_set_gpu_options2(ctx_id: u32, virgl_flags: u32, shm_size: u64) -> i32;
    pub(super) fn krun_set_root(ctx_id: u32, root_path: *const c_char) -> i32;
    pub(super) fn krun_set_kernel(
        ctx_id: u32,
        kernel_path: *const c_char,
        kernel_format: u32,
        initramfs: *const c_char,
        cmdline: *const c_char,
    ) -> i32;
    pub(super) fn krun_set_kernel_console(ctx_id: u32, console_id: *const c_char) -> i32;
    pub(super) fn krun_disable_implicit_console(ctx_id: u32) -> i32;
    pub(super) fn krun_add_virtio_console_default(
        ctx_id: u32,
        input_fd: i32,
        output_fd: i32,
        err_fd: i32,
    ) -> i32;
    #[allow(dead_code)]
    pub(super) fn krun_add_net_unixstream(
        ctx_id: u32,
        c_path: *const c_char,
        fd: i32,
        c_mac: *mut u8,
        features: u32,
        flags: u32,
    ) -> i32;
    #[allow(dead_code)]
    pub(super) fn krun_add_net_unixgram(
        ctx_id: u32,
        c_path: *const c_char,
        fd: i32,
        c_mac: *mut u8,
        features: u32,
        flags: u32,
    ) -> i32;
    pub(super) fn krun_get_shutdown_eventfd(ctx_id: u32) -> i32;
    pub(super) fn krun_start_enter(ctx_id: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{
        COMPAT_NET_FEATURES, NET_FEATURE_CSUM, NET_FEATURE_GUEST_CSUM, NET_FEATURE_GUEST_TSO4,
        NET_FEATURE_GUEST_UFO, NET_FEATURE_HOST_TSO4, NET_FEATURE_HOST_UFO,
    };

    #[test]
    fn compat_net_features_is_expected_union() {
        let expected = NET_FEATURE_CSUM
            | NET_FEATURE_GUEST_CSUM
            | NET_FEATURE_GUEST_TSO4
            | NET_FEATURE_GUEST_UFO
            | NET_FEATURE_HOST_TSO4
            | NET_FEATURE_HOST_UFO;
        assert_eq!(COMPAT_NET_FEATURES, expected);
    }
}
