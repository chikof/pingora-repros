{pkgs, ...}: {
  languages.rust = {
    enable = true;
    # rustup reads the same file outside Nix.
    toolchainFile = ./rust-toolchain.toml;
  };

  packages = [
    pkgs.git
    pkgs.cmake # libz-ng-sys, pulled in by pingora-core
    pkgs.pkg-config
    pkgs.openssl
    pkgs.cargo-nextest
    pkgs.typos
    pkgs.shellcheck
    pkgs.gh
    pkgs.curl
    pkgs.websocat # manual upgrade testing
  ];

  git-hooks.hooks.typos.enable = true;
}
