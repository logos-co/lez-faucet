# Releasing LEZ Faucet

Release the core package before the UI package. The version source of truth is
each module's `metadata.json`.

## Standard workflow

The repository contains three manually dispatchable workflows:

- `release-lez-faucet.yml` builds and publishes `faucet-module/`.
- `release-lez-faucet-ui.yml` builds and publishes `faucet-ui/`.
- `rebuild-index.yml` regenerates the rolling catalog index.

They call `logos-co/logos-modules-release-action@v1` and deliberately request
only `darwin-arm64`. The reusable release workflow builds `.#lgx-portable`,
verifies the package, creates a sidecar from the embedded manifest, publishes a
`<module>-v<version>` GitHub release, and asks the index workflow to run.

Dispatch in this order:

```sh
gh workflow run release-lez-faucet.yml
gh run watch

gh workflow run release-lez-faucet-ui.yml
gh run watch
```

Then verify that the rolling `index` release contains both packages.

## Current CI blocker

As verified during the initial v0.1 bootstrap on 2026-07-23, the shared Nix
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
lez_faucet-0.1.0.lgx
lez_faucet_ui-0.1.0.lgx
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
- neither package contains `/nix/store` runtime paths.

## Generate a sidecar

Generate a fresh sidecar for each artifact. Never reuse hashes, sizes, manifests,
or timestamps from another build.

```sh
module=lez_faucet
version=0.1.0
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
gh release create lez_faucet-v0.1.0 \
  --target main \
  --title "lez_faucet v0.1.0" \
  release/core/lez_faucet-0.1.0.lgx \
  release/core/sidecar.json

gh release create lez_faucet_ui-v0.1.0 \
  --target main \
  --title "lez_faucet_ui v0.1.0" \
  release/ui/lez_faucet_ui-0.1.0.lgx \
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
2. Install `lez_faucet_ui` and confirm `lez_faucet` resolves with it.
3. Create a disposable wallet and save the one-time mnemonic.
4. Initialize a fresh account.
5. Claim until its balance reaches at least 1,000 testnet LEZ.
