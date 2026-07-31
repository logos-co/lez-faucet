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

      # The LEZ client stack links prebuilt, per-platform circuit and rapidsnark
      # archives, so this module can only be built for a system where upstream
      # publishes both. That is what this table records: it is the definition of
      # "supported", not a preference.
      #
      # x86_64-darwin is absent on purpose. rapidsnark v0.0.8 ships a macOS
      # x86_64 archive but logos-blockchain-circuits v0.5.3 does not, so there
      # is nothing to point LBC_ROOT_DIR at. The three entries below are exactly
      # the release variants darwin-arm64, linux-amd64, and linux-arm64.
      #
      # The hashes are `fetchzip` hashes of the unpacked trees, obtained with
      # `nix-prefetch-url --unpack <url>`.
      #
      # The rapidsnark URLs deliberately differ per platform, because that is
      # what rust-rapidsnark itself does. Its build script picks the asset per
      # target triple (crates/download_rapidsnark.sh at the rev pinned in
      # Cargo.lock, e91187f8): macOS takes the upstream iden3 release, but both
      # Linux targets take logos-blockchain's `-pic` rebuilds, because the
      # iden3 Linux archives are non-PIC and built against a newer glibc than
      # the fork's glibc-2.35 rebuild. Pointing RAPIDSNARK_LIB_DIR at the iden3
      # Linux archives would hand the build something it would never have
      # fetched for itself. The three hashes below are exactly the
      # x86_64-linux / aarch64-linux / aarch64-darwin entries of that repo's
      # own nix-hashes.json, in nix32 form.
      rapidsnarkVersion = "v0.0.8";
      iden3Base = "https://github.com/iden3/rapidsnark/releases/download/${rapidsnarkVersion}";
      picBase = "https://github.com/logos-blockchain/logos-blockchain-rust-rapidsnark/releases/download/rapidsnark-pic-${rapidsnarkVersion}";

      prebuilt = {
        aarch64-darwin = {
          circuitsPlatform = "macos-aarch64";
          circuitsHash = "0w3i0phgzjswsk1q2k6cr3001jjc55a82z79zw9w5p3x9hwaqljq";
          rapidsnarkUrl = "${iden3Base}/rapidsnark-macOS-arm64-${rapidsnarkVersion}.zip";
          rapidsnarkHash = "1600dzr7hjg6lc5r0cdh189l7019djvy4cz2qyn75z5vrac4qs0f";
        };
        x86_64-linux = {
          circuitsPlatform = "linux-x86_64";
          circuitsHash = "1mwy3g9dyjvlwykzs62gzf79rrnm20sy7c587nv26c1y9bm71wfv";
          rapidsnarkUrl = "${picBase}/rapidsnark-linux-x86_64-pic-${rapidsnarkVersion}.zip";
          rapidsnarkHash = "07qdnh4lm99alkmmg3av916bma7s86s616s56y0j4q4h82897kzk";
        };
        aarch64-linux = {
          circuitsPlatform = "linux-aarch64";
          circuitsHash = "14r4vghipk66k8g22kymy2gpfa1ghwwa74v57a230yk0pm9zvgp7";
          rapidsnarkUrl = "${picBase}/rapidsnark-linux-aarch64-pic-${rapidsnarkVersion}.zip";
          rapidsnarkHash = "15f4iqy2szqpp84v8584s5b86vw8rfz60wx7h7ylp34r0m7qii4i";
        };
      };

      systems = builtins.attrNames prebuilt;

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

      faucetFfiFor = system:
        let
          artifacts = prebuilt.${system};
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.stable."1.93.0".default;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };

          # This tree's Cargo.lock pins logos-blockchain-circuits at v0.5.3.
          # Its build scripts consume this prebuilt tree through LBC_ROOT_DIR
          # and cannot download it from inside a pure Nix build.
          circuits = pkgs.fetchzip {
            url = "https://github.com/logos-blockchain/logos-blockchain-circuits/releases/download/v0.5.3/logos-blockchain-circuits-v0.5.3-${artifacts.circuitsPlatform}.tar.gz";
            sha256 = artifacts.circuitsHash;
          };

          # logos-blockchain-circuits-prover enables static-rapidsnark, so
          # rust-rapidsnark's build.rs links librapidsnark/libfr/libfq/libgmp
          # statically out of RAPIDSNARK_LIB_DIR. Point it at the same prebuilt
          # archive it would otherwise download for this target (see the
          # rapidsnarkUrl note above); the sandbox has no network.
          rapidsnark = pkgs.fetchzip {
            url = artifacts.rapidsnarkUrl;
            sha256 = artifacts.rapidsnarkHash;
          };

          # Mach-O and ELF disagree only on the suffix; the install is otherwise
          # identical. `logos_module(... EXTERNAL_LIBS ...)` searches for both.
          libFile = "liblez_faucet_ffi${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}";
        in
        rustPlatform.buildRustPackage {
          pname = "lez-faucet-ffi";
          version = "0.3.0";
          src = faucetFfiSource;
          # Regenerated for vNext: dropping the `wallet`, `zeroize` and
          # `tempfile` dependencies changed the vendored set, so the 0.2.0 hash
          # no longer applies. The vendor step reads Cargo.lock and nothing
          # else, so this hash is the same on every system.
          cargoHash = "sha256-g1kXxMjVbalceSm51CebyMqOBORi25Xg+BokvTLkJhw=";
          cargoBuildFlags = [ "-p" "lez-faucet-ffi" ];
          doCheck = false;

          # pyo3-ffi performs a python3 probe during its build.
          nativeBuildInputs = [ pkgs.python3 ];
          LBC_ROOT_DIR = circuits;
          RAPIDSNARK_LIB_DIR = "${rapidsnark}/lib";

          # build_utils resolves ../artifacts relative to its vendored manifest,
          # so stage the builtin-program artifacts from the pinned upstream LEZ
          # v0.2.0 revision beside it.
          postPatch = ''
            cp -R ${logos-execution-zone}/artifacts "$cargoDepsCopy/artifacts"
          '';

          installPhase = ''
            runHook preInstall

            mkdir -p $out/lib $out/include
            ffi_lib=$(find target -name ${libFile} -print -quit)
            if [ -z "$ffi_lib" ]; then
              echo "lez-faucet-ffi build did not produce ${libFile}" >&2
              exit 1
            fi
            cp "$ffi_lib" $out/lib/${libFile}
            cp lez-faucet-ffi/lez_faucet_ffi.h $out/include/lez_faucet_ffi.h

            runHook postInstall
          '';

          # mkExternalLib uses an already-resolved derivation as-is and does not
          # apply its own install-name fixup, so record the relocatable name here
          # instead. Otherwise the packaged plugin would carry this store path.
          postFixup =
            if pkgs.stdenv.hostPlatform.isDarwin then ''
              install_name_tool -id @rpath/${libFile} \
                $out/lib/${libFile} || true
            '' else ''
              patchelf --set-soname ${libFile} $out/lib/${libFile} || true
            '';
        };

      faucetFfi = lib.genAttrs systems faucetFfiFor;

      faucetFfiInput = {
        packages = lib.mapAttrs (_system: drv: { default = drv; }) faucetFfi;
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
      # The builder emits outputs for every system it knows about, x86_64-darwin
      # included, and both `packages` and the `unit-tests` check reach into the
      # external-lib input for whichever system they are asked about. Publish
      # only the systems the table above can satisfy, so an unsupported
      # attribute is absent rather than present and broken — and
      # `nix flake check --all-systems` passes rather than failing on a platform
      # this module never claimed.
      packages = lib.genAttrs systems (system:
        (base.packages.${system} or {}) // {
          lez-faucet-ffi = faucetFfi.${system};
        });
    } // lib.optionalAttrs (base ? checks) {
      checks = lib.genAttrs systems (system: base.checks.${system} or {});
    };
}
