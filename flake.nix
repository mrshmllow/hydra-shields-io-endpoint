{
  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-compat.url = "github:edolstra/flake-compat";
    git-hooks.url = "github:cachix/git-hooks.nix";
    systems.url = "github:nix-systems/default";
    crane.url = "github:ipetkov/crane";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix.url = "github:nix-community/fenix";
    fenix.inputs.nixpkgs.follows = "nixpkgs";
    treefmt-nix.url = "github:numtide/treefmt-nix";

    # determines systems available for deployment
    linux-systems.url = "github:nix-systems/default-linux";

    # testing inputs
    nixpkgs_current_stable.url = "github:NixOS/nixpkgs/nixos-25.05";
  };
  outputs =
    {
      self,
      nixpkgs,
      flake-parts,
      systems,
      git-hooks,
      crane,
      treefmt-nix,
      ...
    }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        git-hooks.flakeModule
        treefmt-nix.flakeModule
        ./nix/hooks.nix # pre-commit hooks
        ./nix/utils.nix # utility functions
        ./nix/shells.nix
      ];
      systems = import systems;

      perSystem =
        {
          pkgs,
          inputs',
          config,
          lib,
          ...
        }:
        {
          _module.args = {
            toolchain = inputs'.fenix.packages.complete;
            craneLib = (crane.mkLib pkgs).overrideToolchain config._module.args.toolchain.toolchain;
            inherit self;
          };
          treefmt = {
            programs = {
              nixfmt.enable = true;
              rustfmt.enable = true;
              taplo.enable = true;
            };
          };
        };
    };
}
