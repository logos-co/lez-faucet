# Testnet and wallet safety

## Network contract

LEZ Faucet claims on two networks. The app opens on the first and has a
selector for the second; nothing persists the choice.

| Item | LEZ legacy testnet | LEZ v0.3 testnet |
| --- | --- | --- |
| Network id | `lez-legacy-testnet` | `lez-v03-testnet` |
| Sequencer RPC | `https://testnet-legacy.lez.logos.co` | `https://testnet.lez.logos.co` |
| Explorer | `https://explorer.testnet-legacy.lez.logos.co` | `https://explorer.testnet.lez.logos.co` |
| LEZ source tag | `v0.2.4-lez-legacy` | `v0.3.1` |
| LEZ source commit | `399793b4f99b836e40694cea34fa02790912e18e` | `1a003bfbfb89fcf987f62975cd08a401169e1b8c` |
| Claim mechanism | Piñata (unsigned `PublicNoSign` claim) | Claim API (hosted `claimd`, see below) |
| Pool | Pinata account `EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7` | the service's hot account |
| Unit | whole LEZ (0 decimals) | lepta, 10^9 per LEZ (9 decimals) |
| Claim value | 150 LEZ | 100,000,000 lepta = 0.1 LEZ |
| Fingerprint | compiled `authenticated_transfer` + `pinata` ImageIDs | `privacy_preserving_circuit` ImageID, as data |

Each column is one row of the network table in `lez-faucet-ffi/src/network.rs`,
which is the copy the build reads: the sequencer host the client will accept,
the explorer it links receipts to, the LEZ tag it runs, the fingerprint it
expects, the unit and the prize, and how a claim is made there.
`faucet-ui/src/FaucetBackend.cpp` carries the display half of the same table,
and the core-contract test holds the two to each other. Moving the app to a
different sequencer means changing the table, not a URL.

### Why legacy, and what happened to the main testnet

The main testnet, `https://testnet.lez.logos.co`, **upgraded to LEZ v0.3.x at
the start of October 2026**. v0.3 ships neither the `pinata` nor the
`authenticated_transfer` builtin program — its `getProgramIds` reports only
`privacy_preserving_circuit` — so there is nothing on it for this app to claim
from. Every shipped build has refused every claim there with
`program_fingerprint_mismatch` since; that is what issue #20 reported daily.

Logos kept the previous chain running as the legacy testnet at
`testnet-legacy.lez.logos.co`, on tag `v0.2.4-lez-legacy` (published
2026-10-13). It carries the v0.2.x program set, including Piñata, with its
pool at the full 1,500,000. That is the stable testnet residents are expected to
use, so from LEZ Faucet 0.4.0 the app targets it.

The client is built to refuse the v0.3 host outright rather than fail its
fingerprint check against it: an account on one testnet is not an account on
the other, and a window that *looked* pointed at the main testnet would be
worse than one that says where it is. The UI states the network it is on.

### The v0.3 testnet and why a hosted service

LEZ Faucet 0.5.0 adds the v0.3 testnet as a second row. v0.3 has no Piñata and,
more fundamentally, no fee-less public transaction: every user transaction has
to be signed by a funded fee payer. A keyless client cannot produce one. So on
v0.3 the claim is made by **`claimd`**, a small service the team runs, which
signs one native transfer from its own hot account and pays the fee. Its HTTP
contract, policy and operations are in [`claimd/README.md`](../claimd/README.md);
the public root is `https://rog-devbox.tail957e2.ts.net:8443`.

What the app does on v0.3, in order: verify the sequencer's
`privacy_preserving_circuit` ImageID against the row; read the recipient's
balance with `getAccountBalance`; `POST /v1/claim` with the recipient and the
same request key it uses for its own idempotency; then poll
`GET /v1/claim/{key}` for the transaction hash while reading `getTransaction`
and the balance itself, until it has seen **its** transaction on chain and the
balance at exactly `before + 100,000,000`. The service's own `included` is
never taken as proof. There is no initialization step on v0.3 — any valid
32-byte base58 id can receive a native transfer, and an unseen one reads as
balance 0 — so the uninitialized/wrong-owner states and the init command do
not exist on that network.

**Trust statement.** The app is as keyless on v0.3 as on legacy: it holds no
key, signs nothing, and stores nothing. What it adds is a dependency on one
service, which it treats as a counterparty rather than an oracle — every
receipt is still the app's own two reads. The service holds only a small hot
balance (topped up from the team's mined supply when it drops below
300,000,000 lepta) and never loads the treasury key. It caps claims at one per
recipient per day, two per client IP per day, and 3/hour, 10/day overall; a
refusal arrives as `rate_limited` (with `details.retry_after_seconds`),
`faucet_paused` or `pool_depleted`, and records nothing, so the app offers no
retry on the first two and a fresh attempt is honest on `claim_api_unavailable`
(the service could not be reached and has no record of the request).

Adding a further network is the same shape: a `Network` constant with its own
`ClaimMechanism` and `NetworkFingerprint`, added to `network::ENABLED`, and a
mirrored row in the backend's table.

### History

- **2026-08-05**: the testnet was reset and upgraded from `v0.2.0` to
  `v0.2.2`. The reset discarded all prior chain history, so block and
  transaction links from before that date no longer resolve, and accounts
  initialized on the old chain had to be initialized again.
- **2026-10-01 (approx.)**: the main testnet moved to `v0.3.1` without
  Piñata. The pre-upgrade chain continued as the legacy testnet, which is
  where the app now points. The Piñata account ID is derived, not allocated,
  so it is the same on legacy as it was before; its pool on legacy stood at
  1,500,000 when the app was repinned.

The pin is a protocol requirement, not a convenience. Builtin program IDs are
derived from program ELFs embedded in the client. A client built from a
different LEZ revision may serialize a transaction that the sequencer accepts
but does not execute as intended. (The v0.3 row escapes this only because the
app builds no transaction there: the service does, from its own v0.3.1 build.)

The backend performs a runtime fingerprint check against `getProgramIds` before
creating transactions. At the pinned deployment, the relevant program IDs are
(they are unchanged from `v0.2.2`: the legacy tag changed the sequencer and the
`logos-blockchain` dependency, not the guest programs):

| Program | ImageID as 32-byte little-endian hex |
| --- | --- |
| `authenticated_transfer` | `fe96c4228babbe8bc578e3e25b884cacb07f8c86541f27ed676789875eef875a` |
| `pinata` | `fc52f17a60f8b5e8de28e1a8c3133c012485011a36aef985ce24d69ff4f3528c` |

The v0.3 testnet reports exactly one program. This build compiles nothing for
v0.3, so its ImageID is data in the network row, compared live before every
v0.3 claim. It is recorded here in both forms: the hex the row carries, and the
raw words the sequencer returns, which the `v03` job of the fingerprint
workflow compares with `curl` and `jq`:

| Program | Value |
| --- | --- |
| `privacy_preserving_circuit` (hex) | `189513eb0a037edb638b3da824952e69894a4d11ab05c5956a5986181ab02721` |
| `privacy_preserving_circuit` (words) | `[3943929112,3682468618,2822605667,1764660516,290278025,2512717227,411457898,556249114]` |

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
  https://testnet-legacy.lez.logos.co | jq
```

For v0.3, the same call against `https://testnet.lez.logos.co` returns only
`privacy_preserving_circuit`, and the claim service's health is
`curl -fsS https://rog-devbox.tail957e2.ts.net:8443/v1/info`. The `v03` job in
`.github/workflows/testnet-fingerprint.yml` runs both daily; it needs no
toolchain and opens the same `testnet-drift` issue on a circuit change.

Do not update these values independently. A testnet upgrade requires updating
the LEZ git pin, rebuilding the embedded programs, refreshing the expected
fingerprint, and rerunning the complete account-init/claim test.

### Repin procedure

When the legacy testnet upgrades (or when a second network lands), move the pin
in this order; each step is checked by the next.

1. Resolve the new tag to a commit (`git ls-remote --tags`; annotated tags
   need the `^{}` peeled form). Replace the old commit in `Cargo.toml`,
   `scaffold.toml` and `faucet-module/flake.nix`'s `logos-execution-zone`
   input, then `cargo update -p common` to move every LEZ git checkout in
   `Cargo.lock`.
2. Check what moved underneath. `v0.2.4-lez-legacy` moved `logos-blockchain`
   to its `0.3.1` tag, which raised the minimum Rust to 1.98 (`usize::bit_width`
   in a const fn), moved `logos-blockchain-circuits` from v0.5.3 to v0.5.7 (the
   prebuilt archives `faucet-module/flake.nix` fetches, so all three
   `circuitsHash` values change: `nix-prefetch-url --unpack <url>`), and pulled
   in `rocksdb` through `common` → `common-http-client` → `chain-service`,
   which needs `libclang` at build time (`bindgenHook` in the flake; the CI
   runners already install it). `lez-faucet-ffi/Cargo.toml` records
   `rust-version` so an old toolchain fails with a clear message.
3. Update the network row in `lez-faucet-ffi/src/network.rs` (tag, commit,
   ImageIDs if they moved) and `cargo test -p lez-faucet-ffi`. The offline
   test `programs_match_the_compiled_build` fails if the row disagrees with
   the ELFs compiled from the new pin; that is the check that keeps the table
   honest.
4. Regenerate the UI contract fixture (`cargo run -p lez-faucet-ffi --example
   dump_contract_fixtures > faucet-ui/tests/fixtures/core-contract.json`) and
   run the UI and core-module test suites.
5. Run the read-only live tests, then the one write test (see below) against
   a fresh account, and record the claim here.
6. Only then refresh the ImageID table above, bump both `metadata.json`
   versions (and `version()` in `lez_faucet_module.cpp`, `Cargo.toml`,
   `flake.nix`), and set `cargoDeps.hash` in `faucet-module/flake.nix` from a
   `lib.fakeHash` build — the vendor hash moves on any `Cargo.lock` change.

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

Read-only checks against the public testnets are safe at any time:

```sh
cargo test -p lez-faucet-ffi --test live_public_testnet -- --ignored --nocapture
cargo test -p lez-faucet-ffi --test live_v03_testnet -- --ignored --nocapture
```

The v0.3 file asserts the circuit ImageID matches the row, that the claim
service's `/v1/info` reports the row's prize and decimals, and that any public
id is eligible with a balance. `LEZ_FAUCET_CLAIM_API_URL` points it at a
service under test; the sequencer stays pinned.

They assert the pinned program fingerprint matches the deployed one, that the
pool balance is a whole multiple of the 150 prize, that `claims_remaining` is
`floor(pool / 150)`, and that malformed, oversized and `Private/` addresses are
refused before any network call.

There is exactly one write test. It spends 150 LEZ from a finite shared pool, so
it requires the destination to be named explicitly and skips otherwise:

```sh
LEZ_FAUCET_LIVE_RECIPIENT=Public/<account-id> \
  cargo test --release -p lez-faucet-ffi --test live_public_testnet \
  one_authorized_claim_credits_exactly_the_prize \
  -- --ignored --exact --nocapture
```

`--release` is not optional. The claim's 24-bit proof-of-work has a 60-second
`solve_deadline`, and the unoptimized `dev` profile misses it on an Apple
Silicon laptop (`SolveTimeout` in phase `solving`, before anything is sent, so
nothing is spent — but nothing is tested either).

It reads the pool and the recipient independently before and after, requires an
exact `+150`, and then replays the same request key to prove a repeat produces
no second claim.

This was last run against the legacy testnet at `v0.2.4-lez-legacy` on
2026-10-21: claim `c1d6b85a5a6b8f29bd110081d89f98a66e421566b6e082567cf0c8716f8be680` credited
`Public/7GeNUNLaDCPsfq7Qaj23ajxqK2t7SCexSRrD8aWWCu35` from 0 to 150, moving the
pool from 1500000 to 1499850 (https://explorer.testnet-legacy.lez.logos.co/transaction/c1d6b85a5a6b8f29bd110081d89f98a66e421566b6e082567cf0c8716f8be680).

The run before that was against the main testnet at `v0.2.2` on 2026-08-10:
claim `4581848e26271a512ec2dfacbdfad568b1dede31d96821685536958006968b88`
credited `Public/AwTjnoBMZZ7KQCKxYYkcv3npTXZqXyYnFwwHkdw7Az5i` from 0 to 150.
That chain continues as the legacy testnet, so the transaction still resolves
in the legacy explorer.

The v0.3 write test asks the service for one 0.1 LEZ drop and is gated the
same way. Use a fresh throwaway id — the service pays each recipient once a
day — and the release profile is not required, since there is no
proof-of-work:

```sh
LEZ_FAUCET_LIVE_RECIPIENT=Public/<fresh-account-id> \
  cargo test --release -p lez-faucet-ffi --test live_v03_testnet \
  one_authorized_claim_credits_exactly_the_prize \
  -- --ignored --exact --nocapture
```

First run against the public service on 2026-10-22 (all four `live_v03_testnet`
tests passed, 99 s): recipient `Public/8HjcNL4Y4duo2zMCPCKtJXm1qUS3AtnQ1HnRmZsiZQtW`
went from 0 to 100,000,000 lepta through transaction
`12811d98d37e1285867f925613fa3bc4599a57c03325c0c2049648ab1e124e3c`
(https://explorer.testnet.lez.logos.co/transaction/12811d98d37e1285867f925613fa3bc4599a57c03325c0c2049648ab1e124e3c).
The service's hot balance moved by exactly the prize plus the 2,968 lepta fee
(1,029,997,032 to 929,994,064).

The destination on the **legacy** testnet must already be initialized under
authenticated-transfer. A public account ID alone cannot authorize
initialization; the owner runs
`wallet auth-transfer init --account-id Public/<account-id>` in their own wallet
context. The faucet copies that command for them and never requests a recovery
phrase or private key. Private account IDs are not accepted: private funding
would additionally require privacy keys, synchronized private state and proof
handling.

### Getting a destination account

A destination has to exist on the legacy testnet specifically; an account
created against the main testnet's wallet default is on the wrong chain. Four
things about the pinned wallet cost real time if you meet them cold:

- **The wallet CLI defaults to the main testnet.** `wallet change-network
  testnet` and the config default both point at `testnet.lez.logos.co`, which
  is v0.3 and has no `authenticated_transfer` to initialize under. Write the
  config by hand before the first command:

  ```sh
  export LEE_WALLET_HOME_DIR=$(mktemp -d)
  cat > "$LEE_WALLET_HOME_DIR/wallet_config.json" <<'EOF'
  {
    "sequencers": [{ "sequencer_addr": "https://testnet-legacy.lez.logos.co" }],
    "seq_poll_timeout": "30s",
    "seq_tx_poll_max_blocks": 15,
    "seq_poll_max_retries": 10,
    "seq_block_poll_max_amount": 100,
    "calibration_limit": 100
  }
  EOF
  wallet check-health          # creates the keychain, prints a recovery phrase
  wallet account new public    # prints Public/<account-id>
  wallet auth-transfer init --account-id Public/<account-id>
  ```

- **The flake's `wallet` output is not the CLI.** At `399793b4` as at
  `d6e4ae69`,
  `packages.<system>.wallet` (and `.default`) is the `wallet-ffi` shared
  library; no flake output contains a CLI binary, so `nix run …#wallet` cannot
  work. Build it from the source tree instead, which needs none of the
  `LBC_ROOT_DIR` / `RAPIDSNARK_LIB_DIR` nix-sandbox variables:

  ```sh
  cargo build --release -p wallet --locked --target-dir /tmp/lez-wallet-target
  ```

  The tree's `rust-toolchain.toml` asks for 1.98.1; a six-minute release build
  on an Apple Silicon laptop.

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
  The shape above is the `v0.2.4-lez-legacy` one and loads unchanged.

The write test mutates shared public Piñata state. Run only one at a time.
