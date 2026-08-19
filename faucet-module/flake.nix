{
  description = "LEZ public-testnet faucet Basecamp module";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";

    # The packaging tool, and the one input whose staleness is silent. Its
    # bundle.sh copies metadata.json's descriptive fields into the bundled
    # manifest.json through a hand-written key allowlist, so a pin that
    # predates a key drops that key with no build failure and no test failure.
    # The sibling repo shipped swap v0.3.0 with no `display_name` exactly that
    # way, from a pin one commit short of the fix
    # (logos-co/eth-lez-atomic-swaps#60).
    #
    # As of this writing b49074a8 is the tip of upstream `main`, and
    # logos-module-builder resolves nix-bundle-lgx to that same commit, so
    # there is nothing newer to move to. The durable protection is not the pin
    # anyway — it is CI's manifest round trip,
    # scripts/check-lgx-manifest.py, which compares the built .lgx's manifest
    # against metadata.json field by field on every build. Chasing this
    # through `nix flake update` would drag `nixpkgs` along with it (it
    # `follows` logos-module-builder, just below) for no packaging gain; see
    # the fetchCargoVendorPatched note further down for why moving nixpkgs
    # wholesale is not free here.
    nix-bundle-lgx.url = "github:logos-co/nix-bundle-lgx";

    nixpkgs.follows = "logos-module-builder/nixpkgs";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    logos-execution-zone = {
      url = "github:logos-blockchain/logos-execution-zone/d6e4ae694e7419f5906b340c232704466a1917b7";
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

          # Until this, no Nix build of this module had ever got past
          # vendoring. Every leg, on every platform, died on the first crate:
          #
          #   Failed to fetch file from
          #   https://crates.io/api/v1/crates/adler2/2.0.1/download.
          #   Status code: 403
          #
          # crates.io answers 403 to the User-Agent nixpkgs' cargo vendor
          # fetcher sends. It sets none, so requests sends `python-requests/*`;
          # measured, that is a 403 and so is `curl/*`, while a descriptive one
          # is a 200. Upstream fixed it in NixOS/nixpkgs#512735 (2026-04-26)
          # and later moved the download to static.crates.io; the nixpkgs this
          # flake follows through logos-module-builder is e9f00bd8 (2025-09),
          # which predates both. This is not Linux-specific — it is what
          # blocked the darwin-arm64 release too.
          #
          # Repinning nixpkgs is not available: it is this module's entire
          # Qt/C++ toolchain. So this is nixpkgs' `fetch-cargo-vendor.nix`
          # re-expressed here, with its two helper scripts rebuilt from the
          # pinned tree and three substitutions applied to the fetching one.
          # Only lez-faucet-ffi uses it; nixpkgs' own `rustPlatform` is
          # untouched, so no other derivation in the graph moves and nothing
          # loses its binary-cache hit.
          #
          # This REPLACES the earlier fix, which set the User-Agent by running
          # `applyPatches` over the whole nixpkgs source and importing the
          # result. That form worked on a warm local machine and is wrong for
          # CI: patching the shared `fetch-cargo-vendor-util` shifts the
          # `outPath` of every Rust package in nixpkgs (measured:
          # `qt6.qtdeclarative`, `python3Packages.cryptography` and
          # `cargo-auditable` all move), which costs the build its cache hits
          # on exactly the cold runners it is meant to help, and can mean hours
          # of Qt rebuilds. A deliberately scoped `replaceStrings` on a private
          # copy leaves every other outPath untouched — verified against those
          # same three probes, which stay at their stock paths. Do not layer
          # the two: there must be exactly one patched fetcher, and it is this
          # one.
          #
          # Deliberately re-expressed rather than generated with
          # `builtins.toFile` from the original: a generated file has to embed
          # `${nixpkgs}` store paths as text, and registering those as
          # references made evaluation die with `path '…-source' is not valid`
          # on CI's Determinate Nix while evaluating clean on stock Nix.
          # `builtins.readFile` keeps every store reference out of it.
          #
          # The substitutions are asserted, so a nixpkgs bump that reshapes the
          # fetcher fails evaluation loudly instead of silently going back to a
          # 403. Delete all of this once logos-module-builder's nixpkgs carries
          # the upstream fix (logos-co/logos-module-builder#173).
          #
          # FIRST: the User-Agent, which is what the 403 is about.
          #
          # SECOND: crates.io 429. Once the 403 is out of the way, six legs
          # fanning out at once, each vendoring hundreds of crates from one
          # runner IP range, get throttled instead. Upstream ALREADY mounts a
          # urllib3 `Retry`, but its `status_forcelist` is 5xx-only, so the one
          # status that explicitly means "back off and come back" is the one it
          # does not retry. Widen it, and make the backoff suit a shared
          # throttle: 429 added, 5xx kept, 403 deliberately NOT retried (that
          # is a policy answer, and retrying it would burn the 90-minute
          # timeout to reach the same error); jitter, so legs that trip the
          # limit in the same second do not retry in lockstep; bounded at 12
          # attempts and 60s, so a genuinely down registry fails in minutes
          # rather than hanging to the timeout; and `respect_retry_after_header`
          # pinned explicitly so a `Retry-After` wins over our own curve.
          # `backoff_jitter`/`backoff_max` are urllib3 >= 2.0 kwargs; the pinned
          # tree has urllib3 2.5.0.
          #
          # THIRD, and the one that actually fixes it: fetch the tarballs from
          # `static.crates.io` instead of the `crates.io/api/v1` endpoint.
          # Retry alone was measured insufficient in the sibling swap module —
          # legs rode the full backoff budget and still came out with
          # `RetryError: ... too many 429 error responses`, because the throttle
          # outlasts any bounded budget. `static.crates.io` is the CDN cargo
          # itself downloads from (the registry's `dl` key); it applies neither
          # the User-Agent policy that produces the 403 nor the rate limit that
          # produces the 429, so it removes the cause rather than waiting the
          # symptom out. It was measured to return 200 for every user-agent
          # tried, `python-requests/2.32.3` and `curl/*` included, on the exact
          # URLs that 403. This is also where upstream moved after #512735.
          #
          # A different host cannot vendor different bytes without being
          # noticed: the script checksums every tarball against Cargo.lock, so
          # a wrong URL fails loudly rather than silently vendoring something
          # else. Which is also why none of the three can move `cargoDeps.hash`
          # below — it is the same value a plain `cargoHash` takes, and indeed
          # it is the value that was already there.
          fetchCargoVendorPatched =
            let
              rustBuildSupport = "${nixpkgs}/pkgs/build-support/rust";
              subst = what: from: to: text:
                let out = builtins.replaceStrings [ from ] [ to ] text; in
                if out == text
                then throw "lez-faucet-ffi: crates.io ${what} workaround is stale — ${builtins.toJSON from} not found in nixpkgs' cargo vendor fetcher"
                else out;
              replaceWorkspaceValues = pkgs.writers.writePython3Bin "replace-workspace-values" {
                libraries = with pkgs.python3Packages; [ tomli tomli-w ];
                flakeIgnore = [ "E501" "W503" ];
              } (builtins.readFile "${rustBuildSupport}/replace-workspace-values.py");
              fetchCargoVendorUtil = pkgs.writers.writePython3Bin "fetch-cargo-vendor-util" {
                libraries = with pkgs.python3Packages; [ requests ];
                flakeIgnore = [ "E501" ];
              } (subst "User-Agent"
                   "    session = requests.Session()\n"
                   "    session = requests.Session()\n    session.headers[\"User-Agent\"] = \"nixpkgs-fetchCargoVendor/1 (https://github.com/NixOS/nixpkgs)\"\n"
                (subst "429 retry"
                   "        total=5,\n        backoff_factor=0.5,\n        status_forcelist=[500, 502, 503, 504]\n"
                   "        total=12,\n        backoff_factor=1.5,\n        backoff_jitter=1.0,\n        backoff_max=60,\n        respect_retry_after_header=True,\n        status_forcelist=[429, 500, 502, 503, 504]\n"
                (subst "static.crates.io download"
                   "    return f\"https://crates.io/api/v1/crates/{pkg[\"name\"]}/{pkg[\"version\"]}/download\"\n"
                   "    return f\"https://static.crates.io/crates/{pkg[\"name\"]}/{pkg[\"name\"]}-{pkg[\"version\"]}.crate\"\n"
                   (builtins.readFile "${rustBuildSupport}/fetch-cargo-vendor-util.py"))));
            in
            { name, hash, ... }@args:
            let
              vendorStaging = pkgs.stdenvNoCC.mkDerivation ({
                name = "${name}-vendor-staging";

                impureEnvVars = lib.fetchers.proxyImpureEnvVars;

                nativeBuildInputs = [
                  fetchCargoVendorUtil
                  pkgs.cacert
                  # break loop of nix-prefetch-git -> git-lfs -> asciidoctor ->
                  # ruby (yjit) -> fetchCargoVendor -> nix-prefetch-git
                  (pkgs.nix-prefetch-git.override { git-lfs = null; })
                ];

                buildPhase = ''
                  runHook preBuild
                  fetch-cargo-vendor-util create-vendor-staging ./Cargo.lock "$out"
                  runHook postBuild
                '';

                strictDeps = true;
                dontConfigure = true;
                dontInstall = true;
                dontFixup = true;

                outputHash = hash;
                outputHashMode = "recursive";
              } // builtins.removeAttrs args [ "name" "hash" ]);
            in
            pkgs.runCommand "${name}-vendor"
              {
                inherit vendorStaging;
                nativeBuildInputs = [ fetchCargoVendorUtil rustToolchain replaceWorkspaceValues ];
              }
              ''
                fetch-cargo-vendor-util create-vendor "$vendorStaging" "$out"
              '';
        in
        rustPlatform.buildRustPackage {
          pname = "lez-faucet-ffi";
          version = "0.3.2";
          src = faucetFfiSource;
          # `cargoDeps` rather than `cargoHash` so vendoring goes through the
          # patched fetcher above. The value is the 0.3.1 vendor hash and is
          # not affected by that patch: it covers only the fixed-output
          # vendor-staging tree — checksum-verified crate tarballs, git
          # checkouts and Cargo.lock — so it tracks the lockfile, not the
          # nixpkgs or the host serving the tarballs. It is likewise the same
          # on every system. (0.3.1 regenerated it because repinning LEZ from
          # v0.2.0 to v0.2.2 moved every LEZ git checkout in the vendored set
          # and shifted their transitive crates; the 0.3.0 hash no longer
          # applies.)
          cargoDeps = fetchCargoVendorPatched {
            name = "lez-faucet-ffi-0.3.2";
            src = faucetFfiSource;
            hash = "sha256-KQrfJXYLwp2gOE3DrO8gG0C7CQnBx5AWVWtdAuhPHGw=";
          };
          cargoBuildFlags = [ "-p" "lez-faucet-ffi" ];
          doCheck = false;

          # pyo3-ffi performs a python3 probe during its build.
          nativeBuildInputs = [ pkgs.python3 ];
          LBC_ROOT_DIR = circuits;
          RAPIDSNARK_LIB_DIR = "${rapidsnark}/lib";

          # build_utils resolves ../artifacts relative to its vendored manifest,
          # so stage the builtin-program artifacts from the pinned upstream LEZ
          # v0.2.2 revision beside it.
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
