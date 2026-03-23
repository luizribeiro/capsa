{ vmLib, pkgs }:
let
  vm = vmLib.mkVM { name = "capsa-net-dhcp"; };
  expectSandboxEnv = pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
    set env(CAPSA_DISABLE_SANDBOX) 1
  '';
in
pkgs.runCommand "capsa-net-dhcp-vm-check"
{
  nativeBuildInputs = [ pkgs.expect pkgs.coreutils ];
}
  ''
    set -euo pipefail

    cat > vm-check.expect <<'EOF'
    #!${pkgs.expect}/bin/expect
    set timeout 20

    set vm "${vm}/bin/capsa-net-dhcp-run"
    ${expectSandboxEnv}
    spawn $vm --net

    proc fail {message} {
      puts "ERROR: $message"
      exit 1
    }

    set vm_prompt_re {~ # }
    set unsupported_re {failed to add network device: Function not implemented}

    expect {
      -re $unsupported_re {
        puts "SKIP: libkrun networking APIs unavailable"
        expect eof
        set wait_result [wait]
        set status [lindex $wait_result 3]
        if {$status != 1} {
          fail "expected capsa to exit with status 1 when networking is unsupported, got $status"
        }
        exit 0
      }
      -re $vm_prompt_re {}
      timeout { fail "timed out waiting for VM prompt" }
      eof { fail "capsa exited before VM prompt" }
    }

    send -- "/bin/busybox sh -c 'iface=\$(/bin/busybox ls /sys/class/net | /bin/busybox grep -v \"^lo\$\" | /bin/busybox head -n1); [ -n \"\$iface\" ] || { echo NO_IFACE; exit 1; }; /bin/busybox udhcpc -f -q -n -t 5 -T 2 -i \"\$iface\" 2>&1 | /bin/busybox grep -E \"lease of 10\\.0\\.2\\.[0-9]+ obtained\"; rc=\$?; echo DHCP_GREP_RC:\$rc'\r"

    expect {
      -re {DHCP_GREP_RC:0} {}
      -re {DHCP_GREP_RC:[1-9][0-9]*} { fail "DHCP lease check failed" }
      -re {NO_IFACE} { fail "no non-loopback interface present" }
      timeout { fail "did not observe DHCP lease in expected subnet" }
      eof { fail "capsa exited while waiting for DHCP result" }
    }

    expect {
      -re $vm_prompt_re {}
      timeout { fail "timed out waiting for VM prompt after DHCP" }
      eof { fail "capsa exited before VM shutdown" }
    }

    send -- "exit\r"
    expect {
      eof {}
      timeout { fail "timed out waiting for VM shutdown" }
    }

    set wait_result [wait]
    set status [lindex $wait_result 3]
    if {$status != 0} {
      fail "capsa exited with status $status"
    }
    EOF

    chmod +x vm-check.expect
    ./vm-check.expect

    touch $out
  ''
