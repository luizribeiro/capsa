# Test VMs for aarch64 (ARM64) - used for macOS Virtualization.framework
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

  # Build sandbox binaries from the workspace
  sandboxBinaries = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
    name = "capsa-sandbox";
    src = ../..;
    cargoLock = {
      lockFile = ../../Cargo.lock;
      outputHashes = {
        "apple-main-0.1.0" = "sha256-TnOkBourbvRGFj+eL1D1rAtM183jsrv2Ivq8+JkX/lY=";
      };
    };
    cargoBuildFlags = [ "-p" "capsa-sandbox-init" "-p" "capsa-sandbox-agent" ];
    doCheck = false;
  };

  extraBinaries = [ "${vsockPong}/bin/vsock-pong" ];

  kernel = vmLib.mkKernel {
    name = "universal";
    linuxArch = "arm64";
    inherit kernelImage kernelTarget;
    config = {
      # PCI bus - required for virtio on ARM64
      PCI = true;
      PCI_HOST_COMMON = true;
      PCI_HOST_GENERIC = true;
      VIRTIO_PCI = true;
      VIRTIO_PCI_LIB = true;

      # ARM architecture timer (required for timer interrupts)
      ARM_ARCH_TIMER = true;

      # Disable unused features to reduce kernel size
      IPV6 = false;
    };
  };

  initrd = vmLib.mkInitrd {
    inherit console extraBinaries;
  };

  sandboxInitrd = vmLib.mkSandboxInitrd {
    sandboxInit = "${sandboxBinaries}/bin/capsa-sandbox-init";
    sandboxAgent = "${sandboxBinaries}/bin/capsa-sandbox-agent";
    inherit extraBinaries;
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
      # PCI bus - required for virtio on ARM64
      PCI = true;
      PCI_HOST_COMMON = true;
      PCI_HOST_GENERIC = true;
      VIRTIO_PCI = true;
      VIRTIO_PCI_LIB = true;

      # ARM architecture timer (required for timer interrupts)
      ARM_ARCH_TIMER = true;

      # EFI stub boot
      EFI = true;
      EFI_STUB = true;
      ACPI = true;

      # Baked-in cmdline (EFI stub doesn't pass cmdline from firmware)
      CMDLINE_FORCE = true;
      CMDLINE = "rdinit=/init console=${console}";

      # FAT filesystem - required to read EFI System Partition
      VFAT_FS = true;
      FAT_FS = true;
      FAT_DEFAULT_CODEPAGE = 437;
      FAT_DEFAULT_IOCHARSET = "iso8859-1";

      # Native Language Support - required by FAT for filenames
      NLS = true;
      NLS_CODEPAGE_437 = true;
      NLS_ISO8859_1 = true;

      # Disable unused features to reduce kernel size
      IPV6 = false;
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
    sandbox = vmLib.mkDirectBootVm {
      name = "sandbox";
      inherit kernel kernelImage;
      initrd = sandboxInitrd;
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
