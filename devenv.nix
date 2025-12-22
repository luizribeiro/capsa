{ pkgs, lib, ... }:

{
  languages.rust.enable = true;
  languages.rust.channel = "nightly";

  packages = lib.optionals pkgs.stdenv.isDarwin [
    pkgs.vfkit
  ];

  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
    clippy.settings.allFeatures = true;
  };

  # codesign-run wraps binary execution with ad-hoc codesigning using virtualization
  # entitlements, required for running tests and examples that use Virtualization.framework
  enterShell = lib.optionalString pkgs.stdenv.isDarwin ''
    if ! command -v codesign-run &> /dev/null; then
      echo "Installing codesign-run..."
      cargo install --git https://github.com/luizribeiro/apple-main codesign-run
    fi
  '';
}
