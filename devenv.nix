{ pkgs, lib, ... }:

{
  cachix.pull = [ "capsa" ];

  languages.rust.enable = true;
  languages.rust.channel = "nightly";

  packages = lib.optionals pkgs.stdenv.isDarwin [
    pkgs.vfkit
  ];

  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
    clippy.settings.extraArgs =
      if pkgs.stdenv.isDarwin
      then "--workspace --features macos-native,macos-subprocess,vfkit --exclude capsa-linux-kvm"
      else "--workspace --features linux-kvm --exclude capsa-apple-vz --exclude capsa-apple-vzd --exclude capsa-apple-vzd-ipc";
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
