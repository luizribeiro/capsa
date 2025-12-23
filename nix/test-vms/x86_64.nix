# Test VMs for x86_64 - used for Linux KVM backend
#
# Build with: nix-build nix/test-vms -A x86_64

{ nixpkgs ? <nixpkgs> }:

let
  pkgs = import nixpkgs { system = "x86_64-linux"; };
  vmLib = import ./lib.nix { inherit pkgs; };

  linuxArch = "x86";
  kernelImage = "bzImage";
  kernelTarget = "bzImage";
  console = "ttyS0";

  kernelConfig = {
    X86_64 = true;
    SERIAL_8250 = true;
    SERIAL_8250_CONSOLE = true;
  };

  universalKernel = vmLib.mkKernel {
    name = "universal";
    inherit linuxArch kernelImage kernelTarget;
    config = kernelConfig;
  };

  universalInitrd = vmLib.mkInitrd { inherit console; };

  vms = {
    default = vmLib.mkDirectBootVm {
      name = "default";
      kernel = universalKernel;
      inherit kernelImage;
      initrd = universalInitrd;
    };
    with-disk = vmLib.mkDirectBootVm {
      name = "with-disk";
      kernel = universalKernel;
      inherit kernelImage;
      initrd = universalInitrd;
      disk = { sizeMB = 32; };
    };
  };
in
vmLib.mkCombined {
  name = "x86_64";
  inherit vms;
}
