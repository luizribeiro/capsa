# Test VMs for x86_64 - used for Linux KVM backend
#
# Build with: nix-build nix/test-vms -A x86_64

{ nixpkgs ? <nixpkgs> }:

let
  pkgs = import nixpkgs { system = "x86_64-linux"; };
  vmLib = import ./lib.nix { inherit pkgs; };

  kernelImage = "bzImage";
  kernelTarget = "bzImage";

  kernel = vmLib.mkKernel {
    name = "universal";
    linuxArch = "x86";
    inherit kernelImage kernelTarget;
    config = {
      X86_64 = true;
      SERIAL_8250 = true;
      SERIAL_8250_CONSOLE = true;
    };
  };

  initrd = vmLib.mkInitrd {
    console = "ttyS0";
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
