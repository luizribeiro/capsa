# Test VMs for Capsa integration tests
#
# Usage:
#   nix-build nix/test-vms -A aarch64 -o result-vms  # for macOS
#   nix-build nix/test-vms -A x86_64 -o result-vms   # for Linux KVM
#
# Architecture:
#   lib.nix     - shared functions (config generation, init scripts, etc.)
#   aarch64.nix - ARM64 VMs for macOS Virtualization.framework
#   x86_64.nix  - x86_64 VMs for Linux KVM

{ nixpkgs ? <nixpkgs> }:

{
  aarch64 = import ./aarch64.nix { inherit nixpkgs; };
  x86_64 = import ./x86_64.nix { inherit nixpkgs; };
}
