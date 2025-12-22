# Test VMs for Capsa integration tests
# Build with: nix-build test-vms.nix
#
# This builds multiple test VM configurations and outputs a manifest.json
# that maps VM names to their kernel/initrd paths.
#
# TODO: Add ACPI support to test VMs for graceful shutdown testing.
# Currently, vm.stop() times out on these VMs because they don't respond
# to ACPI shutdown requests. Tests use vm.kill() as a workaround.

# TODO: find a better place for this

# TODO: make sure pkgs is always pinned from flake

# TODO: there are probably much better ways to implement everything on this file :)

{ pkgs ? import <nixpkgs> { } }:

let
  linuxPkgs = import pkgs.path {
    system = "aarch64-linux";
    overlays = [];
  };

  # Build a minimal kernel from tinyconfig + only what vfkit needs
  # vfkit uses virtio-PCI (not MMIO) so we need PCI support
  mkMinimalKernel = { networking ? false }: linuxPkgs.stdenv.mkDerivation {
    name = "linux-minimal${if networking then "-net" else ""}";
    src = linuxPkgs.linux.src;

    nativeBuildInputs = with linuxPkgs; [
      flex bison bc perl openssl elfutils
    ];

    dontConfigure = true;

    buildPhase = ''
      # Start with tinyconfig (absolute minimum)
      make ARCH=arm64 tinyconfig

      # Add essential options for vfkit via .config fragment
      cat >> .config << 'EOF'
      # 64-bit kernel (required for arm64)
      CONFIG_64BIT=y

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

      # PCI support (vfkit uses virtio-PCI, not MMIO)
      CONFIG_PCI=y
      CONFIG_PCI_HOST_COMMON=y
      CONFIG_PCI_HOST_GENERIC=y

      # VIRTIO support (PCI-based)
      CONFIG_VIRTIO_MENU=y
      CONFIG_VIRTIO=y
      CONFIG_VIRTIO_PCI=y
      CONFIG_VIRTIO_PCI_LIB=y

      # virtio-console for serial (hvc0)
      CONFIG_HVC_DRIVER=y
      CONFIG_VIRTIO_CONSOLE=y

      # virtio-blk for disk support
      CONFIG_VIRTIO_BLK=y

      # ext4 filesystem support
      CONFIG_EXT4_FS=y

      # Basic initramfs support
      CONFIG_BLK_DEV=y
      CONFIG_BLK_DEV_INITRD=y
      CONFIG_RD_GZIP=y
      EOF

      ${if networking then ''
      # Add networking support
      cat >> .config << 'EOF'
      # Networking core
      CONFIG_NET=y
      CONFIG_INET=y
      CONFIG_PACKET=y
      CONFIG_UNIX=y

      # virtio-net driver
      CONFIG_NETDEVICES=y
      CONFIG_VIRTIO_NET=y

      # Needed for IP configuration
      CONFIG_IPV6=n
      EOF
      '' else ""}

      # Resolve dependencies
      make ARCH=arm64 olddefconfig

      # Build
      make ARCH=arm64 -j$NIX_BUILD_CORES Image
    '';

    installPhase = ''
      mkdir -p $out
      cp arch/arm64/boot/Image $out/Image
      cp .config $out/config
    '';
  };

  minimalKernel = mkMinimalKernel { networking = false; };
  minimalNetKernel = mkMinimalKernel { networking = true; };

  # Kernel with vsock support for host-guest communication testing
  vsockKernel = linuxPkgs.stdenv.mkDerivation {
    name = "linux-vsock";
    src = linuxPkgs.linux.src;

    nativeBuildInputs = with linuxPkgs; [
      flex bison bc perl openssl elfutils
    ];

    dontConfigure = true;

    buildPhase = ''
      make ARCH=arm64 tinyconfig

      cat >> .config << 'EOF'
      # 64-bit kernel
      CONFIG_64BIT=y

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

      # PCI support (vfkit uses virtio-PCI)
      CONFIG_PCI=y
      CONFIG_PCI_HOST_COMMON=y
      CONFIG_PCI_HOST_GENERIC=y

      # VIRTIO support
      CONFIG_VIRTIO_MENU=y
      CONFIG_VIRTIO=y
      CONFIG_VIRTIO_PCI=y
      CONFIG_VIRTIO_PCI_LIB=y

      # virtio-console for serial (hvc0)
      CONFIG_HVC_DRIVER=y
      CONFIG_VIRTIO_CONSOLE=y

      # vsock support for host-guest communication
      CONFIG_NET=y
      CONFIG_VSOCKETS=y
      CONFIG_VIRTIO_VSOCKETS=y

      # initramfs support
      CONFIG_BLK_DEV=y
      CONFIG_BLK_DEV_INITRD=y
      CONFIG_RD_GZIP=y
      EOF

      make ARCH=arm64 olddefconfig
      make ARCH=arm64 -j$NIX_BUILD_CORES Image
    '';

    installPhase = ''
      mkdir -p $out
      cp arch/arm64/boot/Image $out/Image
      cp .config $out/config
    '';
  };

  # Ultra-minimal kernel: absolute fastest boot possible
  # - No SMP (single CPU)
  # - No kernel printk during boot
  # - Minimal security features
  # - LZ4 initrd compression (faster than gzip)
  ultraMinimalKernel = linuxPkgs.stdenv.mkDerivation {
    name = "linux-ultra-minimal";
    src = linuxPkgs.linux.src;

    nativeBuildInputs = with linuxPkgs; [
      flex bison bc perl openssl elfutils
    ];

    dontConfigure = true;

    buildPhase = ''
      make ARCH=arm64 tinyconfig

      cat >> .config << 'EOF'
      # 64-bit kernel
      CONFIG_64BIT=y

      # Disable SMP - single CPU only (faster boot)
      CONFIG_SMP=n

      # Minimal console - just enough for userspace echo
      CONFIG_PRINTK=y
      CONFIG_TTY=y
      CONFIG_UNIX98_PTYS=y

      # Required for running binaries
      CONFIG_BINFMT_ELF=y
      CONFIG_BINFMT_SCRIPT=y

      # Minimal filesystem support
      CONFIG_PROC_FS=y
      CONFIG_SYSFS=y
      CONFIG_DEVTMPFS=y
      CONFIG_DEVTMPFS_MOUNT=y
      CONFIG_TMPFS=y

      # PCI + virtio (required for vfkit)
      CONFIG_PCI=y
      CONFIG_PCI_HOST_COMMON=y
      CONFIG_PCI_HOST_GENERIC=y
      CONFIG_VIRTIO_MENU=y
      CONFIG_VIRTIO=y
      CONFIG_VIRTIO_PCI=y
      CONFIG_VIRTIO_PCI_LIB=y

      # virtio-console for serial
      CONFIG_HVC_DRIVER=y
      CONFIG_VIRTIO_CONSOLE=y

      # initramfs with LZ4 (faster decompression)
      CONFIG_BLOCK=y
      CONFIG_BLK_DEV=y
      CONFIG_BLK_DEV_INITRD=y
      CONFIG_RD_LZ4=y

      # Disable debugging and security overhead
      CONFIG_DEBUG_KERNEL=n
      CONFIG_STACKPROTECTOR=n
      CONFIG_RETPOLINE=n
      CONFIG_BUG=n
      CONFIG_KALLSYMS=n
      CONFIG_PRINTK_TIME=n
      EOF

      make ARCH=arm64 olddefconfig
      make ARCH=arm64 -j$NIX_BUILD_CORES Image
    '';

    installPhase = ''
      mkdir -p $out
      cp arch/arm64/boot/Image $out/Image
      cp .config $out/config
    '';
  };

  # Build the UEFI initramfs directory structure first
  # This will be embedded into the kernel
  uefiInitramfs = linuxPkgs.runCommand "uefi-initramfs" {} ''
    mkdir -p $out/{bin,dev,proc,sys,etc,tmp,mnt}

    cp ${linuxPkgs.pkgsStatic.busybox}/bin/busybox $out/bin/
    for cmd in \
      sh ash \
      ls cat echo pwd mkdir ln rm cp mv touch head tail tee \
      mount umount \
      ps kill sleep \
      grep sed awk cut sort uniq wc tr \
      df du free top uptime uname dmesg \
      vi less more \
      chmod chown id whoami \
      date env printenv export \
      true false test expr \
    ; do
      ln -s busybox $out/bin/$cmd
    done

    cat > $out/init << 'INIT'
    ${mkUefiInitScript}
    INIT
    chmod +x $out/init
  '';

  # UEFI-capable kernel with EFI stub support and embedded initramfs
  # This kernel can boot directly as an EFI application with the initramfs built-in
  uefiKernel = linuxPkgs.stdenv.mkDerivation {
    name = "linux-uefi";
    src = linuxPkgs.linux.src;

    nativeBuildInputs = with linuxPkgs; [
      flex bison bc perl openssl elfutils
    ];

    dontConfigure = true;

    buildPhase = ''
      make ARCH=arm64 tinyconfig

      cat >> .config << EOF
      # 64-bit kernel
      CONFIG_64BIT=y

      # EFI support - required for UEFI boot
      CONFIG_EFI=y
      CONFIG_EFI_STUB=y

      # ACPI support - required for UEFI boot on ARM64
      # UEFI provides ACPI tables for device discovery instead of device tree
      CONFIG_ACPI=y

      # Built-in kernel command line - required for UEFI boot without bootloader
      CONFIG_CMDLINE_FORCE=y
      CONFIG_CMDLINE="rdinit=/init console=hvc0"

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

      # PCI support (vfkit uses virtio-PCI)
      CONFIG_PCI=y
      CONFIG_PCI_HOST_COMMON=y
      CONFIG_PCI_HOST_GENERIC=y

      # VIRTIO support
      CONFIG_VIRTIO_MENU=y
      CONFIG_VIRTIO=y
      CONFIG_VIRTIO_PCI=y
      CONFIG_VIRTIO_PCI_LIB=y

      # virtio-console for serial (hvc0)
      CONFIG_HVC_DRIVER=y
      CONFIG_VIRTIO_CONSOLE=y

      # virtio-blk for disk support
      CONFIG_VIRTIO_BLK=y

      # FAT filesystem for ESP
      CONFIG_VFAT_FS=y
      CONFIG_FAT_FS=y
      CONFIG_FAT_DEFAULT_CODEPAGE=437
      CONFIG_FAT_DEFAULT_IOCHARSET="iso8859-1"
      CONFIG_NLS=y
      CONFIG_NLS_CODEPAGE_437=y
      CONFIG_NLS_ISO8859_1=y

      # ext4 filesystem support
      CONFIG_EXT4_FS=y

      # Embed initramfs directly into kernel - bypasses need for separate initrd loading
      CONFIG_BLK_DEV=y
      CONFIG_BLK_DEV_INITRD=y
      CONFIG_INITRAMFS_SOURCE="${uefiInitramfs}"
      CONFIG_INITRAMFS_ROOT_UID=0
      CONFIG_INITRAMFS_ROOT_GID=0
      CONFIG_INITRAMFS_FORCE=y
      CONFIG_RD_GZIP=y
      EOF

      make ARCH=arm64 olddefconfig
      make ARCH=arm64 -j$NIX_BUILD_CORES Image
    '';

    installPhase = ''
      mkdir -p $out
      cp arch/arm64/boot/Image $out/Image
      cp .config $out/config
    '';
  };

  # Common init script builder
  mkInitScript = { networking ? true }: ''
#!/bin/sh
export PATH=/bin

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

exec < /dev/console > /dev/console 2>&1

echo ""
echo "======================================"
echo "  Capsa Test VM - Boot successful!"
echo "======================================"
echo ""

${if networking then ''
echo "Configuring network..."
if ifconfig eth0 up 2>/dev/null; then
  if udhcpc -i eth0 -s /bin/udhcpc-script -n -q 2>/dev/null; then
    echo "Network configured via DHCP"
    ifconfig eth0
  else
    echo "DHCP failed, no network"
  fi
else
  echo "No network interface found"
fi

echo ""
echo "Try: ping -c 3 8.8.8.8"
'' else ''
echo "Networking disabled"
''}
echo ""

exec sh
  '';

  # Ultra-minimal init script - absolute minimum for boot detection
  ultraMinimalInitScript = ''
#!/bin/sh
mount -t devtmpfs dev /dev
exec < /dev/console > /dev/console 2>&1
echo "Boot successful!"
exec sh
  '';

  # Build ultra-minimal VM with LZ4 compression
  mkUltraMinimalVm = { name }: linuxPkgs.runCommand "capsa-test-vm-${name}" {
    nativeBuildInputs = [ linuxPkgs.cpio linuxPkgs.lz4 ];
  } ''
    mkdir -p $out

    # Copy kernel
    cp ${ultraMinimalKernel}/Image $out/kernel

    # Build minimal initrd
    mkdir -p initrd-root/{bin,dev}

    # Only busybox with minimal symlinks
    cp ${linuxPkgs.pkgsStatic.busybox}/bin/busybox initrd-root/bin/
    for cmd in sh mount echo; do
      ln -s busybox initrd-root/bin/$cmd
    done

    # Minimal init
    cat > initrd-root/init << 'INIT'
    ${ultraMinimalInitScript}
    INIT
    chmod +x initrd-root/init

    # LZ4 compression (faster decompression than gzip)
    (cd initrd-root && find . | cpio -o -H newc | lz4 -l -9) > $out/initrd
  '';

  # Build a test VM with given options
  mkTestVm = { name, networking ? true, kernel ? linuxPkgs.linux }: linuxPkgs.runCommand "capsa-test-vm-${name}" {
    nativeBuildInputs = [ linuxPkgs.cpio linuxPkgs.gzip ];
  } ''
    mkdir -p $out

    # Copy kernel
    cp ${kernel}/Image $out/kernel

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

  mkUefiInitScript = ''
#!/bin/sh
export PATH=/bin

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

exec < /dev/console > /dev/console 2>&1

echo "======================================"
echo "  UEFI Boot successful!"
echo "======================================"

exec sh
  '';

  # Build a UEFI-bootable VM with ESP partition
  # Uses the kernel's EFI stub to boot directly as an EFI application
  mkUefiVm = { name, sizeMB ? 64 }: linuxPkgs.runCommand "capsa-test-vm-${name}" {
    nativeBuildInputs = with linuxPkgs; [
      cpio gzip
      parted dosfstools mtools
    ];
  } ''
    mkdir -p $out

    # Create initrd for the bootloader to load
    (cd ${uefiInitramfs} && find . | cpio -o -H newc | gzip) > initrd.gz

    # Create GPT disk image with ESP partition
    dd if=/dev/zero of=$out/disk.raw bs=1M count=${toString sizeMB}
    parted -s $out/disk.raw mklabel gpt
    parted -s $out/disk.raw mkpart ESP fat32 1MiB 100%
    parted -s $out/disk.raw set 1 esp on

    # Create the ESP filesystem
    ESP_SIZE=$(((${toString sizeMB} - 1) * 1024 * 1024))
    dd if=/dev/zero of=esp.img bs=1 count=$ESP_SIZE
    mkfs.vfat -F 32 -n EFI esp.img

    # Create EFI boot directory structure
    mmd -i esp.img ::/EFI
    mmd -i esp.img ::/EFI/BOOT

    # Copy kernel with embedded initramfs as the default EFI bootloader
    mcopy -i esp.img ${uefiKernel}/Image ::/EFI/BOOT/BOOTAA64.EFI

    # Also copy initrd separately (for reference/debugging)
    mcopy -i esp.img initrd.gz ::/initrd.gz

    # Write the ESP filesystem content to the partition
    dd if=esp.img of=$out/disk.raw bs=512 seek=2048 conv=notrunc

    # Also output the kernel separately for reference (used by Linux direct boot tests)
    cp ${uefiKernel}/Image $out/kernel
    cp initrd.gz $out/initrd
  '';

  # Init script for disk-enabled VMs
  mkDiskInitScript = ''
#!/bin/sh
export PATH=/bin

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

exec < /dev/console > /dev/console 2>&1

echo ""
echo "======================================"
echo "  Capsa Test VM - Boot successful!"
echo "======================================"
echo ""

# Mount disk if present
if [ -e /dev/vda ]; then
  echo "Mounting disk /dev/vda..."
  mkdir -p /mnt
  # Try read-write first, fallback to read-only
  if mount /dev/vda /mnt 2>/dev/null; then
    echo "Disk mounted at /mnt (read-write)"
  elif mount -o ro /dev/vda /mnt; then
    echo "Disk mounted at /mnt (read-only)"
  else
    echo "Failed to mount disk"
  fi
  echo "Disk contents:"
  ls -la /mnt
else
  echo "No disk found at /dev/vda"
fi

echo ""
exec sh
  '';

  # Build a test VM with a disk image
  mkTestVmWithDisk = { name, sizeMB ? 32 }: linuxPkgs.runCommand "capsa-test-vm-${name}" {
    nativeBuildInputs = [ linuxPkgs.cpio linuxPkgs.gzip linuxPkgs.e2fsprogs ];
  } ''
    mkdir -p $out

    # Copy kernel (using minimal kernel which now has disk support)
    cp ${minimalKernel}/Image $out/kernel

    # Build initrd
    mkdir -p initrd-root/{bin,dev,proc,sys,etc,tmp,mnt}

    # Add busybox and symlinks
    cp ${linuxPkgs.pkgsStatic.busybox}/bin/busybox initrd-root/bin/
    for cmd in \
      sh ash \
      ls cat echo pwd mkdir ln rm cp mv touch head tail tee \
      mount umount \
      ps kill sleep \
      grep sed awk cut sort uniq wc tr \
      df du free top uptime uname dmesg \
      vi less more \
      tar gzip gunzip \
      chmod chown id whoami \
      date env printenv export \
      true false test expr \
    ; do
      ln -s busybox initrd-root/bin/$cmd
    done

    # Add init script with disk mounting
    cat > initrd-root/init << 'INIT'
    ${mkDiskInitScript}
    INIT
    chmod +x initrd-root/init

    # Create initrd
    (cd initrd-root && find . | cpio -o -H newc | gzip) > $out/initrd

    # Create disk image
    dd if=/dev/zero of=$out/disk.raw bs=1M count=${toString sizeMB}
    mkfs.ext4 -L rootfs $out/disk.raw
  '';

  # vsock-pong C program for vsock integration testing
  vsockPongSrc = ./test-programs/vsock-pong.c;

  vsockPong = linuxPkgs.stdenv.mkDerivation {
    name = "vsock-pong";
    src = vsockPongSrc;
    dontUnpack = true;
    buildPhase = ''
      ${linuxPkgs.pkgsStatic.stdenv.cc}/bin/aarch64-unknown-linux-musl-cc \
        -static -O2 -o vsock-pong $src
    '';
    installPhase = ''
      mkdir -p $out/bin
      cp vsock-pong $out/bin/
    '';
  };

  # Init script for vsock test VM - starts vsock-pong automatically
  mkVsockInitScript = { port }: ''
#!/bin/sh
export PATH=/bin

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

exec < /dev/console > /dev/console 2>&1

echo ""
echo "======================================"
echo "  Capsa vsock Test VM"
echo "======================================"
echo ""
echo "Starting vsock-pong on port ${toString port}..."
/bin/vsock-pong ${toString port} &
echo "vsock-pong started in background"
echo ""
exec sh
  '';

  # Build a test VM with vsock support and vsock-pong program
  mkVsockTestVm = { name, port ? 1024 }: linuxPkgs.runCommand "capsa-test-vm-${name}" {
    nativeBuildInputs = [ linuxPkgs.cpio linuxPkgs.gzip ];
  } ''
    mkdir -p $out

    # Copy vsock-enabled kernel
    cp ${vsockKernel}/Image $out/kernel

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
      df du free top uptime uname dmesg \
      vi less more \
      chmod chown id whoami \
      date env printenv export \
      true false test expr \
    ; do
      ln -s busybox initrd-root/bin/$cmd
    done

    # Add vsock-pong program
    cp ${vsockPong}/bin/vsock-pong initrd-root/bin/

    # Add init script that starts vsock-pong
    cat > initrd-root/init << 'INIT'
    ${mkVsockInitScript { inherit port; }}
    INIT
    chmod +x initrd-root/init

    # Create initrd
    (cd initrd-root && find . | cpio -o -H newc | gzip) > $out/initrd
  '';

  # Define our test VMs
  vms = {
    default = mkTestVm { name = "default"; networking = true; };
    no-network = mkTestVm { name = "no-network"; networking = false; };
    minimal = mkTestVm { name = "minimal"; networking = false; kernel = minimalKernel; };
    minimal-net = mkTestVm { name = "minimal-net"; networking = true; kernel = minimalNetKernel; };
    ultra-minimal = mkUltraMinimalVm { name = "ultra-minimal"; };
    with-disk = mkTestVmWithDisk { name = "with-disk"; };
    uefi = mkUefiVm { name = "uefi"; };
    vsock = mkVsockTestVm { name = "vsock"; port = 1024; };
  };

  # VMs that have disk images
  vmsWithDisk = [ "with-disk" "uefi" ];

  # UEFI VMs need special manifest handling
  uefiVms = [ "uefi" ];

  combined = linuxPkgs.runCommand "capsa-test-vms" {} ''
    mkdir -p $out

    # Link each VM's outputs
    ${builtins.concatStringsSep "\n" (builtins.attrValues (builtins.mapAttrs (name: vm: ''
      mkdir -p $out/${name}
      ln -s ${vm}/kernel $out/${name}/kernel
      ln -s ${vm}/initrd $out/${name}/initrd
      ${if builtins.elem name vmsWithDisk then "ln -s ${vm}/disk.raw $out/${name}/disk.raw" else ""}
    '') vms))}

    # Generate manifest.json
    cat > $out/manifest.json << 'EOF'
    ${builtins.toJSON (builtins.mapAttrs (name: vm:
      { kernel = "${vm}/kernel"; initrd = "${vm}/initrd"; }
      // (if builtins.elem name vmsWithDisk then { disk = "${vm}/disk.raw"; } else {})
      // (if builtins.elem name uefiVms then { is_uefi = true; } else {})
    ) vms)}
    EOF
  '';

in vms // { inherit combined minimalKernel minimalNetKernel ultraMinimalKernel uefiKernel vsockKernel; }
