{
  description = "capsa: Rust + libkrun dev shell and minimal VM kernel/initramfs assets";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    git-hooks.url = "github:cachix/git-hooks.nix";
  };

  outputs = { self, nixpkgs, flake-utils, git-hooks, ... }:
    flake-utils.lib.eachSystem [
      "aarch64-darwin"
      "aarch64-linux"
      "x86_64-linux"
    ]
      (hostSystem:
        let
          inherit (nixpkgs) lib;
          pkgs = import nixpkgs { system = hostSystem; };

          capsaPackage = import ./nix/package.nix {
            inherit lib pkgs;
            src = ./.;
          };

          vmLib = import ./nix/vm {
            inherit lib pkgs nixpkgs hostSystem capsaPackage;
          };

          defaultVmAssets = vmLib.mkVMAssets { name = "capsa"; };
          defaultVm = vmLib.mkVM { name = "capsa"; };

          preCommitCheck = git-hooks.lib.${hostSystem}.run {
            src = ./.;
            hooks = {
              rustfmt.enable = true;
              clippy = {
                enable = true;
                packageOverrides = {
                  inherit (pkgs) cargo clippy;
                };
                settings = {
                  denyWarnings = true;
                  extraArgs = "--target-dir target/pre-commit";
                };
              };
              nixpkgs-fmt.enable = true;
              statix.enable = true;
              deadnix.enable = true;
            };
          };

          checks = import ./nix/checks {
            inherit vmLib pkgs;
          };
        in
        {
          lib = {
            inherit (vmLib) mkVMAssets mkVM mkVMCheck;
          };

          packages = {
            capsa = capsaPackage;

            vm-assets = defaultVmAssets.vmAssets;

            vm = defaultVm;

            default = self.packages.${hostSystem}.capsa;
          };

          inherit checks;

          devShells.default = pkgs.mkShell {
            packages =
              (with pkgs; [
                cargo
                rustc
                rustfmt
                clippy
                pkg-config
              ])
              ++ preCommitCheck.enabledPackages
              ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.libkrun pkgs.sydbox ]
              ++ lib.optionals pkgs.stdenv.isDarwin [
                pkgs."libkrun-efi"
                pkgs.libepoxy
                pkgs.virglrenderer
              ];

            shellHook = lib.concatStringsSep "\n" [
              preCommitCheck.shellHook
              (lib.optionalString pkgs.stdenv.isLinux ''
                export LIBKRUN_LIB_DIR="''${LIBKRUN_LIB_DIR:-${lib.getLib pkgs.libkrun}/lib}"
              '')
              (lib.optionalString pkgs.stdenv.isDarwin ''
                export LIBKRUN_LIB_DIR="''${LIBKRUN_LIB_DIR:-${lib.getLib pkgs."libkrun-efi"}/lib}"
              '')
            ];
          };

          formatter = pkgs.nixpkgs-fmt;
        });
}
