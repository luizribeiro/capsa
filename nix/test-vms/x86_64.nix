# Test VMs for x86_64 - used for Linux KVM backend
#
# Build with: nix-build nix/test-vms -A x86_64

{ nixpkgs ? <nixpkgs> }:

let
  pkgs = import nixpkgs { system = "x86_64-linux"; };
  vmLib = import ./lib.nix { inherit pkgs; };

  kernelImage = "bzImage";
  kernelTarget = "bzImage";

  vsockPong = pkgs.pkgsStatic.rustPlatform.buildRustPackage {
    name = "vsock-pong";
    src = ../../crates/test-utils/vsock-pong;
    cargoLock.lockFile = ../../crates/test-utils/vsock-pong/Cargo.lock;
  };

  extraBinaries = [ "${vsockPong}/bin/vsock-pong" ];

  kernel = vmLib.mkKernel {
    name = "x86_64";
    linuxArch = "x86";
    inherit kernelImage kernelTarget;
    config = {
      # Architecture
      X86_64 = true;

      # LZ4 compression for faster decompression (XZ is default but slow)
      KERNEL_LZ4 = true;

      # Virtio MMIO transport (used by KVM backend)
      VIRTIO_MMIO = true;
      VIRTIO_MMIO_CMDLINE_DEVICES = true;

      # IOAPIC support for interrupt routing
      X86_LOCAL_APIC = true;
      X86_IO_APIC = true;
      X86_MPPARSE = true;

      # Serial console
      SERIAL_8250 = true;
      SERIAL_8250_CONSOLE = true;
    };
  };

  initrd = vmLib.mkInitrd {
    console = "hvc0";
    inherit extraBinaries;
  };
in
vmLib.mkCombined {
  name = "x86_64";
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
  };
}
