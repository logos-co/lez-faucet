{
  description = "LEZ public-testnet faucet Basecamp module";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    nix-bundle-lgx.url = "github:logos-co/nix-bundle-lgx";
    nixpkgs.follows = "logos-module-builder/nixpkgs";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    logos-execution-zone = {
      url = "github:logos-blockchain/logos-execution-zone/a58fbce2ff48c58b7bb5001b1a27e64b9596ee3a";
      flake = false;
    };
    faucet-source = {
      url = "path:..";
      flake = false;
    };
  };

  outputs = inputs@{
    logos-module-builder,
    nixpkgs,
    rust-overlay,
    logos-execution-zone,
    faucet-source,
    ...
  }:
    let
      lib = nixpkgs.lib;
      system = "aarch64-darwin";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      rustToolchain = pkgs.rust-bin.stable."1.93.0".default;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };

      # v0.2.0 locks logos-blockchain-circuits at v0.5.3. Its build scripts
      # consume this prebuilt tree through LBC_ROOT_DIR and cannot download it
      # from inside a pure Nix build.
      circuits = pkgs.fetchzip {
        url = "https://github.com/logos-blockchain/logos-blockchain-circuits/releases/download/v0.5.3/logos-blockchain-circuits-v0.5.3-macos-aarch64.tar.gz";
        sha256 = "0w3i0phgzjswsk1q2k6cr3001jjc55a82z79zw9w5p3x9hwaqljq";
      };

      # logos-blockchain-circuits-prover enables static-rapidsnark. Point its
      # build script at the same prebuilt archive it would otherwise download.
      rapidsnark = pkgs.fetchzip {
        url = "https://github.com/iden3/rapidsnark/releases/download/v0.0.8/rapidsnark-macOS-arm64-v0.0.8.zip";
        sha256 = "1600dzr7hjg6lc5r0cdh189l7019djvy4cz2qyn75z5vrac4qs0f";
      };

      # Keep the Rust derivation independent of concurrent UI/module work in
      # the repository that provides faucet-source.
      faucetFfiSource = lib.cleanSourceWith {
        name = "lez-faucet-ffi-source";
        src = faucet-source;
        filter = path: type:
          let
            sourcePath = toString path;
            baseName = builtins.baseNameOf sourcePath;
          in
          baseName == "Cargo.toml"
          || baseName == "Cargo.lock"
          || lib.hasSuffix "/lez-faucet-ffi" sourcePath
          || lib.hasInfix "/lez-faucet-ffi/" sourcePath;
      };

      faucetFfi = rustPlatform.buildRustPackage {
        pname = "lez-faucet-ffi";
        version = "0.1.0";
        src = faucetFfiSource;
        cargoHash = "sha256-N5RcG24Xz3joKvOy0nKWFWtWEjAQl7bDGL7UCi0qBm8=";
        cargoBuildFlags = [ "-p" "lez-faucet-ffi" ];
        doCheck = false;

        # pyo3-ffi performs a python3 probe during its build.
        nativeBuildInputs = [ pkgs.python3 ];
        LBC_ROOT_DIR = circuits;
        RAPIDSNARK_LIB_DIR = "${rapidsnark}/lib";

        # build_utils resolves ../artifacts relative to its vendored manifest,
        # so stage the exact v0.2.0 builtin-program artifacts beside it.
        postPatch = ''
          cp -R ${logos-execution-zone}/artifacts "$cargoDepsCopy/artifacts"
        '';

        installPhase = ''
          runHook preInstall

          mkdir -p $out/lib $out/include
          ffi_lib=$(find target -name liblez_faucet_ffi.dylib -print -quit)
          if [ -z "$ffi_lib" ]; then
            echo "lez-faucet-ffi build did not produce liblez_faucet_ffi.dylib" >&2
            exit 1
          fi
          cp "$ffi_lib" $out/lib/liblez_faucet_ffi.dylib
          cp lez-faucet-ffi/lez_faucet_ffi.h $out/include/lez_faucet_ffi.h

          runHook postInstall
        '';

        postFixup = ''
          install_name_tool -id @rpath/liblez_faucet_ffi.dylib \
            $out/lib/liblez_faucet_ffi.dylib || true
        '';
      };

      faucetFfiInput = {
        packages.${system}.default = faucetFfi;
      };

      base = logos-module-builder.lib.mkLogosModule {
        src = ./.;
        configFile = ./metadata.json;
        flakeInputs = inputs;
        externalLibInputs.lez_faucet_ffi = {
          input = faucetFfiInput;
          packages.default = "default";
        };
        tests.dir = ./tests;
      };
    in
    base // {
      packages.${system} = (base.packages.${system} or {}) // {
        lez-faucet-ffi = faucetFfi;
      };
    };
}
