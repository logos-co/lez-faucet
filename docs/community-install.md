# Install LEZ Faucet in Basecamp

LEZ Faucet is distributed through this repository's own Basecamp catalog. It is
not currently part of the built-in Logos module catalog.

## Requirements

- Logos Basecamp 0.2.1
- Apple Silicon macOS (`darwin-arm64`)
- Public-testnet use only

Linux and Intel macOS packages are not published yet because the required
circuit and rapidsnark fixed-output artifacts have only been validated for
Apple Silicon.

## Add the catalog

In Basecamp, open **Settings → Repositories** and add:

```text
https://raw.githubusercontent.com/logos-co/lez-faucet/main/logos-repo.json
```

The repository descriptor points Basecamp at the rolling catalog index hosted
in this repository's `index` GitHub release. Before the first module releases
and index rebuild exist, the repository can be added but will contain no
installable packages.

## Install the app

Install **`lez_faucet_ui`** from the package browser. It declares
**`lez_faucet`** as a dependency, so Basecamp should install the core module
automatically from the same catalog. If the package manager does not offer
dependency resolution, install `lez_faucet` first and then `lez_faucet_ui`.

Installing only `lez_faucet` does not add a visible panel; the UI package is the
app entry point. Restart Basecamp if a newly installed view does not appear.

## Create and fund a local account

1. Leave the sequencer set to `https://testnet.lez.logos.co` unless you are
   deliberately using a compatible localnet.
2. Read and accept the plaintext-wallet warning.
3. Create the wallet. LEZ Faucet stores its config, plaintext wallet, and faucet
   state under Basecamp's application-data directory; v0.1 does not expose a
   custom path chooser.
4. Save the recovery mnemonic from the one-time recovery screen. It cannot be
   shown again by the app.
5. Create and initialize the public account. Wait for the initialization
   transaction to be confirmed.
6. Use **Claim 150** for one claim, or enter a target and use **Claim until
   target**. The app confirms each claim before starting the next one.

## Fund an existing public account

LEZ Faucet can also send faucet claims to a public account created in another
LEZ wallet, including a node or CLI wallet. It does not import that wallet's
keys or take ownership of the recipient.

The recipient must already:

- use a `Public/<account-id>` address;
- be initialized on the same sequencer; and
- be owned by the authenticated-transfer program.

If necessary, initialize the account from the LEZ wallet that actually owns its
signing key before opening the faucet:

```sh
LEE_WALLET_HOME_DIR=/path/to/owning-wallet \
  wallet auth-transfer init --account-id Public/<account-id>
```

Then fund it in Basecamp:

1. Create or open the app's local faucet wallet. The Rust client currently
   requires this local wallet as its LEZ `WalletCore` network context, but it
   does not use it to own or sign for the external recipient. On first use,
   follow the plaintext-storage warning and save the local wallet's one-time
   mnemonic.
2. Choose **Fund an existing public account instead** during initialization, or
   enable **Fund an existing public account** on the ready screen.
3. Paste `Public/<account-id>` or the bare base58 account ID.
4. Select **Check account and balance**. This preflight rejects an uninitialized
   account or one not owned by the authenticated-transfer program.
5. Review the normalized account ID and fetched balance, then select the
   explicit confirmation that this is the account you intend to fund.
6. Use **Claim 150 LEZ** or **Claim until target**. Changing the recipient text
   clears the preflight and confirmation, so the new value must be checked
   again.

Funding is a public credit and does not require the faucet to possess the
recipient's signing key. The recipient remains controlled only by its original
wallet.

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
Use `LEE_WALLET_HOME_DIR` with LEZ v0.2.0, not the older
`NSSA_WALLET_HOME_DIR` name.

## Security warning

The exact v0.2.0 wallet dependency used by this release ignores the wallet
password and stores its keychain data as plaintext JSON. The password field is
not encryption.

- Use a disposable testnet-only wallet.
- Do not put the storage file in iCloud Drive, Dropbox, a shared repository, or
  another automatically synchronized location.
- Do not use a mnemonic or password associated with real assets.
- Remove the local wallet files when you no longer need the test account.

Testnet LEZ has no monetary value. This app must not be used for mainnet funds.

## Troubleshooting

- **Network version mismatch:** the sequencer fingerprint differs from the
  compiled v0.2.0 program IDs. Stop; the client must be upgraded to the exact
  testnet revision before transacting.
- **Account is uninitialized:** wait for the initialization transaction, then
  retry only after the account state is visible from the sequencer. For an
  existing recipient, initialize it from the wallet that owns its signing key;
  the faucet does not import or initialize that account.
- **Existing recipient has the wrong owner:** only a public account owned by the
  authenticated-transfer program can receive a v0.1 faucet claim.
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
