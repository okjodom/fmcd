{
  description = "A fedimint client daemon for server side applications to hold, use, and manage Bitcoin and ecash";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-24.11";

    flakebox = {
      url = "github:rustshop/flakebox";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    fedimint.url = "github:fedimint/fedimint?ref=v0.9.0";
  };

  outputs =
    {
      self,
      nixpkgs,
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
        flakeboxLib = flakebox.lib.${system} { };

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
          # Use stable Rust channel
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
          default = flakeboxLib.mkDevShell commonShellArgs;

          # Lint shell for CI (same as default)
          lint = flakeboxLib.mkDevShell commonShellArgs;
        };
      }
    );
}
