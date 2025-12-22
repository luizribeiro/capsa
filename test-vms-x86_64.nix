# Test VMs for Capsa integration tests (x86_64)
# Build with: nix-build test-vms-x86_64.nix -A combined -o result-vms
#
# This builds x86_64 test VM configurations for KVM backend testing.

{ pkgs ? import <nixpkgs> { } }:

let
  linuxPkgs = import pkgs.path {
    system = "x86_64-linux";
    overlays = [];
  };

  # Build a minimal x86_64 kernel for KVM
  mkMinimalKernel = { networking ? false }: linuxPkgs.stdenv.mkDerivation {
    name = "linux-minimal-x86_64${if networking then "-net" else ""}";
    src = linuxPkgs.linux.src;

    nativeBuildInputs = with linuxPkgs; [
      flex bison bc perl openssl elfutils
    ];

    dontConfigure = true;

    buildPhase = ''
      # Start with tinyconfig
      make ARCH=x86 tinyconfig

      cat >> .config << 'EOF'
      # 64-bit kernel
      CONFIG_64BIT=y
      CONFIG_X86_64=y

      # Basic requirements
      CONFIG_PRINTK=y
      CONFIG_BUG=y
      CONFIG_BINFMT_ELF=y
      CONFIG_BINFMT_SCRIPT=y
      CONFIG_TTY=y
      CONFIG_UNIX98_PTYS=y
      CONFIG_PROC_FS=y
      CONFIG_SYSFS=y
      CONFIG_DEVTMPFS=y
      CONFIG_DEVTMPFS_MOUNT=y
      CONFIG_TMPFS=y
      CONFIG_BLOCK=y

      # Serial console (ttyS0 for KVM)
      CONFIG_SERIAL_8250=y
      CONFIG_SERIAL_8250_CONSOLE=y

      # virtio-blk for disk support
      CONFIG_VIRTIO=y
      CONFIG_VIRTIO_PCI=y
      CONFIG_VIRTIO_BLK=y

      # ext4 filesystem support
      CONFIG_EXT4_FS=y

      # Basic initramfs support
      CONFIG_BLK_DEV=y
      CONFIG_BLK_DEV_INITRD=y
      CONFIG_RD_GZIP=y
      EOF

      ${if networking then ''
      cat >> .config << 'EOF'
      # Networking core
      CONFIG_NET=y
      CONFIG_INET=y
      CONFIG_PACKET=y
      CONFIG_UNIX=y

      # virtio-net driver
      CONFIG_NETDEVICES=y
      CONFIG_VIRTIO_NET=y
      EOF
      '' else ""}

      make ARCH=x86 olddefconfig
      make ARCH=x86 -j$NIX_BUILD_CORES bzImage
    '';

    installPhase = ''
      mkdir -p $out
      cp arch/x86/boot/bzImage $out/bzImage
      cp .config $out/config
    '';
  };

  minimalKernel = mkMinimalKernel { networking = false; };
  minimalNetKernel = mkMinimalKernel { networking = true; };

  # Common init script builder
  mkInitScript = { networking ? true }: ''
#!/bin/sh
export PATH=/bin

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

echo "Boot successful" > /dev/ttyS0
echo "" > /dev/ttyS0

${if networking then ''
echo "Configuring network..." > /dev/ttyS0
'' else ''
echo "Networking disabled" > /dev/ttyS0
''}

# Run interactive shell on serial console
exec sh </dev/ttyS0 >/dev/ttyS0 2>&1
  '';

  # Build a test VM with given options
  mkTestVm = { name, networking ? true, kernel ? minimalNetKernel }: linuxPkgs.runCommand "capsa-test-vm-${name}" {
    nativeBuildInputs = [ linuxPkgs.cpio linuxPkgs.gzip ];
  } ''
    mkdir -p $out

    # Copy kernel
    cp ${kernel}/bzImage $out/kernel

    # Build initrd
    mkdir -p initrd-root/{bin,dev,proc,sys,etc,tmp}

    # Add busybox and symlinks
    cp ${linuxPkgs.pkgsStatic.busybox}/bin/busybox initrd-root/bin/
    for cmd in \
      sh ash \
      ls cat echo pwd mkdir ln rm cp mv touch head tail tee \
      mount umount \
      ps kill sleep \
      grep sed awk cut sort uniq wc tr \
      ${if networking then "ping ping6 ifconfig ip route netstat wget nc hostname nslookup udhcpc" else ""} \
      df du free top uptime uname dmesg \
      vi less more \
      tar gzip gunzip \
      chmod chown id whoami \
      date env printenv export \
      true false test expr \
    ; do
      ln -s busybox initrd-root/bin/$cmd
    done

    # Add init script
    cat > initrd-root/init << 'INIT'
    ${mkInitScript { inherit networking; }}
    INIT
    chmod +x initrd-root/init

    ${if networking then ''
    # Add udhcpc script for DHCP
    cat > initrd-root/bin/udhcpc-script << 'DHCP'
#!/bin/sh
case "$1" in
  bound|renew)
    ifconfig $interface $ip netmask $subnet
    if [ -n "$router" ]; then
      route add default gw $router
    fi
    if [ -n "$dns" ]; then
      echo "nameserver $dns" > /etc/resolv.conf
    fi
    ;;
esac
DHCP
    chmod +x initrd-root/bin/udhcpc-script
    '' else ""}

    # Create initrd
    (cd initrd-root && find . | cpio -o -H newc | gzip) > $out/initrd
  '';

  # Define our test VMs
  vms = {
    default = mkTestVm { name = "default"; networking = true; };
    no-network = mkTestVm { name = "no-network"; networking = false; kernel = minimalKernel; };
    minimal = mkTestVm { name = "minimal"; networking = false; kernel = minimalKernel; };
    minimal-net = mkTestVm { name = "minimal-net"; networking = true; kernel = minimalNetKernel; };
  };

  combined = linuxPkgs.runCommand "capsa-test-vms-x86_64" {} ''
    mkdir -p $out

    # Link each VM's outputs
    ${builtins.concatStringsSep "\n" (builtins.attrValues (builtins.mapAttrs (name: vm: ''
      mkdir -p $out/${name}
      ln -s ${vm}/kernel $out/${name}/kernel
      ln -s ${vm}/initrd $out/${name}/initrd
    '') vms))}

    # Generate manifest.json
    cat > $out/manifest.json << 'EOF'
    ${builtins.toJSON (builtins.mapAttrs (name: vm:
      { kernel = "${vm}/kernel"; initrd = "${vm}/initrd"; }
    ) vms)}
    EOF
  '';

in vms // { inherit combined minimalKernel minimalNetKernel; }
