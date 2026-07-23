# Public testnet and wallet safety

## Network contract

LEZ Faucet targets only the public testnet:

| Item | Value |
| --- | --- |
| Sequencer RPC | `https://testnet.lez.logos.co` |
| Explorer | `https://explorer.testnet.lez.logos.co` |
| LEZ source tag | `v0.2.0` |
| LEZ source commit | `a58fbce2ff48c58b7bb5001b1a27e64b9596ee3a` |
| Pinata account | `EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7` |
| Claim value | 150 testnet LEZ |

The pin is a protocol requirement, not a convenience. Builtin program IDs are
derived from program ELFs embedded in the client. A client built from a
different LEZ revision may serialize a transaction that the sequencer accepts
but does not execute as intended.

The backend performs a runtime fingerprint check against `getProgramIds` before
creating transactions. At the pinned deployment, the relevant program IDs are:

| Program | ImageID as 32-byte little-endian hex |
| --- | --- |
| `authenticated_transfer` | `dcbbfebcd59399961ed9973b8307dc475fd4c5ca5779aacfe7588f7dbc3f4a71` |
| `pinata` | `66f6a58d92c159c3c13ea54d1e37a68a814f0fd3b8fd44b7d35c0617ac4456f8` |

You can inspect the live response with:

```sh
curl -fsS -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getProgramIds","params":[]}' \
  https://testnet.lez.logos.co | jq
```

Do not update these values independently. A testnet upgrade requires updating
the LEZ git pin, rebuilding the embedded programs, refreshing the expected
fingerprint, and rerunning the complete account-init/claim test.

## Wallet creation and the plaintext-storage risk

At v0.2.0, upstream `wallet::Storage::new` and `Storage::restore` explicitly
discard the password argument. `Storage::save_to_path` serializes its persistent
keychain data directly to JSON. Consequently:

- The password entered in the app does not encrypt the wallet file.
- File permissions and local machine security are the only protection.
- The wallet must be treated as disposable public-testnet state.
- The app must never describe the stored wallet as encrypted or password-protected.

This project keeps the password field because it is part of the upstream API
and future versions may implement encryption. Until the exact pinned dependency
changes and is audited, the UI must display the plaintext-storage warning next
to wallet creation.

## One-time mnemonic contract

Wallet creation returns a BIP-39 recovery mnemonic. LEZ Faucet applies this
handling contract:

1. Receive it from the Rust wallet creation call.
2. Show it only on the recovery screen.
3. Require explicit confirmation that the user saved it.
4. Clear backend and QML copies when leaving that screen.
5. Never write it to logs, settings, telemetry, error messages, screenshots, or
   test output.

The app cannot reveal the same mnemonic later. Losing both the mnemonic and the
plaintext wallet file means losing control of the derived account.

## Transaction sequence

A fresh public account is not immediately usable. The backend must complete
these operations in order:

1. Create the wallet and persist its keychain.
2. Derive one public account and persist the updated keychain.
3. Submit authenticated-transfer `Initialize` with that account's key.
4. Poll the transaction until it is included; surface rejection or timeout.
5. Confirm the account is no longer the default uninitialized state.
6. Fetch the current pinata challenge and compute its solution.
7. Submit one unsigned public pinata claim.
8. Reconcile the transaction, current challenge, and account balance. Normal
   success proves inclusion and an exact balance increase of 150. If the
   submission response was lost and no transaction hash is available, that
   exact balance increase together with challenge rotation proves success with
   a null `tx_hash`. If the evidence remains ambiguous, return an explicit
   unknown outcome and do not resubmit.

For “claim until target,” repeat steps 6–8 sequentially. The pinata program
hashes its seed after each successful claim, so an old solution cannot safely be
reused and concurrent claims would race the same challenge.

## Live verification

The network-writing integration test is ignored by default and requires an
explicit acknowledgement value:

```sh
LEZ_FAUCET_LIVE_TEST=I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ \
  cargo test -p lez-faucet-ffi --test live_public_testnet \
  -- --ignored --nocapture
```

Success evidence must include:

- the new public account ID;
- initialization transaction ID;
- claim transaction ID when available, otherwise an explicit unknown-hash
  marker backed by the exact balance/challenge reconciliation;
- balance before and after the claim;
- an assertion that `after == before + 150`.

The test must use fresh temporary local storage and must not print its mnemonic,
password, or keys.
