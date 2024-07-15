{
  description = "A simpler rust re-implementation of the filetracker project from SIO2";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, flake-utils, nixpkgs }:
    {
      nixosModules.default = ./nix/module.nix;
      overlays.default = final: prev: {
        filetracker-rs = final.callPackage ./nix/package.nix { };
      };
    } //
    (flake-utils.lib.eachDefaultSystem (system: {
      packages.default =
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        pkgs.callPackage ./nix/package.nix { };
      devShells.default = self.packages.${system}.unwrapped;
    }));
}
