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

## Create and fund an account

1. Leave the sequencer set to `https://testnet.lez.logos.co` unless you are
   deliberately using a compatible localnet.
2. Read and accept the plaintext-wallet warning.
3. Choose local config and storage paths, and create the wallet.
4. Save the recovery mnemonic from the one-time recovery screen. It cannot be
   shown again by the app.
5. Create and initialize the public account. Wait for the initialization
   transaction to be confirmed.
6. Use **Claim 150** for one claim, or enter a target and use **Claim until
   target**. The app confirms each claim before starting the next one.

The public testnet can take tens of seconds to include a transaction. Do not
close Basecamp while an initialization or claim is pending. A submission is not
reported as successful until inclusion and the expected balance change are
observed.

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
  retry only after the account state is visible from the sequencer.
- **Claim solution rejected:** refresh the pinata challenge. Another successful
  claim changes the challenge seed.
- **Catalog is empty:** verify that the `index` release contains `index.json`
  and that both module releases carry an `.lgx` and `sidecar.json`.
