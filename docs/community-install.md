# Install LEZ Faucet in Basecamp

LEZ Faucet is distributed through this repository's own Basecamp catalog. It is
not currently part of the built-in Logos module catalog.

## Requirements

- **LEZ Faucet 0.3.1** is the version documented here, for both packages
  (`lez_faucet` and `lez_faucet_ui`).
- Requires **Logos Basecamp 0.2.1**. That 0.2.1 is the host application's
  version, not the faucet's. The two numbers are unrelated and are not meant to
  match.
- Apple Silicon macOS (`darwin-arm64`), x86-64 Linux (`linux-amd64`), or ARM64
  Linux (`linux-arm64`)
- Public-testnet use only

Version numbers in these docs always name their subject. Where you see LEZ
`v0.2.2`, that is the pinned upstream protocol revision this client is built
against, and it is a third independent number.

Intel macOS is not published. The required circuit artifacts
(logos-blockchain-circuits v0.5.3) exist for Apple Silicon and for both Linux
architectures, but not for macOS x86_64, so there is nothing to build against.

The already-published 0.1.0 and 0.2.0 packages predate Linux support and carry
`darwin-arm64` only. A package's variants are recorded in its release notes and
in the `sidecar.json` asset beside it; check them rather than assuming.

## Add the catalog

In Basecamp, open **Settings → Repositories** and add:

```text
https://raw.githubusercontent.com/logos-co/lez-faucet/main/logos-repo.json
```

The repository descriptor points Basecamp at the rolling catalog index hosted
in this repository's `index` GitHub release. The index is rebuilt from every
non-draft release, so 0.3.0, 0.2.0 and 0.1.0 all remain listed alongside 0.3.1
and can be rolled back to. Note that rolling back is not useful against the
current testnet: every release before 0.3.1 is pinned to LEZ `v0.2.0` and
refuses each claim with "This app does not match the deployed testnet".

## Install the app

For a new installation, install **`lez_faucet_ui`** from the package browser. It
declares **`lez_faucet`** as a dependency, so Basecamp should install the newest
core module automatically from the same catalog.

For an existing 0.1.0 or 0.2.0 installation, upgrade or install **`lez_faucet`
0.3.1 first**, then upgrade **`lez_faucet_ui` to 0.3.1**. The UI dependency is
currently unversioned, so upgrading the UI alone may treat an installed older
core as sufficient and leave it in place. If automatic dependency resolution
is unavailable, use this same core-then-UI order for a new installation.

Get that order wrong and the app does not merely look old: 0.3.0 removed the
wallet flow and changed the core module's interface, so a 0.3.x UI calls slots a
0.2.x core does not have. Expect errors on the first action rather than a
working, older screen.

Installing only `lez_faucet` does not add a visible panel; the UI package is the
app entry point. Restart Basecamp if a newly installed view does not appear.

## Fund an existing public account

LEZ Faucet can also send faucet claims to a public account created in another
LEZ wallet, including a node or CLI wallet. It does not import that wallet's
keys or take ownership of the recipient.

The recipient must already:

- use a `Public/<account-id>` address;
- be initialized on the same sequencer; and
- be owned by the authenticated-transfer program.

New public accounts start uninitialized. Initialization claims the account for
the authenticated-transfer program and must be authorized by the wallet that
owns the account's signing key. The faucet cannot do this from an address alone,
and it must never be given the recipient mnemonic or private key.

If necessary, initialize the account from the LEZ wallet that actually owns its
signing key:

```sh
wallet auth-transfer init --account-id Public/<account-id>
```

Run this in the owning wallet's context. When using a non-default wallet home,
set that wallet's `LEE_WALLET_HOME_DIR` environment variable before the
command; a different wallet home cannot authorize initialization.

Then fund it in Basecamp. This is the 0.3.x screen: one address field and one
button. If you see a password or recovery-phrase step instead, you are running
0.2.0 and the core-then-UI upgrade above did not complete.

1. Open the LEZ Faucet app. There is no onboarding: no account to create, no
   password, no recovery phrase.
2. Paste `Public/<account-id>` or the bare 32-byte base58 account ID. Private
   account IDs are not accepted.
3. The app reports malformed IDs, valid-but-uninitialized accounts, and
   accounts owned by another program as separate states. For an uninitialized
   account it shows the exact command for **you** to run in the owning wallet.
4. Press **Request 150 LEZ** once.
5. Watch the phases. Before the transaction is sent you can cancel; after it is
   sent the app can only reconcile it, so cancelling then reports the real
   outcome rather than pretending nothing happened.
6. A receipt appears only when the app has seen its own transaction included
   *and* your balance up by exactly 150. If it cannot prove that in time it
   says so, shows the transaction hash, and offers no retry — re-check the
   balance yourself before trying again.

Funding is a public credit and does not require the faucet to possess the
recipient's signing key. The recipient remains controlled only by its original
wallet.

Only public authenticated-transfer accounts are supported. A
`Private/<account-id>` is not enough to fund a private account: private
recipient handling additionally needs privacy public keys, synchronized private
state, and proof/decryption support. LEZ Faucet 0.3.x collects no key material
of any kind and has no surface that could accept it.

Attribute that to 0.3.0 and no earlier. The shipped 0.2.0 screen did ask for a
"Wallet password", which the pinned wallet API accepted and then ignored; see
`docs/screenshots/README.md`. 0.3.0 removed that field rather than relabelling
it, which is exactly why this release is a breaking one.

The public testnet can take tens of seconds to include a transaction. Do not
close Basecamp while an initialization or claim is pending. Normal claim
success proves transaction inclusion and the exact `+150` balance change. If a
submission response is lost, the faucet may instead prove success from that
exact balance change plus pinata challenge rotation; the receipt then has no
transaction hash. If it cannot prove success or a safe retry, it reports an
unknown outcome and does not submit another claim. Reconcile the balance before
trying again.

## Verify the drop from a terminal

The local or existing recipient account ID displayed by LEZ Faucet is public,
so its native balance can be checked independently without opening its owning
wallet or exposing any keys. After a claim, run this from a checkout of the
repository:

```sh
./scripts/lez-balance.sh Public/<account-id>
# example output after one claim:
# 150
```

The script accepts either `Public/<account-id>` or the bare base58 account ID,
uses `https://testnet.lez.logos.co` by default, and prints only the exact decimal
balance on success. It requires `curl` and Python 3; Python's arbitrary-precision
integer parser avoids rounding LEZ's `u128` balances. Override the endpoint only
when checking a compatible sequencer:

```sh
LEZ_FAUCET_SEQUENCER_URL=https://sequencer.example.test \
  ./scripts/lez-balance.sh Public/<account-id>
```

Users with `jq` and the pinned upstream LEZ wallet CLI can make the same
read-only query:

```sh
LEE_WALLET_HOME_DIR=/path/to/wallet \
  wallet account get --raw --account-id Public/<account-id> |
  jq -er '.balance'
```

The CLI wallet home is used only as the network-client context and does not need
to own the account being checked. It must already contain `wallet_config.json`
and `storage.json`, and its configuration must point at the intended sequencer.
Use `LEE_WALLET_HOME_DIR` with the pinned upstream LEZ v0.2.2 wallet CLI, not
the older `NSSA_WALLET_HOME_DIR` name.

A wallet home created before v0.2.2 will not open. That release replaced
`wallet_config.json`'s `sequencer_addr` with a `sequencers` list and added
`calibration_limit`, with no migration, so an older file fails to parse,
complaining that the `sequencers` field is missing. Create a fresh wallet home
or reshape the file by hand. The `scripts/lez-balance.sh` route above needs no wallet and is
unaffected.

## What this app does and does not do

- It funds one public, already-initialized account by exactly 150 testnet LEZ
  per press.
- It never asks for a password, recovery phrase or private key, and it has no
  way to accept one.
- It writes no files and remembers nothing after you quit.
- It cannot initialize an account for you. If yours is not initialized, the app
  shows the command for **you** to run in your own wallet.

The Piñata pool is finite and shared: it started at 1,500,000 testnet LEZ, pays
150 per claim, and every claim is a proof-of-work race against everyone else
for one global challenge. It is permissionless and repeatedly claimable, but it
is not unlimited and it will run out. The deployed program has no cooldown and
no per-address quota; this app's internal limits keep it well behaved on your
machine and on a shared sequencer, and are not abuse prevention.

Testnet LEZ has no monetary value.

If you quit the app while a claim is running, it cannot tell you afterwards
whether that claim went through. Check the balance independently.

## Troubleshooting

- **Network version mismatch** ("This app does not match the deployed testnet"):
  the sequencer's program IDs differ from the ones compiled from the pinned
  upstream LEZ v0.2.2 revision, which means the testnet has been upgraded past
  this build. Nothing you can do in the app fixes it, and nothing is wrong with
  your account. Install the newest LEZ Faucet release from the catalog; if the
  banner persists on the newest release, the app has not been repinned yet —
  open an issue. This is what the testnet's 2026-08-05 upgrade did to every
  0.3.0 build, which 0.3.1 repins.
- **Account is uninitialized:** use **Copy command**, run it from the wallet
  that owns the account, wait for the initialization transaction to be visible
  from the sequencer, then use **Re-check account**. The faucet does not import
  or initialize that account and never needs its mnemonic or private key.
- **Existing recipient has the wrong owner:** only a public account owned by the
  authenticated-transfer program can receive a faucet claim.
- **Claim solution rejected:** refresh the pinata challenge. Another successful
  claim changes the challenge seed.
- **Claim outcome unknown:** do not immediately claim again. Reconnect and
  refresh the balance first; the prior submission may have been accepted even
  though its transaction hash or final polling response was lost.
- **Catalog is empty:** verify that the `index` release contains `index.json`
  and that both non-draft module releases carry the expected `.lgx` asset. The
  index is built from those `.lgx` download URLs, not from `sidecar.json`.
  Each module release should still carry `sidecar.json` as its artifact
  metadata and to satisfy the release workflow's already-published check.
