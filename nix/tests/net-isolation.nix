{ vmLib }:
vmLib.mkVMCheck {
  name = "capsa-net-isolation";
  expectProgram = ''
    vm_run {/bin/busybox ip link | /bin/busybox grep -q '^1: lo:'; echo LO_PRESENT:$?} {LO_PRESENT:0}
    vm_run {count=$(/bin/busybox ip link | /bin/busybox grep -c '^[0-9]\+:'); echo IFACE_COUNT:$count} {IFACE_COUNT:1}
    vm_exit
  '';
}
