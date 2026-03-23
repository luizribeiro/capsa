#!/bin/sh
set -eu

iface="${1:-}"
if [ -z "$iface" ]; then
  iface="$(ls /sys/class/net | grep -v '^lo$' | head -n1 || true)"
fi

if [ -z "$iface" ]; then
  echo "net-up: no non-loopback interface found"
  exit 1
fi

echo "net-up: using interface $iface"

# BusyBox udhcpc needs packet sockets (AF_PACKET). In this tiny initramfs
# environment, support may be modular; load it best-effort.
modprobe af_packet >/dev/null 2>&1 || true

ip link set "$iface" up
udhcpc -f -q -n -t 5 -T 2 -i "$iface" -s /bin/udhcpc.script

ip addr show dev "$iface"
ip route show
