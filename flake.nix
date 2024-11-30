# SPDX-FileCopyrightText: 2024 Sebastian Rasor <https://www.sebastianrasor.com/contact>
# SPDX-License-Identifier: AGPL-3.0-only

{
  description = "rasor_ratings rust (and nix) flake";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
      in {
        devShells.default = with pkgs;
          mkShell {
            nativeBuildInputs = [
              (rust-bin.stable.latest.default.override {
                extensions = [ "rust-analyzer" "rust-src" ];
              })
            ];
          };
        devShells.nix = with pkgs;
          mkShell { nativeBuildInputs = [ nil nixd nixfmt ]; };
        devShells.zed = with pkgs;
          mkShell {
            inputsFrom =
              [ self.devShells.${system}.default self.devShells.${system}.nix ];
            shellHook = ''
              exec ${lib.getExe' pkgs.zed-editor "zeditor"} .
            '';
          };
        packages.default = with pkgs;
          let
            cargoToml = (builtins.fromTOML (builtins.readFile ./Cargo.toml));
          in
          rustPlatform.buildRustPackage rec {
            pname = cargoToml.package.name;
            version = cargoToml.package.version;
            meta.mainProgram = cargoToml.package.name;
            buildInputs = [ openssl ];
            nativeBuildInputs = [ pkg-config ];
            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
          };
      });
}

