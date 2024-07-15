{ rustPlatform, ... }:

rustPlatform.buildRustPackage {
  pname = "filetracker";
  version = "0.1.0";

  src = builtins.path {
    path = ./..;
    filter = path: type: (builtins.match "(.*.nix|.*/flake.lock)" path) == null;
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = { };
  };
}
