{ lib
, pkgs
, nixpkgs
, hostSystem
, capsaPackage
}:
let
  defaultGuestSystem =
    if lib.hasPrefix "aarch64" hostSystem then
      "aarch64-linux"
    else if lib.hasPrefix "x86_64" hostSystem then
      "x86_64-linux"
    else
      throw "Unsupported host system: ${hostSystem}";

  mkInitramfsLib = guestSystem: import ../initramfs {
    inherit lib;
    pkgs = import nixpkgs { system = guestSystem; };
  };

  defaultGuestModule =
    { modulesPath, ... }:
    {
      imports = [ "${modulesPath}/profiles/minimal.nix" ];

      boot.loader.grub.enable = false;
      boot.loader.systemd-boot.enable = false;

      fileSystems."/" = {
        device = "none";
        fsType = "tmpfs";
      };

      networking.hostName = "capsa";
      system.stateVersion = "25.11";
    };

  mkVMAssets =
    { name ? "capsa"
    , guestSystem ? defaultGuestSystem
    , modules ? [ ]
    , specialArgs ? { }
    , vm ? { }
    ,
    }:
    let
      vmConfig = {
        vcpus = vm.vcpus or 1;
        memoryMiB = vm.memoryMiB or 512;
        kernelCmdline = vm.kernelCmdline or "console=hvc0 rdinit=/init";
        extraArgs = vm.extraArgs or [ ];
      };

      guestNixos = lib.nixosSystem {
        system = guestSystem;
        inherit specialArgs;
        modules = [ defaultGuestModule ] ++ modules;
      };

      kernelImage = "${guestNixos.config.system.build.kernel}/${guestNixos.config.system.boot.loader.kernelFile}";
      initramfsImage = (mkInitramfsLib guestSystem).mkInitramfs {
        inherit (guestNixos.config.system) modulesTree;
      };

      vmAssets = pkgs.runCommand "${name}-vm-assets" { } ''
        mkdir -p $out
        cp ${kernelImage} $out/vmlinuz
        cp ${initramfsImage} $out/initramfs.cpio.lz4
      '';
    in
    {
      inherit name guestSystem vmConfig guestNixos kernelImage initramfsImage vmAssets;
    };

  mkVM = args:
    let
      assets = mkVMAssets args;

      libkrunLibDir =
        if pkgs.stdenv.isDarwin then
          "${lib.getLib pkgs."libkrun-efi"}/lib"
        else
          "${lib.getLib pkgs.libkrun}/lib";

      capsaCmd = "${capsaPackage}/bin/capsa";
    in
    pkgs.writeShellApplication {
      name = "${assets.name}-run";
      text = ''
        export LIBKRUN_LIB_DIR="${libkrunLibDir}"

        exec ${capsaCmd} \
          --kernel ${assets.kernelImage} \
          --initramfs ${assets.initramfsImage} \
          --kernel-cmdline ${lib.escapeShellArg assets.vmConfig.kernelCmdline} \
          --vcpus ${toString assets.vmConfig.vcpus} \
          --memory-mib ${toString assets.vmConfig.memoryMiB} \
          ${lib.escapeShellArgs assets.vmConfig.extraArgs} \
          "$@"
      '';
    };

  mkVMCheck =
    { name ? "capsa"
    , guestSystem ? defaultGuestSystem
    , modules ? [ ]
    , specialArgs ? { }
    , vm ? { }
    , timeout ? 5
    , expectProgram
    }:
    let
      assets = mkVMAssets {
        inherit name guestSystem modules specialArgs vm;
      };
      capsaCmd = "${capsaPackage}/bin/capsa";
      expectSandboxEnv = lib.optionalString pkgs.stdenv.isDarwin ''
        # Nix already runs this check inside a macOS sandbox; launching a nested
        # seatbelt sandbox via `sandbox-exec` fails with EPERM (sandbox_apply).
        # Disable capsa's inner sandbox for this check environment.
        set env(CAPSA_DISABLE_SANDBOX) 1
      '';
    in
    pkgs.runCommand "${assets.name}-vm-check"
      {
        nativeBuildInputs = [ pkgs.expect pkgs.coreutils ];
      }
      ''
        set -euo pipefail

        cat > vm-check.expect <<'EOF'
        #!${pkgs.expect}/bin/expect
        set timeout ${toString timeout}

        set capsa "${capsaCmd}"
        set kernel "${assets.kernelImage}"
        set initramfs "${assets.initramfsImage}"
        set kernel_cmdline {${assets.vmConfig.kernelCmdline}}
        ${expectSandboxEnv}
        spawn $capsa \
          --kernel $kernel \
          --initramfs $initramfs \
          --kernel-cmdline $kernel_cmdline \
          --vcpus ${toString assets.vmConfig.vcpus} \
          --memory-mib ${toString assets.vmConfig.memoryMiB}

        set vm_prompt_re {~ # }

        proc fail {message} {
          puts "ERROR: $message"
          exit 1
        }

        proc vm_expect {pattern timeout_err eof_err} {
          expect {
            -re $pattern {}
            timeout { fail $timeout_err }
            eof { fail $eof_err }
          }
        }

        proc vm_prompt {} {
          vm_expect $::vm_prompt_re "timed out waiting for VM prompt" "capsa exited before prompt"
        }

        # Minimal check API:
        #   vm_run <command> ?expected_regex?
        #   vm_exit
        proc vm_run {line {pattern ""}} {
          vm_prompt
          send -- "$line\r"
          if {$pattern ne ""} {
            vm_expect $pattern "did not observe expected output for: $line" "capsa exited while running: $line"
          }
        }

        proc vm_exit {} {
          vm_prompt
          send -- "exit\r"
          expect {
            eof {}
            timeout { fail "timed out waiting for VM shutdown after exit" }
          }
        }

        ${expectProgram}

        set wait_result [wait]
        set status [lindex $wait_result 3]
        if {$status != 0} {
          puts "ERROR: capsa exited with status $status"
          exit 1
        }
        EOF

        chmod +x vm-check.expect
        ./vm-check.expect

        touch $out
      '';
in
{
  inherit defaultGuestSystem defaultGuestModule mkVMAssets mkVM mkVMCheck;
}
