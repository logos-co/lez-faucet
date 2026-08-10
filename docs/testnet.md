# Public testnet and wallet safety

## Network contract

LEZ Faucet targets only the public testnet:

| Item | Value |
| --- | --- |
| Sequencer RPC | `https://testnet.lez.logos.co` |
| Explorer | `https://explorer.testnet.lez.logos.co` |
| LEZ source tag | `v0.2.2` |
| LEZ source commit | `d6e4ae694e7419f5906b340c232704466a1917b7` |
| Pinata account | `EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7` |
| Claim value | 150 testnet LEZ |

The testnet was **reset and upgraded on 2026-08-05**, from `v0.2.0` to `v0.2.2`.
The reset discarded all prior chain history, so block and transaction links from
before that date no longer resolve in the explorer, and accounts initialized on
the old chain must be initialized again. The Piñata account ID is derived, not
allocated, so it survived the reset unchanged.

The pin is a protocol requirement, not a convenience. Builtin program IDs are
derived from program ELFs embedded in the client. A client built from a
different LEZ revision may serialize a transaction that the sequencer accepts
but does not execute as intended.

The backend performs a runtime fingerprint check against `getProgramIds` before
creating transactions. At the pinned deployment, the relevant program IDs are:

| Program | ImageID as 32-byte little-endian hex |
| --- | --- |
| `authenticated_transfer` | `fe96c4228babbe8bc578e3e25b884cacb07f8c86541f27ed676789875eef875a` |
| `pinata` | `fc52f17a60f8b5e8de28e1a8c3133c012485011a36aef985ce24d69ff4f3528c` |

The `Testnet fingerprint` workflow checks this daily, so that an upgrade is
reported here before a user meets it as a red banner in the app. It runs two
checks, and the difference between them matters.

The **authoritative** one compares the ImageIDs *compiled into the build*
against the sequencer — the same comparison the runtime gate makes:

```sh
cargo test -p lez-faucet-ffi --test live_public_testnet \
  faucet_info_matches_the_pinned_protocol -- --ignored
```

The **advisory** one compares the sequencer against the table above. It needs
no toolchain, so it answers in a second and names the drift precisely:

```sh
scripts/check-program-fingerprint.sh
```

The table is documentation, not the contract. Editing it would silence the
advisory check while leaving every build unable to claim, which is why the
compiled comparison is the gate.

You can also inspect the live response directly:

```sh
curl -fsS -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"getProgramIds","params":[]}' \
  https://testnet.lez.logos.co | jq
```

Do not update these values independently. A testnet upgrade requires updating
the LEZ git pin, rebuilding the embedded programs, refreshing the expected
fingerprint, and rerunning the complete account-init/claim test.

## No wallet, no key material

vNext holds no wallet and no key material, so the plaintext-storage risk that
applied to earlier releases no longer applies to this app.

A Piñata claim names the pool and the recipient as `PublicNoSign`, so the
transaction carries no signatures and no nonces. At the pinned revision this is
verifiable in three places: the facade builds both accounts as `PublicNoSign`
(`lez/wallet/src/program_facades/pinata.rs`); the `PublicNoSign` arm of
`AccountManager` sets `sk = None` (`lez/wallet/src/account_manager.rs:225-235`),
so `sign_message` and `public_account_nonces` both return empty; and the
state machine only requires that the nonce and signature lists have equal
length (`lee/state_machine/src/validated_state_diff/mod.rs`). An empty witness set
is an ordinary, exercised shape upstream — the per-block clock transaction is
built exactly the same way.

The app therefore never accepts a password, recovery phrase or private key, and
writes no files. Earlier releases asked for a password that the pinned wallet
API accepted and ignored; that surface has been removed rather than relabelled.

## Transaction sequence

A fresh public account is not immediately usable. The backend must complete
these operations in order:

1. Create the wallet and persist its keychain.
2. Derive one public account and persist the updated keychain.
3. Submit authenticated-transfer `Initialize` with that account's key.
4. Poll the transaction until it is included; surface rejection or timeout.
5. Confirm the account is no longer the default uninitialized state.
6. Fetch the current pinata challenge and compute its solution.
7. Compute the transaction hash locally, then submit one unsigned public
   pinata claim. The hash is a pure function of the transaction bytes and is
   the same value the sequencer returns, so a lost submission response never
   leaves the client unable to identify its own transaction.
8. Reconcile. Success requires the client's own transaction observed included
   **and** the recipient at exactly `balance_before + 150`.

A losing claim never reaches a block. The guest returns without calling
`ProgramOutput::write`, so it commits an empty journal; decoding that fails as
`ProgramExecutionFailed`; and `build_block_from_mempool` logs and skips such a
transaction instead of sealing it in. Inclusion is therefore itself proof of
payout.

A second attempt is made only once the client's own transaction is absent, the
recipient balance is unchanged, and the challenge has rotated. The balance
check carries the safety: `apply_state_diff` runs while the block is still
being assembled and the block is stored afterwards, so a credit is visible in
the balance no later than the transaction is visible by hash. Rotation alone is
never evidence — every claimant races the same challenge.

The pinata program hashes its seed after each successful claim, so an old
solution cannot be reused and all claimants race one global challenge.

## Live verification

Read-only checks against the public testnet are safe at any time:

```sh
cargo test -p lez-faucet-ffi --test live_public_testnet -- --ignored --nocapture
```

They assert the pinned program fingerprint matches the deployed one, that the
pool balance is a whole multiple of the 150 prize, that `claims_remaining` is
`floor(pool / 150)`, and that malformed, oversized and `Private/` addresses are
refused before any network call.

There is exactly one write test. It spends 150 LEZ from a finite shared pool, so
it requires the destination to be named explicitly and skips otherwise:

```sh
LEZ_FAUCET_LIVE_RECIPIENT=Public/<account-id> \
  cargo test -p lez-faucet-ffi --test live_public_testnet \
  one_authorized_claim_credits_exactly_the_prize \
  -- --ignored --exact --nocapture
```

It reads the pool and the recipient independently before and after, requires an
exact `+150`, and then replays the same request key to prove a repeat produces
no second claim.

This was last run against `v0.2.2` on 2026-08-10: claim
`4581848e26271a512ec2dfacbdfad568b1dede31d96821685536958006968b88` credited
`Public/AwTjnoBMZZ7KQCKxYYkcv3npTXZqXyYnFwwHkdw7Az5i` from 0 to 150, moving the
pool from 1498800 to 1498650.

The destination must already be initialized under authenticated-transfer. A
public account ID alone cannot authorize initialization; the owner runs
`wallet auth-transfer init --account-id Public/<account-id>` in their own wallet
context. The faucet copies that command for them and never requests a recovery
phrase or private key. Private account IDs are not accepted: private funding
would additionally require privacy keys, synchronized private state and proof
handling.

### Getting a destination account

The 2026-08-05 reset discarded every account, so a destination generally has to
be created from scratch. Three things about the pinned wallet cost real time if
you meet them cold:

- **The flake's `wallet` output is not the CLI.** At `d6e4ae69`,
  `packages.<system>.wallet` (and `.default`) is the `wallet-ffi` shared
  library; no flake output contains a CLI binary, so `nix run …#wallet` cannot
  work. Build it from the source tree instead, which needs none of the
  `LBC_ROOT_DIR` / `RAPIDSNARK_LIB_DIR` nix-sandbox variables:

  ```sh
  cargo build --release -p wallet --locked --target-dir /tmp/lez-wallet-target
  ```

- **`auth-transfer init` exits non-zero even when it succeeds.** It prints the
  transaction hash and then fails its own inclusion poller with
  `Error: All pollers failed`. The account is initialized regardless. Confirm
  with `getAccount` — `program_owner` should equal the `authenticated_transfer`
  ImageID above — rather than trusting the exit code.

- **A wallet home from before v0.2.2 will not load.** `WalletConfig` replaced
  `sequencer_addr: Url` with `sequencers: Vec<SequencerConnectionData>` and
  added `calibration_limit`, with no serde alias, default or migration. An old
  `wallet_config.json` fails to parse, complaining that the `sequencers` field
  is missing. Use a fresh `LEE_WALLET_HOME_DIR`, or reshape the file by hand.

The write test mutates shared public Piñata state. Run only one at a time.
