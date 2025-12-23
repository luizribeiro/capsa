#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for UEFI boot functionality.

#[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
use capsa::test_utils::{BOOT_SUCCESS_MESSAGE, test_uefi_vm};
#[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
use std::time::Duration;

#[apple_main::harness_test]
async fn test_uefi_vm_boots_successfully() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support UEFI boot yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit UEFI boot not working (use native backend)");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        let vm = test_uefi_vm("uefi")
            .build()
            .await
            .expect("Failed to build UEFI VM");

        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout(BOOT_SUCCESS_MESSAGE, Duration::from_secs(30))
            .await
            .expect("UEFI VM did not boot successfully");

        vm.kill().await.expect("Failed to stop UEFI VM");
    }
}
