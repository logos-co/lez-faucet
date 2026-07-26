# LEZ Faucet

LEZ Faucet is a small Basecamp app for funding public accounts on the Logos
Execution Zone public testnet. It supports two guided recipient flows:

1. Create a local faucet wallet, show its recovery mnemonic once, derive a
   public account, and initialize that account on-chain; or
2. Enter an existing public account ID from LEZ Wallet or the wallet CLI,
   verify whether it is ready for native transfers, and fund it without
   importing its keys. If it is uninitialized, the app provides the exact
   owner-side initialization command and a re-check action.
3. Check the selected recipient and its current balance.
4. Claim 150 testnet LEZ, once or repeatedly until a target is reached.

The project is intentionally not a general wallet. Existing-account funding is
address-only: the faucet neither imports the recipient's keys nor proves or
takes ownership of that account. The recipient must be public and initialized
under authenticated-transfer. A `Private/<account-id>` alone is insufficient:
private funding also requires recipient privacy keys and private state/proof
handling that this release does not implement. Transfers, key import,
multiple-account management, and mainnet are outside the v0.2.0 scope.

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

For an existing recipient, the app first opens its local faucet wallet solely
as the LEZ `WalletCore` client context. That local wallet does not need to own
the recipient and does not sign on its behalf. The recipient must already be a
public, initialized account owned by the authenticated-transfer program. Before
enabling a claim, the app fetches and displays that account's balance and
requires the user to confirm the checked account ID explicitly.

For an account newly created by LEZ Wallet or the wallet CLI, initialize it from
the wallet that owns its signing key. Run the command in that owning wallet's
context, setting `LEE_WALLET_HOME_DIR` first when it is not the CLI default:

```sh
wallet auth-transfer init --account-id Public/<account-id>
```

The app distinguishes an invalid ID, a valid-but-uninitialized account, and an
account owned by another program. For an uninitialized account it displays this
exact command with **Copy command** and **Re-check account** actions. The
recipient mnemonic and private key must remain in the owning wallet and are
never needed by the faucet.

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

The first opt-in public-testnet integration test creates and initializes a fresh
account, then consumes one 150 LEZ claim:

```sh
LEZ_FAUCET_LIVE_TEST=I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ \
  cargo test -p lez-faucet-ffi --test live_public_testnet \
  create_initialize_and_claim_once_on_public_testnet \
  -- --ignored --exact --nocapture
```

It must report the initialization transaction ID, the claim transaction ID when
available (otherwise an explicit unknown-hash marker), and balances, but never
the mnemonic, password, or key material.

The separate external-recipient proof creates two isolated wallets and proves
that wallet A can fund wallet B's initialized public account without importing
wallet B's key:

```sh
LEZ_FAUCET_RUN_LIVE=I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ \
  cargo test --release -p lez-faucet-ffi --test live_public_testnet \
  client_wallet_funds_distinct_external_public_account_on_public_testnet \
  -- --ignored --exact --nocapture
```

Both tests mutate the public testnet. Run only one at a time.

### Verify a public balance from the terminal

The repository includes an independent, read-only balance query. Pass the public
account shown by the app with or without its `Public/` prefix:

```sh
./scripts/lez-balance.sh Public/<account-id>
# prints one exact decimal integer, for example: 150
```

It calls the public sequencer's `getAccountBalance` JSON-RPC method directly and
does not open a wallet or read key material. The helper requires `curl` and
Python 3; Python's arbitrary-precision integers preserve the full LEZ `u128`
balance without rounding. To query a compatible localnet or a different
sequencer:

```sh
LEZ_FAUCET_SEQUENCER_URL=https://sequencer.example.test \
  ./scripts/lez-balance.sh Public/<account-id>
```

If you already have the pinned LEZ wallet CLI and a configured wallet home, the
equivalent read-only command uses `jq` to select its JSON balance:

```sh
LEE_WALLET_HOME_DIR=/path/to/wallet \
  wallet account get --raw --account-id Public/<account-id> |
  jq -er '.balance'
```

The CLI wallet is only the network-client context for this query and need not
own the account being checked. Its `wallet_config.json` must point at the same
sequencer. LEZ v0.2.0 uses `LEE_WALLET_HOME_DIR`; the older
`NSSA_WALLET_HOME_DIR` name is not read by this pinned wallet.

## Install and release

For a new installation, install `lez_faucet_ui`; Basecamp should resolve its
`lez_faucet` core dependency from the same catalog. When upgrading an existing
v0.1.0 installation, upgrade or install `lez_faucet` 0.2.0 first, then
`lez_faucet_ui` 0.2.0. The UI dependency is currently unversioned, so installing
the new UI alone may leave an already-installed 0.1.0 core in place. The rolling
catalog retains v0.1.0 for rollback. See
[Community installation](docs/community-install.md).

Release workflows are present for both modules, but the shared Nix release
pipeline is currently affected by
[`logos-module-builder#159`](https://github.com/logos-co/logos-module-builder/issues/159):
the pinned cargo-vendor fetch receives HTTP 403 responses from crates.io. Until
that upstream fix lands, produce the release artifacts locally and publish the
`.lgx` plus generated `sidecar.json` as described in [Releasing](docs/releasing.md).
The catalog index reads the published `.lgx` directly. The sidecar remains the
release's artifact metadata and is required by the release workflow's
already-published check.

## License and provenance

LEZ Faucet is available under either the MIT License or Apache License 2.0.
See `LICENSE-MIT`, `LICENSE-APACHE`, and `NOTICE`.
