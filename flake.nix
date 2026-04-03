{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    microvm = {
      url = "github:microvm-nix/microvm.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    crane,
    flake-utils,
    rust-overlay,
    microvm,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [(import rust-overlay)];
        };
        rustToolchain = pkgs.rust-bin.stable.latest.default;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        cellaSource = let
          nixFilter = path: _type: builtins.match ".*nix/lib/disk-.*\\.nix$" path != null;
          filter = path: type:
            (nixFilter path type) || (craneLib.filterCargoSources path type);
        in pkgs.lib.cleanSourceWith {
          src = ./.;
          inherit filter;
        };
        commonArgs = {
          pname = "cella";
          version = "0.1.0";
          src = cellaSource;
          buildInputs = [pkgs.openssl];
          nativeBuildInputs = [pkgs.pkg-config];
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        cella = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "-p cella";
          nativeBuildInputs = (commonArgs.nativeBuildInputs or []) ++ [pkgs.makeWrapper];
          postInstall = ''
            wrapProgram $out/bin/cella \
              --prefix PATH : ${pkgs.lib.makeBinPath [pkgs.age pkgs.openssh]}
            ln -s $out/bin/cella $out/bin/git-remote-cella
          '';
        });
        cellx = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          cargoExtraArgs = "-p cellx";
        });
      in {
        formatter = pkgs.alejandra;

        packages.default = cella;
        packages.cellx = cellx;

        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.rust-analyzer
            pkgs.pkg-config
            pkgs.openssl
          ];
        };
      }
    )
    // {
      lib.mkHost = import ./nix/lib/mkHost.nix;
      lib.mkCell = import ./nix/lib/mkCell.nix;

      nixosModules.server = {
        config,
        lib,
        pkgs,
        ...
      }: {
        imports = [
          microvm.nixosModules.host
          ./nix/modules/host.nix
        ];

        nixpkgs.overlays = [
          (final: prev: {
            cella = self.packages.${final.stdenv.hostPlatform.system}.default;
          })
        ];

        _module.args.inputs = {
          inherit microvm;
        };
      };

      nixosModules.client = ./nix/modules/client.nix;
    };
}
