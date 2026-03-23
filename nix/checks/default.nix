{ vmLib }:
let
  netIsolation = import ../tests/net-isolation.nix { inherit vmLib; };
in
{
  vm-smoke = vmLib.mkVMCheck {
    name = "capsa";
    expectProgram = ''
      vm_run "echo TTY_OK" {TTY_OK}
      vm_exit
    '';
  };

  vm-shell-basics = vmLib.mkVMCheck {
    name = "capsa-shell-basics";
    expectProgram = ''
      vm_run {/bin/busybox uname -s} {Linux}
      vm_run {test -c /dev/console; echo CONSOLE_RC:$?} {CONSOLE_RC:0}
      vm_run {false; echo FALSE_RC:$?} {FALSE_RC:1}
      vm_run {cat /proc/meminfo} {MemTotal:}
      vm_exit
    '';
  };

  vm-stateful-session = vmLib.mkVMCheck {
    name = "capsa-stateful-session";
    expectProgram = ''
      vm_run {x=41}
      vm_run {x=$((x+1))}
      vm_run {echo X:$x} {X:42}

      vm_run {echo hello >/run/capsa-smoke}
      vm_run {cat /run/capsa-smoke} {hello}
      vm_run {/bin/busybox grep -q '^MemTotal:' /proc/meminfo; echo MEMINFO_RC:$?} {MEMINFO_RC:0}
      vm_exit
    '';
  };

  vm-implicit-devices = vmLib.mkVMCheck {
    name = "capsa-implicit-devices";
    expectProgram = ''
      vm_run {
        balloon=0
        rng=0
        vsock=0
        for d in /sys/bus/virtio/devices/*; do
          case "$(cat "$d/device")" in
            0x0005) balloon=1 ;;
            0x0004) rng=1 ;;
            0x0013) vsock=1 ;;
          esac
        done
        echo DEVICES_OK:''${balloon}''${rng}''${vsock}
      } {DEVICES_OK:111}
      vm_exit
    '';
  };

  vm-net-isolation = netIsolation;
}


