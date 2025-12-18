{ pkgs, ... }:

{
  languages.rust.enable = true;

  packages = [
    pkgs.vfkit
  ];

  enterShell = ''
    if ! command -v codesign-run &> /dev/null; then
      echo "Installing codesign-run..."
      cargo install --git https://github.com/luizribeiro/apple-main codesign-run
    fi
  '';
}
