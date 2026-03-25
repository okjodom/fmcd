{
  description = "A fedimint client daemon for server side applications to hold, use, and manage Bitcoin and ecash";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-25.11";

    fenix = {
      url = "github:nix-community/fenix?rev=6b5325a017a9a9fe7e6252ccac3680cc7181cd63";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flakebox = {
      url = "github:dpc/flakebox?rev=09d74b0ecac2214a57887f80f2730f2399418067";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.fenix.follows = "fenix";
    };

    flake-utils.url = "github:numtide/flake-utils";

    fedimint.url = "github:fedimint/fedimint?ref=v0.9.0";
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      flakebox,
      flake-utils,
      fedimint,
    }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        lib = pkgs.lib;
        flakeboxLib = flakebox.lib.mkLib pkgs { };

        # Source files for the build
        rustSrc = flakeboxLib.filterSubPaths {
          root = builtins.path {
            name = "fmcd";
            path = ./.;
          };
          paths = [
            "Cargo.toml"
            "Cargo.lock"
            ".cargo"
            "src"
          ];
        };

        # Build configuration
        commonArgs = {
          buildInputs =
            [
              # System libraries needed for dependencies
              pkgs.zstd
              pkgs.openssl
              pkgs.protobuf
            ]
            # Add clang/llvm for cross-compilation support
            ++ lib.optionals (pkgs.stdenv.hostPlatform != pkgs.stdenv.buildPlatform) [
              pkgs.llvmPackages.clang
            ];
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.cmake
            pkgs.clang
            pkgs.llvmPackages.libclang.lib
          ];
          # Environment variables for build
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };

        # Toolchain configuration
        toolchainArgs = {
          extraRustFlags = "--cfg tokio_unstable";
          components = [
            "rustc"
            "cargo"
            "clippy"
            "rust-analyzer"
            "rust-src"
          ];
          # Use the current stable toolchain from the overridden fenix input.
          channel = "stable";
        };

        toolchainsStd = flakeboxLib.mkStdFenixToolchains toolchainArgs;

        # Common dev shell configuration
        commonShellArgs = {
          buildInputs = commonArgs.buildInputs ++ [
            pkgs.glibcLocales
            pkgs.glibc.dev
          ];
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
            # Build tools
            pkgs.perl

            # Development tools
            pkgs.mprocs
          ];
          # Inherit environment variables from commonArgs
          inherit (commonArgs) LIBCLANG_PATH;

          shellHook = ''
            export RUSTFLAGS="--cfg tokio_unstable"
            export RUSTDOCFLAGS="--cfg tokio_unstable"
            export RUST_LOG="info"
            export LOCALE_ARCHIVE="${pkgs.glibcLocales}/lib/locale/locale-archive"
            export LANG="en_US.UTF-8"
            export LC_ALL="en_US.UTF-8"
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
          '';
        };

        # Build outputs
        outputs = (flakeboxLib.craneMultiBuild { toolchains = toolchainsStd; }) (
          craneLib':
          let
            craneLib =
              (craneLib'.overrideArgs {
                pname = "fmcd";
                src = rustSrc;
              }).overrideArgs
                commonArgs;
          in
          rec {
            workspaceDeps = craneLib.buildDepsOnly { };

            fmcd = craneLib.buildPackage {
              pname = "fmcd";
              cargoArtifacts = workspaceDeps;
            };

            oci = pkgs.dockerTools.buildLayeredImage {
              name = "fmcd";
              contents = [ fmcd ];
              config = {
                Cmd = [ "${fmcd}/bin/fmcd" ];
              };
            };
          }
        );
      in
      {
        packages = {
          default = outputs.fmcd;
          oci = outputs.oci;
        };

        devShells = {
          default = flakeboxLib.mkDevShell (
            commonShellArgs
            // {
              toolchain = toolchainsStd.default;
            }
          );

          # Lint shell for CI (same as default)
          lint = flakeboxLib.mkDevShell (
            commonShellArgs
            // {
              toolchain = toolchainsStd.default;
            }
          );
        };
      }
    );
}
