# Releasing LEZ Faucet

Release the core package before the UI package. The version source of truth is
each module's `metadata.json`.

Both packages must be released at 0.3.1. LEZ Faucet 0.3.1 is a patch: it repins
the client to LEZ `v0.2.2` to match the testnet upgraded on 2026-08-05, and
changes no interface. It is nonetheless mandatory rather than optional, because
0.3.0 and every earlier build are pinned to `v0.2.0` and now refuse every claim
with "This app does not match the deployed testnet".

Its predecessor 0.3.0 was the breaking release — the core module's C++ ABI
changed, the UI's Qt Remote Objects interface changed, and the wallet and
key-material flow was removed — so that one was a minor bump under semver
rather than a patch. Existing v0.1.0 and v0.2.0 users must still upgrade or
install `lez_faucet` before `lez_faucet_ui`, because the UI's core dependency is
currently unversioned and upgrading the UI alone can leave the older core
installed. That combination is broken, not merely stale: the 0.2.x core does not
implement the slots a 0.3.x UI calls, so the app fails on the first action
rather than presenting an older screen. New installations should resolve the
newest core automatically.

## Standard workflow

The repository contains three manually dispatchable workflows:

- `release-lez-faucet.yml` builds and publishes `faucet-module/`.
- `release-lez-faucet-ui.yml` builds and publishes `faucet-ui/`.
- `rebuild-index.yml` regenerates the rolling catalog index.

They call `logos-co/logos-modules-release-action@v1` and pass no `variants`
input, so its default applies: `darwin-arm64`, `linux-amd64`, `linux-arm64`.
Those are exactly the systems `faucet-module/flake.nix` exposes, and the flake
is the single source of truth for what is buildable — restating the list in the
workflow would only let the two drift. The reusable release workflow builds
`.#lgx-portable` once per variant on its matching runner, merges the results
into one multi-variant `.lgx`, verifies the package, creates a sidecar from the
embedded manifest, publishes a `<module>-v<version>` GitHub release, and asks
the index workflow to run.

The matrix is `fail-fast: false` and the merge tolerates a partial result, so a
variant that fails to build costs that variant, not the release. The sidecar's
`builtVariants` and `missingVariants` record which is which. A release that is
short a variant is a bug to investigate, not a normal outcome.

Intel macOS is not requested and cannot be. logos-blockchain-circuits v0.5.3
publishes no macOS x86_64 archive, so `LBC_ROOT_DIR` has nothing to point at;
the flake omits `x86_64-darwin` rather than exposing an output that would fail.

The current v1 index workflow enumerates `.lgx` download URLs from every
non-draft module release, verifies each package, and builds `index.json` from
the package manifest and bytes. It does not read `sidecar.json`. The sidecar is
still a required release asset: it records publication metadata, and the
release workflow requires both an `.lgx` and `sidecar.json` before treating a
module version as already published and skipping a rebuild.

Dispatch in this order:

```sh
gh workflow run release-lez-faucet.yml
gh run watch

gh workflow run release-lez-faucet-ui.yml
gh run watch
```

Then verify that the rolling `index` release contains both packages at 0.3.1.
The rebuild scans every non-draft module release, so the 0.3.0, v0.2.0 and
v0.1.0 entries remain listed for rollback — though none of them can claim
against the current testnet, all being pinned to LEZ `v0.2.0`.

## The crates.io vendoring workaround

As verified during the initial v0.1.0 bootstrap on 2026-07-23, the shared Nix
release path failed while staging Cargo dependencies:

```text
Failed to fetch file from https://crates.io/api/v1/crates/.../download.
Status code: 403
```

The pinned `fetchCargoVendor` path uses a User-Agent that crates.io rejects.
This is tracked in
[`logos-module-builder#159`](https://github.com/logos-co/logos-module-builder/issues/159),
which is still open.

This tree carries its own workaround rather than waiting on it.
`faucet-module/flake.nix` re-expresses nixpkgs' `fetch-cargo-vendor.nix` as a
private `fetchCargoVendorPatched`, used only by `lez-faucet-ffi`, with three
substitutions applied to the fetch script: a descriptive User-Agent, a retry
policy that includes HTTP 429, and crate tarballs downloaded from
`static.crates.io` instead of the `crates.io/api/v1` endpoint. The scoping
matters. Patching the shared `fetch-cargo-vendor-util` through `applyPatches`
also fixes the 403, but it moves the `outPath` of every Rust package in
nixpkgs — `qt6.qtdeclarative` and `python3Packages.cryptography` included — and
so throws away the binary-cache hits that keep a cold CI runner from rebuilding
Qt. If you change this code, keep it scoped, and keep exactly one patched
fetcher.

`cargoDeps.hash` is unaffected by all three substitutions: it covers the
checksum-verified vendor-staging tree, so it tracks `Cargo.lock` and not the
host that served the tarballs.

The workaround is exercised on every pull request by `.github/workflows/ci.yml`,
which runs the same `nix build .#lgx-portable` on the same three runner types
the release workflow uses. That is evidence about the build, not about a
release: do not claim that a GitHub Actions release succeeded unless both the
`.lgx` and `sidecar.json` assets actually exist on the release. Until a
dispatched run has produced both, the local path below remains the fallback.

## Local artifact checks

A local build produces one variant: the host's. Everything below therefore
describes a single-variant package and, as written, assumes an Apple Silicon
macOS host. Only the CI path produces the merged three-variant `.lgx`; do not
publish a locally built package as though it covered Linux.

Build portable packages, not development packages:

```sh
nix build ./faucet-module#lgx-portable --print-out-paths
nix build ./faucet-ui#lgx-portable --print-out-paths
```

Locate the `.lgx` files in the returned store paths, copy them to a temporary
release directory, and name them from the embedded module versions, for example:

```text
lez_faucet-0.3.1.lgx
lez_faucet_ui-0.3.1.lgx
```

For each artifact:

```sh
lgx verify "$artifact"
lgx manifest "$artifact" --json | jq
shasum -a 256 "$artifact"
stat -f%z "$artifact"
```

Check that:

- the core manifest name is `lez_faucet`;
- the UI manifest name is `lez_faucet_ui`;
- the UI dependency list includes `lez_faucet`;
- the only built variant is the host's — `darwin-arm64` on this machine;
- every bundled Mach-O library has no `/nix/store` load command; bundled
  dylibs use `@loader_path`, while Qt frameworks use `@rpath`;
- a read-only sequencer RPC succeeds with Nix and SSL-related environment
  overrides cleared.

Do not treat an archive-wide string scan as a portability check. The current
packages retain some build/debug source paths, and bundled `libcrypto` contains
OpenSSL's compiled `OPENSSLDIR`, `ENGINESDIR`, and `MODULESDIR` strings. Those
strings are not Mach-O load commands. The sequencer RPC path uses Rust with
rustls, so its clean-environment smoke proves that path does not depend on the
OpenSSL defaults. The C++ transport still carries OpenSSL symbols; this smoke
does not prove that every C++ TLS path ignores those compiled default
directories.

## Generate a sidecar

Generate a fresh sidecar for each artifact. Never reuse hashes, sizes, manifests,
or timestamps from another build. This file accompanies the release as
artifact metadata and satisfies the release workflow's already-published gate;
the catalog index independently reads and verifies the `.lgx` asset.

Read the variant list out of the package rather than typing it. The manifest's
`main` field maps each built variant to its plugin filename, so its keys are
the variants the artifact actually contains — a locally built package will
report `["darwin-arm64"]` and be two short of the requested three.

```sh
module=lez_faucet
version=0.3.1
release_dir="release/core"
artifact="${release_dir}/${module}-${version}.lgx"

manifest=$(lgx manifest "$artifact" --json)
sha256=$(shasum -a 256 "$artifact" | awk '{print $1}')
size=$(stat -f%z "$artifact")
root_hash=$(jq -r '.hashes.root' <<<"$manifest")
built=$(jq -c '.main | keys' <<<"$manifest")
missing=$(jq -cn --argjson b "$built" \
  '["darwin-arm64","linux-amd64","linux-arm64"] - $b')

jq -n \
  --arg publisherRef "${module}-v${version}" \
  --arg releasedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg sha256 "$sha256" \
  --argjson size "$size" \
  --arg rootHash "$root_hash" \
  --argjson manifest "$manifest" \
  --argjson builtVariants "$built" \
  --argjson missingVariants "$missing" \
  '{
    publisherRef: $publisherRef,
    releasedAt: $releasedAt,
    sha256: $sha256,
    size: $size,
    rootHash: $rootHash,
    builtVariants: $builtVariants,
    missingVariants: $missingVariants,
    manifest: $manifest
  }' > "${release_dir}/sidecar.json"
```

Validate the sidecar before uploading:

```sh
jq -e \
  --arg module "$module" \
  --arg version "$version" \
  '.publisherRef == ($module + "-v" + $version)
   and .manifest.name == $module
   and .manifest.version == $version
   and (.builtVariants | length) > 0
   and (.builtVariants + .missingVariants | sort)
       == ["darwin-arm64","linux-amd64","linux-arm64"]' \
  "${release_dir}/sidecar.json"
```

A non-empty `missingVariants` here is expected for the local fallback and is
the record that the published package is partial. Say so in the release notes.

Repeat with `module=lez_faucet_ui` and `release_dir=release/ui`; both GitHub
releases receive an asset named `sidecar.json`, while the local directories
keep the independently generated files separate.

## Publish the manual releases

```sh
gh release create lez_faucet-v0.3.1 \
  --target main \
  --title "lez_faucet v0.3.1" \
  release/core/lez_faucet-0.3.1.lgx \
  release/core/sidecar.json

gh release create lez_faucet_ui-v0.3.1 \
  --target main \
  --title "lez_faucet_ui v0.3.1" \
  release/ui/lez_faucet_ui-0.3.1.lgx \
  release/ui/sidecar.json

gh workflow run rebuild-index.yml
```

Publishing changes external state. Resolve the exact local artifact paths and
inspect their manifests immediately before running these commands.

## Final catalog verification

```sh
curl -fsSL \
  https://github.com/logos-co/lez-faucet/releases/download/index/index.json \
  | jq '.packages[] | {name, versions: [.versions[].manifest.version]}'
```

Both packages must list 0.3.1, with 0.3.0, 0.2.0 and 0.1.0 still present as the
rollback entries.

Finally, test the user journey in a fresh Basecamp profile:

1. Add the raw `logos-repo.json` URL.
2. Install `lez_faucet_ui` 0.3.1 and confirm `lez_faucet` 0.3.1 resolves with it.
3. Confirm the first screen has no onboarding, password, recovery phrase or
   "create account" step.
4. Paste an independently created, already-initialized public
   authenticated-transfer address.
5. Query the recipient and pool balances independently first.
6. Press **Request 150 LEZ** exactly once.
7. Confirm the recipient is up exactly 150 and the pool down at least 150.
8. Confirm no wallet, config or state file was created anywhere.

Test the upgrade path separately, in a profile that already has 0.2.0
installed: upgrade `lez_faucet` to 0.3.1 first, then `lez_faucet_ui`. Upgrading
the UI alone is the failure mode this release has to be checked against, and it
presents as an error on the first action, not as the old screen.
