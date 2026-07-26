# LEZ Faucet

LEZ Faucet is a small Basecamp app that funds one public account on the Logos
Execution Zone public testnet. Its entire user input is one public LEZ address,
and its entire action is one button:

```text
Public LEZ address
[ Request 150 LEZ ]
```

One press funds one eligible account by exactly 150 testnet LEZ.

## It is a faucet client, not a wallet

The app owns no wallet and holds no key material. It never asks for — and has
no way to accept — a password, recovery phrase, private key, viewing key or
signing key, and it writes no files.

That is a property of the protocol, not a feature we chose to withhold. A
Piñata claim names both the pool and the recipient as unsigned public
participants, so the transaction carries no signatures and no nonces. There is
nothing to sign with, and any key material in this process would be material it
did not need.

Consequences worth stating plainly:

- The recipient does not authorize the claim and is never asked to.
- The app cannot initialize an account on the owner's behalf. If an address is
  not yet initialized, the app shows the command for its **owner** to run, and
  needs no secret from them to do so.
- Nothing survives a restart, because nothing is stored. If the app is quit
  during a claim, it cannot reconcile that claim on the next launch; the
  balance must be checked independently.

### What "success" means here

A receipt is shown only when the app has observed **its own** transaction
included on chain **and** the recipient's balance up by exactly 150. Having
submitted a transaction is never reported as success.

Where that cannot be proven within the deadline, the app says the outcome is
unknown, shows the address, the pre-claim balance and the transaction hash, and
offers no retry — because retrying an unresolved claim is how one press becomes
two credits.

## The pool is finite and shared

The Piñata pool started at 1,500,000 testnet LEZ and pays 150 per claim. It is
**permissionless and repeatedly claimable, not unlimited**: every claim is a
proof-of-work race against every other claimant for one global challenge, and
the pool stops paying when it can no longer subtract 150.

The deployed program enforces no cooldown, no per-address quota and no rate
limit. This app's internal bounds are there to keep it well behaved on your
machine and on a single shared sequencer — they are **not** abuse prevention,
and nothing client-side could be.

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
lez-faucet-ffi/  Stateless Rust faucet client and C ABI
faucet-module/   Universal C++ Basecamp core module (`lez_faucet`)
faucet-ui/       QML view module (`lez_faucet_ui`)
```

The Rust layer owns the full transaction lifecycle. Each pinata claim fetches
the current challenge, solves it, submits at most one transaction, and then
reconciles its own transaction against the recipient's balance before any
receipt is produced.

The transaction hash is computed **locally, before submission** — it is a pure
function of the transaction bytes, and the sequencer returns that same value.
So a lost submission response costs nothing: the app can still ask whether its
own transaction landed. There is no success case with a missing hash.

Success requires both facts together: our transaction observed included, and
the recipient at exactly `balance_before + 150`. Inclusion alone is not enough,
because a claim whose proof-of-work went stale is still accepted and included —
it simply executes and does nothing.

That last point also supplies the only safe retry rule. A second attempt is
made only when the app has watched its own transaction land and credit nothing,
which proves it has already executed and cannot execute again. The app never
infers "safe to retry" from a transaction's *absence*: a transaction that was
included a moment ago looks exactly like one that was never sent, and acting on
that guess is how one press becomes two credits.

The recipient must already be a public, initialized account owned by the
authenticated-transfer program. That is this app's policy, not a rule the
deployed program enforces — the Piñata guest does not check the recipient's
owner at all.

For an account newly created by LEZ Wallet or the wallet CLI, initialize it from
the wallet that owns its signing key. Run the command in that owning wallet's
context, setting `LEE_WALLET_HOME_DIR` first when it is not the CLI default:

```sh
wallet auth-transfer init --account-id Public/<account-id>
```

The app distinguishes an invalid ID, a valid-but-uninitialized account, and an
account owned by another program. For an uninitialized account it displays this
exact command with a **Copy command** action. The recipient's keys stay in the
owning wallet and are never needed by the faucet.

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

Read-only tests against the public testnet are safe to run at any time:

```sh
cargo test -p lez-faucet-ffi --test live_public_testnet -- --ignored --nocapture
```

There is exactly one write test. It spends 150 LEZ from a finite shared pool,
so it runs only when a destination is named explicitly, and skips otherwise:

```sh
LEZ_FAUCET_LIVE_RECIPIENT=Public/<account-id> \
  cargo test -p lez-faucet-ffi --test live_public_testnet \
  one_authorized_claim_credits_exactly_the_prize \
  -- --ignored --exact --nocapture
```

It reads the pool and the recipient before and after, requires an exact `+150`,
and then replays the same request key to prove that a repeat does not produce a
second claim. The destination must already be initialized: the faucet will not
initialize it, and needs no secret from its owner.

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
