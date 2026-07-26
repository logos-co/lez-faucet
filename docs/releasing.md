# Releasing LEZ Faucet

Release the core package before the UI package. The version source of truth is
each module's `metadata.json`.

Both packages must be released at 0.2.0. Existing v0.1.0 users must upgrade or
install `lez_faucet` 0.2.0 before `lez_faucet_ui` 0.2.0 because the UI's core
dependency is currently unversioned; upgrading the UI alone can leave the 0.1.0
core installed. New installations should resolve the newest core automatically.

## Standard workflow

The repository contains three manually dispatchable workflows:

- `release-lez-faucet.yml` builds and publishes `faucet-module/`.
- `release-lez-faucet-ui.yml` builds and publishes `faucet-ui/`.
- `rebuild-index.yml` regenerates the rolling catalog index.

They call `logos-co/logos-modules-release-action@v1` and deliberately request
only `darwin-arm64`. The reusable release workflow builds `.#lgx-portable`,
verifies the package, creates a sidecar from the embedded manifest, publishes a
`<module>-v<version>` GitHub release, and asks the index workflow to run.

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

Then verify that the rolling `index` release contains both packages at 0.2.0.
The rebuild scans every non-draft module release, so the v0.1.0 entries remain
available for rollback.

## Current CI blocker

As verified during the initial v0.1.0 bootstrap on 2026-07-23, the shared Nix
release path can fail while staging Cargo dependencies:

```text
Failed to fetch file from https://crates.io/api/v1/crates/.../download.
Status code: 403
```

The pinned `fetchCargoVendor` path uses a User-Agent that crates.io rejects.
This is tracked in
[`logos-module-builder#159`](https://github.com/logos-co/logos-module-builder/issues/159).
Do not claim that a GitHub Actions release succeeded unless both the `.lgx` and
`sidecar.json` assets actually exist. Until the upstream pin/fetcher is fixed,
build the release artifacts locally after pre-seeding the required fixed-output
vendor staging in the Nix store.

## Local artifact checks

Build portable packages, not development packages:

```sh
nix build ./faucet-module#lgx-portable --print-out-paths
nix build ./faucet-ui#lgx-portable --print-out-paths
```

Locate the `.lgx` files in the returned store paths, copy them to a temporary
release directory, and name them from the embedded module versions, for example:

```text
lez_faucet-0.2.0.lgx
lez_faucet_ui-0.2.0.lgx
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
- the only built variant is `darwin-arm64`;
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

```sh
module=lez_faucet
version=0.2.0
release_dir="release/core"
artifact="${release_dir}/${module}-${version}.lgx"

manifest=$(lgx manifest "$artifact" --json)
sha256=$(shasum -a 256 "$artifact" | awk '{print $1}')
size=$(stat -f%z "$artifact")
root_hash=$(jq -r '.hashes.root' <<<"$manifest")

jq -n \
  --arg publisherRef "${module}-v${version}" \
  --arg releasedAt "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg sha256 "$sha256" \
  --argjson size "$size" \
  --arg rootHash "$root_hash" \
  --argjson manifest "$manifest" \
  '{
    publisherRef: $publisherRef,
    releasedAt: $releasedAt,
    sha256: $sha256,
    size: $size,
    rootHash: $rootHash,
    builtVariants: ["darwin-arm64"],
    missingVariants: [],
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
   and .builtVariants == ["darwin-arm64"]' \
  "${release_dir}/sidecar.json"
```

Repeat with `module=lez_faucet_ui` and `release_dir=release/ui`; both GitHub
releases receive an asset named `sidecar.json`, while the local directories
keep the independently generated files separate.

## Publish the manual releases

```sh
gh release create lez_faucet-v0.2.0 \
  --target main \
  --title "lez_faucet v0.2.0" \
  release/core/lez_faucet-0.2.0.lgx \
  release/core/sidecar.json

gh release create lez_faucet_ui-v0.2.0 \
  --target main \
  --title "lez_faucet_ui v0.2.0" \
  release/ui/lez_faucet_ui-0.2.0.lgx \
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

Finally, test the user journey in a fresh Basecamp profile:

1. Add the raw `logos-repo.json` URL.
2. Install `lez_faucet_ui` 0.2.0 and confirm `lez_faucet` 0.2.0 resolves with it.
3. Confirm the first screen has no onboarding, password, recovery phrase or
   "create account" step.
4. Paste an independently created, already-initialized public
   authenticated-transfer address.
5. Query the recipient and pool balances independently first.
6. Press **Request 150 LEZ** exactly once.
7. Confirm the recipient is up exactly 150 and the pool down at least 150.
8. Confirm no wallet, config or state file was created anywhere.
