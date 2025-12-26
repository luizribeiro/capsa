# Test VM builder utilities
#
# Pure utility functions for building minimal Linux kernels and VMs.
# These are used by the architecture-specific files (aarch64.nix, x86_64.nix).

{ pkgs }:

let
  lib = pkgs.lib;

  configToString = config:
    lib.concatStringsSep "\n" (lib.mapAttrsToList (k: v:
      if v == true then "CONFIG_${k}=y"
      else if v == false then "CONFIG_${k}=n"
      else if lib.isString v then ''CONFIG_${k}="${v}"''
      else "CONFIG_${k}=${toString v}"
    ) config);

  busyboxCommands = [
    "sh" "ash"
    "ls" "cat" "echo" "pwd" "mkdir" "ln" "rm" "cp" "mv" "touch" "head" "tail" "tee"
    "mount" "umount"
    "ps" "kill" "sleep"
    "grep" "sed" "awk" "cut" "sort" "uniq" "wc" "tr"
    "ping" "ping6" "ifconfig" "ip" "route" "netstat" "wget" "nc" "hostname" "nslookup" "udhcpc"
    "df" "du" "free" "top" "uptime" "uname" "dmesg"
    "vi" "less" "more"
    "tar" "gzip" "gunzip"
    "chmod" "chown" "id" "whoami"
    "date" "env" "printenv" "export"
    "true" "false" "test" "expr"
  ];

  mkInitScript = { console }: ''
#!/bin/sh
export PATH=/bin

mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev

${if console == "hvc0" then ''
exec < /dev/console > /dev/console 2>&1
'' else ''
exec < /dev/${console} > /dev/${console} 2>&1
''}

echo ""
echo "======================================"
echo "  Capsa Test VM - Boot successful!"
echo "======================================"
echo ""

if ifconfig eth0 up 2>/dev/null; then
  if udhcpc -i eth0 -s /bin/udhcpc-script -n -q 2>/dev/null; then
    echo "Network configured via DHCP"
  fi
fi

if [ -e /dev/vda ]; then
  mkdir -p /mnt
  if mount /dev/vda /mnt 2>/dev/null; then
    echo "Disk mounted at /mnt"
  fi
fi

exec sh
  '';

  udhcpcScript = ''
#!/bin/sh
case "$1" in
  bound|renew)
    ifconfig $interface $ip netmask $subnet
    [ -n "$router" ] && route add default gw $router
    [ -n "$dns" ] && echo "nameserver $dns" > /etc/resolv.conf
    ;;
esac
  '';

  # Kernel options shared across all architectures. Architecture-specific
  # configs (console drivers, PCI, etc.) are defined in each arch file.
  sharedKernelConfig = {
    # Core kernel features
    "64BIT" = true;           # 64-bit kernel
    PRINTK = true;            # kernel logging
    BUG = true;               # BUG()/WARN() support for debugging

    # Timer support (required for alarm(), setitimer(), etc.)
    TICK_ONESHOT = true;      # one-shot tick mode for hrtimers
    HIGH_RES_TIMERS = true;   # high-resolution timers (needed for alarm())
    POSIX_TIMERS = true;      # POSIX timer syscalls

    # Binary execution
    BINFMT_ELF = true;        # run ELF binaries (busybox, vsock-pong)
    BINFMT_SCRIPT = true;     # run #! scripts (init, udhcpc-script)

    # TTY subsystem (required for any console)
    TTY = true;
    UNIX98_PTYS = true;       # pseudo-terminals

    # Virtual filesystems
    PROC_FS = true;           # /proc (process info, system stats)
    SYSFS = true;             # /sys (device/driver info)
    DEVTMPFS = true;          # /dev (auto-populated device nodes)
    DEVTMPFS_MOUNT = true;    # mount devtmpfs at boot
    TMPFS = true;             # tmpfs for /tmp, /run

    # Block device and initrd support
    BLOCK = true;
    BLK_DEV = true;
    BLK_DEV_INITRD = true;    # boot from initramfs
    RD_LZ4 = true;            # LZ4-compressed initrd (faster decompression)

    # Virtio (paravirtualized I/O for VMs)
    VIRTIO = true;
    VIRTIO_MENU = true;
    VIRTIO_BLK = true;

    # Virtio console (hvc0)
    HVC_DRIVER = true;
    VIRTIO_CONSOLE = true;

    # Filesystems
    EXT4_FS = true;           # ext4 for disk images

    # Networking stack
    NET = true;
    INET = true;              # IPv4
    PACKET = true;            # raw packet access (for DHCP)
    UNIX = true;              # unix domain sockets

    # Network devices
    NETDEVICES = true;
    VIRTIO_NET = true;        # virtio network (eth0)

    # VSock (VM sockets for host-guest communication)
    VSOCKETS = true;
    VIRTIO_VSOCKETS = true;
  };

  mkKernel = { name, linuxArch, kernelImage, kernelTarget, config ? {}, initramfsDir ? null }:
    let
      fullConfig = sharedKernelConfig // config;
    in pkgs.stdenv.mkDerivation {
      name = "linux-${name}";
      src = pkgs.linux.src;
      nativeBuildInputs = with pkgs; [ flex bison bc perl openssl elfutils lz4 ];
      dontConfigure = true;

      buildPhase = ''
        make ARCH=${linuxArch} tinyconfig

        cat >> .config << 'EOF'
        ${configToString fullConfig}
        ${lib.optionalString (initramfsDir != null) ''
        CONFIG_INITRAMFS_SOURCE="${initramfsDir}"
        CONFIG_INITRAMFS_ROOT_UID=0
        CONFIG_INITRAMFS_ROOT_GID=0
        CONFIG_INITRAMFS_FORCE=y
        ''}
        EOF

        make ARCH=${linuxArch} olddefconfig
        make ARCH=${linuxArch} -j$NIX_BUILD_CORES ${kernelTarget}
      '';

      installPhase = ''
        mkdir -p $out
        cp arch/${linuxArch}/boot/${kernelImage} $out/${kernelImage}
        cp .config $out/config
      '';
    };

  mkInitrd = { console, extraBinaries ? [] }:
    let initScript = mkInitScript { inherit console; };
    in pkgs.runCommand "initrd" {
      nativeBuildInputs = [ pkgs.cpio pkgs.lz4 ];
    } ''
      mkdir -p initrd-root/{bin,dev,proc,sys,etc,tmp,mnt}

      cp ${pkgs.pkgsStatic.busybox}/bin/busybox initrd-root/bin/
      for cmd in ${lib.concatStringsSep " " busyboxCommands}; do
        ln -s busybox initrd-root/bin/$cmd
      done

      ${lib.concatMapStringsSep "\n" (bin: "cp ${bin} initrd-root/bin/") extraBinaries}

      cat > initrd-root/init << 'INIT'
      ${initScript}
      INIT
      chmod +x initrd-root/init

      cat > initrd-root/bin/udhcpc-script << 'DHCP'
      ${udhcpcScript}
      DHCP
      chmod +x initrd-root/bin/udhcpc-script

      (cd initrd-root && find . | cpio -o -H newc | lz4 -l) > $out
    '';

  mkInitramfsDir = { console, extraBinaries ? [] }:
    let initScript = mkInitScript { inherit console; };
    in pkgs.runCommand "initramfs-dir" {} ''
      mkdir -p $out/{bin,dev,proc,sys,etc,tmp,mnt}

      cp ${pkgs.pkgsStatic.busybox}/bin/busybox $out/bin/
      for cmd in ${lib.concatStringsSep " " busyboxCommands}; do
        ln -s busybox $out/bin/$cmd
      done

      ${lib.concatMapStringsSep "\n" (bin: "cp ${bin} $out/bin/") extraBinaries}

      cat > $out/init << 'INIT'
      ${initScript}
      INIT
      chmod +x $out/init

      cat > $out/bin/udhcpc-script << 'DHCP'
      ${udhcpcScript}
      DHCP
      chmod +x $out/bin/udhcpc-script
    '';

  # Sandbox initrd uses capsa-sandbox-init as PID 1 and includes the agent
  mkSandboxInitrd = { sandboxInit, sandboxAgent, extraBinaries ? [] }:
    pkgs.runCommand "sandbox-initrd" {
      nativeBuildInputs = [ pkgs.cpio pkgs.lz4 ];
    } ''
      mkdir -p initrd-root/{bin,dev,proc,sys,etc,tmp,mnt}

      # Busybox for shell access (debugging)
      cp ${pkgs.pkgsStatic.busybox}/bin/busybox initrd-root/bin/
      for cmd in ${lib.concatStringsSep " " busyboxCommands}; do
        ln -s busybox initrd-root/bin/$cmd
      done

      ${lib.concatMapStringsSep "\n" (bin: "cp ${bin} initrd-root/bin/") extraBinaries}

      # Sandbox init as PID 1
      cp ${sandboxInit} initrd-root/init
      chmod +x initrd-root/init

      # Sandbox agent
      cp ${sandboxAgent} initrd-root/sandbox-agent
      chmod +x initrd-root/sandbox-agent

      (cd initrd-root && find . | cpio -o -H newc | lz4 -l) > $out
    '';

  mkDirectBootVm = { name, kernel, kernelImage, initrd, disk ? null }:
    let
      manifest = { kernel = "kernel"; initrd = "initrd"; }
        // lib.optionalAttrs (disk != null) { disk = "disk.raw"; };
    in pkgs.runCommand "capsa-test-vm-${name}" {
      nativeBuildInputs = lib.optional (disk != null) pkgs.e2fsprogs;
    } ''
      mkdir -p $out
      cp ${kernel}/${kernelImage} $out/kernel
      cp ${initrd} $out/initrd
      ${lib.optionalString (disk != null) ''
        dd if=/dev/zero of=$out/disk.raw bs=1M count=${toString disk.sizeMB}
        mkfs.ext4 -L rootfs $out/disk.raw
      ''}
      cat > $out/manifest.json << 'EOF'
      ${builtins.toJSON manifest}
      EOF
    '';

  mkUefiVm = { name, kernel, kernelImage, uefiBootloader, initramfsDir, sizeMB ? 64 }:
    let
      manifest = { kernel = "kernel"; initrd = "initrd"; disk = "disk.raw"; is_uefi = true; };
    in pkgs.runCommand "capsa-test-vm-${name}" {
      nativeBuildInputs = with pkgs; [ cpio lz4 parted dosfstools mtools ];
    } ''
      mkdir -p $out

      dd if=/dev/zero of=$out/disk.raw bs=1M count=${toString sizeMB}
      parted -s $out/disk.raw mklabel gpt
      parted -s $out/disk.raw mkpart ESP fat32 1MiB 100%
      parted -s $out/disk.raw set 1 esp on

      ESP_SIZE=$(((${toString sizeMB} - 1) * 1024 * 1024))
      dd if=/dev/zero of=esp.img bs=1 count=$ESP_SIZE
      mkfs.vfat -F 32 -n EFI esp.img

      mmd -i esp.img ::/EFI
      mmd -i esp.img ::/EFI/BOOT
      mcopy -i esp.img ${kernel}/${kernelImage} ::/EFI/BOOT/${uefiBootloader}

      dd if=esp.img of=$out/disk.raw bs=512 seek=2048 conv=notrunc

      cp ${kernel}/${kernelImage} $out/kernel
      (cd ${initramfsDir} && find . | cpio -o -H newc | lz4 -l) > $out/initrd

      cat > $out/manifest.json << 'EOF'
      ${builtins.toJSON manifest}
      EOF
    '';

  mkCombined = { name, vms }:
    pkgs.runCommand "capsa-test-vms-${name}" {
      nativeBuildInputs = [ pkgs.jq ];
    } ''
      mkdir -p $out

      ${builtins.concatStringsSep "\n" (builtins.attrValues (builtins.mapAttrs (vmName: vm: ''
        ln -s ${vm} $out/${vmName}
      '') vms))}

      # Merge individual manifests, prefixing paths with VM name
      echo '{' > $out/manifest.json
      first=true
      for vm in $out/*/; do
        vmName=$(basename "$vm")
        $first || echo ',' >> $out/manifest.json
        first=false
        echo -n "\"$vmName\":" >> $out/manifest.json
        jq --arg prefix "$vmName" 'with_entries(.value = if .value | type == "string" then "\($prefix)/\(.value)" else .value end)' "$vm/manifest.json" >> $out/manifest.json
      done
      echo '}' >> $out/manifest.json
    '';

in {
  inherit
    mkKernel
    mkInitrd
    mkInitramfsDir
    mkSandboxInitrd
    mkDirectBootVm
    mkUefiVm
    mkCombined;
}
