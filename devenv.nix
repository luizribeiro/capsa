{ pkgs, ... }:

{
  languages.rust.enable = true;
  languages.rust.channel = "nightly";

  packages = [
    pkgs.vfkit
  ];

  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy.enable = true;
    clippy.settings.allFeatures = true;
  };

  enterShell = ''
    if ! command -v codesign-run &> /dev/null; then
      echo "Installing codesign-run..."
      cargo install --git https://github.com/luizribeiro/apple-main codesign-run
    fi
  '';
}
