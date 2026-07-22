# LEZ Faucet

LEZ Faucet is a small Basecamp app for creating and funding a public account on
the Logos Execution Zone public testnet. It turns the current wallet-CLI flow
into one guided path:

1. Create a keychain wallet and show its recovery mnemonic once.
2. Derive one public account and initialize it on-chain.
3. Display its balance.
4. Claim 150 testnet LEZ, once or repeatedly until a target is reached.

The project is intentionally not a general wallet. Transfers, private accounts,
imports, multiple-account management, and mainnet are outside the v0.1 scope.

## Security: testnet only

The pinned LEZ v0.2.0 wallet does **not encrypt its persistent storage**.
Upstream currently ignores the password passed to wallet creation/restoration
and serializes account key material as JSON. The password prompt must not be
interpreted as protection for the wallet file.

- Use this app only with public-testnet funds that have no monetary value.
- Keep the storage file private and do not sync it through shared/cloud folders.
- Do not reuse a valuable password or mnemonic from another wallet.
- Anyone who can read the storage file may be able to control its accounts.

The recovery mnemonic is returned only during wallet creation. The UI must show
it once, require the user to acknowledge that it has been saved, then clear it
from application state. It must never be logged or persisted by this project.
See [Testnet and wallet safety](docs/testnet.md) for the upstream evidence and
operational details.

## Version lock

All LEZ client crates and build inputs are pinned to
`logos-blockchain/logos-execution-zone` tag `v0.2.0`, commit:

```text
a58fbce2ff48c58b7bb5001b1a27e64b9596ee3a
```

This must match the software running at `https://testnet.lez.logos.co`.
Before sending transactions the backend compares the sequencer's
`getProgramIds` response with the locally compiled authenticated-transfer and
pinata ImageIDs. A mismatch is a hard error, not a warning: version-skewed
instructions can otherwise be accepted without performing the requested state
transition.

## Project layout

```text
lez-faucet-ffi/  Rust wallet orchestration and C ABI
faucet-module/   Universal C++ Basecamp core module (`lez_faucet`)
faucet-ui/       QML view module (`lez_faucet_ui`)
```

The Rust layer owns the full transaction lifecycle. Each pinata claim fetches
the current challenge, solves it, submits at most one transaction, and then
reconciles the transaction, challenge, and account balance before another claim
starts. Normal success proves both inclusion and an exact 150 LEZ balance
increase. If the submission response is lost and no transaction hash is
available, the same exact `+150` balance change together with challenge rotation
can prove success; that receipt has a null `tx_hash`. If neither success nor a
safe retry can be proven, the operation returns an explicit unknown outcome and
does not resubmit. Claims cannot be precomputed or submitted concurrently
because every successful claim rotates the challenge.

## Development

Prerequisites are Nix with flakes, Rust, and logos-scaffold 0.1.1. Apple Silicon
macOS (`darwin-arm64`) is the only release target currently supported.

```sh
./scripts/scaffold-setup.sh

cargo fmt --all -- --check
cargo test -p lez-faucet-ffi

lgs basecamp build --variant lgx --module lez_faucet
lgs basecamp build --variant lgx --module lez_faucet_ui
```

The filtered builds are deliberately sequential because Scaffold recreates its
portable-artifact directory for each run. `lgs basecamp build` is the intended
aggregate check, but logos-scaffold 0.1.1 can still fail on same-repository
module paths during pure Nix evaluation; treat the filtered builds and direct
`nix build ...#lgx-portable` commands as the reliable fallback while that
upstream issue remains.

Scaffold localnet uses the fixed port `3040`. Run at most one localnet at a time
across Conductor workspaces or other checkouts of this repository to avoid a
port collision.

The opt-in public-testnet integration test creates and initializes a fresh
account and consumes one 150 LEZ claim:

```sh
LEZ_FAUCET_LIVE_TEST=I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ \
  cargo test -p lez-faucet-ffi --test live_public_testnet \
  -- --ignored --nocapture
```

It must report the initialization transaction ID, the claim transaction ID when
available (otherwise an explicit unknown-hash marker), and balances, but never
the mnemonic, password, or key material.

## Install and release

End users install `lez_faucet_ui`; Basecamp then resolves its `lez_faucet` core
dependency from the same catalog. See [Community installation](docs/community-install.md).

Release workflows are present for both modules, but the shared Nix release
pipeline is currently affected by
[`logos-module-builder#159`](https://github.com/logos-co/logos-module-builder/issues/159):
the pinned cargo-vendor fetch receives HTTP 403 responses from crates.io. Until
that upstream fix lands, produce the first artifacts locally and publish the
`.lgx` plus generated `sidecar.json` as described in [Releasing](docs/releasing.md).

## License and provenance

LEZ Faucet is available under either the MIT License or Apache License 2.0.
See `LICENSE-MIT`, `LICENSE-APACHE`, and `NOTICE`.
