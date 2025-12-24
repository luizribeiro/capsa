# Test VMs for aarch64 (ARM64) - used for macOS vfkit/Virtualization.framework
#
# Build with: nix-build nix/test-vms -A aarch64

{ nixpkgs ? <nixpkgs> }:

let
  pkgs = import nixpkgs { system = "aarch64-linux"; };
  vmLib = import ./lib.nix { inherit pkgs; };

  kernelImage = "Image";
  kernelTarget = "Image";
  console = "hvc0";

  vsockPong = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
    name = "vsock-pong";
    src = ../../crates/test-utils/vsock-pong;
    cargoLock.lockFile = ../../crates/test-utils/vsock-pong/Cargo.lock;
  };

  extraBinaries = [ "${vsockPong}/bin/vsock-pong" ];

  kernel = vmLib.mkKernel {
    name = "universal";
    linuxArch = "arm64";
    inherit kernelImage kernelTarget;
    config = {
      PCI = true;
      PCI_HOST_COMMON = true;
      PCI_HOST_GENERIC = true;
      VIRTIO_MENU = true;
      VIRTIO_PCI_LIB = true;
      HVC_DRIVER = true;
      VIRTIO_CONSOLE = true;
      IPV6 = false;
    };
  };

  initrd = vmLib.mkInitrd {
    inherit console extraBinaries;
  };

  uefiInitramfsDir = vmLib.mkInitramfsDir {
    inherit console extraBinaries;
  };

  uefiKernel = vmLib.mkKernel {
    name = "uefi";
    linuxArch = "arm64";
    inherit kernelImage kernelTarget;
    initramfsDir = uefiInitramfsDir;
    config = {
      PCI = true;
      PCI_HOST_COMMON = true;
      PCI_HOST_GENERIC = true;
      VIRTIO_MENU = true;
      VIRTIO_PCI_LIB = true;
      HVC_DRIVER = true;
      VIRTIO_CONSOLE = true;
      IPV6 = false;
      EFI = true;
      EFI_STUB = true;
      ACPI = true;
      CMDLINE_FORCE = true;
      CMDLINE = "rdinit=/init console=${console}";
      VFAT_FS = true;
      FAT_FS = true;
      FAT_DEFAULT_CODEPAGE = 437;
      FAT_DEFAULT_IOCHARSET = "iso8859-1";
      NLS = true;
      NLS_CODEPAGE_437 = true;
      NLS_ISO8859_1 = true;
    };
  };
in
vmLib.mkCombined {
  name = "aarch64";
  vms = {
    default = vmLib.mkDirectBootVm {
      name = "default";
      inherit kernel kernelImage initrd;
    };
    with-disk = vmLib.mkDirectBootVm {
      name = "with-disk";
      inherit kernel kernelImage initrd;
      disk = { sizeMB = 32; };
    };
    uefi = vmLib.mkUefiVm {
      name = "uefi";
      kernel = uefiKernel;
      inherit kernelImage;
      uefiBootloader = "BOOTAA64.EFI";
      initramfsDir = uefiInitramfsDir;
    };
  };
}
