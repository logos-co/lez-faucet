# Changelog

This file records what moved between releases of LEZ Faucet. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html). Under 0.x a breaking
change bumps the minor version, which is why the breaking release below is 0.3.0
and not 0.2.1.

Every version here is LEZ Faucet's own, covering both packages — `lez_faucet`
and `lez_faucet_ui` are released together at the same number, and the version
source of truth is each module's `metadata.json`. These numbers are not Logos
Basecamp's version (0.2.1, the host application) and not the pinned upstream LEZ
revision (`v0.2.2`, the protocol this client is built against). Each release is
published as two GitHub releases, `lez_faucet-v<version>` and
`lez_faucet_ui-v<version>`.

## [0.3.1] — 2026-08-10

Repins the client to the upgraded public testnet. No interface changed, which is
why this is a patch and not a minor.

The testnet was reset and upgraded from LEZ `v0.2.0` to `v0.2.2` on 2026-08-05,
changing the builtin program ELFs and therefore their ImageIDs. Because those
IDs are computed client-side from ELFs embedded at build time, every 0.3.0 build
has since refused to claim, reporting **"This app does not match the deployed
testnet"**. That is the fingerprint gate working as designed — a version-skewed
client would otherwise build transactions the sequencer silently drops — so the
fix is to move the pin, not to relax the gate.

### Changed

- Pinned LEZ revision moved from `v0.2.0` (`a58fbce2`) to `v0.2.2`
  (`d6e4ae694e7419f5906b340c232704466a1917b7`) across `Cargo.toml`,
  `faucet-module/flake.nix`, `scaffold.toml` and both `flake.lock` files.
- `docs/testnet.md` records the new `authenticated_transfer` and `pinata`
  ImageIDs, and notes that the reset discarded all prior chain history —
  pre-reset explorer links are dead, and accounts initialized on the old chain
  must be initialized again.
- No client logic changed — the only source edit is the version string
  `LezFaucetModule::version()` returns, which
  `faucet-module/tests/test_lez_faucet_module.cpp` checks against
  `metadata.json`. Every upstream API this app consumes survived the bump:
  `lee`'s `public_transaction` module is byte-identical, `system_accounts` is
  untouched, and `programs` only gained functions. `getTransaction` now returns
  `Option<(LeeTransaction, BlockId)>` rather than `Option<LeeTransaction>`,
  which the existing inclusion check absorbs because it never names the payload
  type.
- The vendor hash in `faucet-module/flake.nix` was regenerated; it tracks
  `Cargo.lock`, and every LEZ git checkout in the vendored set moved.

### Added

- `scripts/check-program-fingerprint.sh` compares the sequencer's
  `getProgramIds` against the ImageIDs recorded in `docs/testnet.md`. CI runs it
  daily, so the next testnet upgrade is reported by a red build rather than
  discovered by a user meeting the red banner. It reads the expected values out
  of `docs/testnet.md`, keeping that document the single source of truth.

### Removed

- The inert `wallet` entry in `[workspace.dependencies]`. No member crate
  depended on it, so Cargo never resolved it and it has never appeared in
  `Cargo.lock` — it was stale configuration left over from the pre-0.3.0 wallet
  flow, and removing it changes nothing about what gets built.

## [0.3.0] — 2026-08-03

Breaking, and the largest change is a removal: the wallet and all key material
are gone. Both `metadata.json` files carry 0.3.0, and both packages were
published on 2026-08-03 as `lez_faucet-v0.3.0` and `lez_faucet_ui-v0.3.0`. It
is the first release to carry Linux variants: `darwin-arm64`, `linux-amd64`
and `linux-arm64`.

Upgrade the core before the UI. The core module's C++ ABI and the UI's Qt Remote
Objects interface both changed, so a 0.3.0 UI running against a 0.2.x core calls
slots that core does not implement: the result is a broken app, not an older
one.

### Removed

- The wallet. The Rust core owns no wallet, accepts no password, recovery
  phrase or private key, and writes no files. A Piñata claim names both the pool
  and the recipient as unsigned public participants, so the transaction carries
  no signatures and no nonces; the wallet was contributing nothing to the claim
  path except an HTTP client. The `wallet` and `zeroize` dependencies are
  dropped — `zeroize` not because it was unused, but because keeping it would
  imply a secret exists.
- The "Wallet password" field shipped in 0.2.0, together with the
  password/confirm screen and its plaintext-storage warning, the mnemonic
  screen, and the account-initialization screen. The pinned wallet API accepted
  that password and then ignored it: it encrypted nothing and took no part in
  key derivation (`docs/screenshots/README.md`). The surface was removed rather
  than relabelled.
- Wallet creation, mnemonic exposure, staged-file publish and rollback,
  permission hardening, and `faucet_state.json` persistence. Nothing survives a
  restart because nothing is stored.
- The local/external mode toggle, the target-balance row, "Stop after this
  claim", and the claim-until-target loop. One press is one claim.
- The UI's `LEZ_FAUCET_SEQUENCER_URL` override. The sequencer host is now pinned
  inside the client, where a redirected sequencer cannot fabricate a pool, a
  fingerprint or a success, or harvest addresses. The standalone read-only
  `scripts/lez-balance.sh` still honours its own environment override.
- `ErrorCode::IncludedWithoutCredit`, which no code path can now produce, and
  `difficultyBytes` from `poolView`, which nothing read.

### Changed

- The core module's C++ ABI. `create`, `open`, `destroy`, `verifyFingerprint`,
  `createAndInitializeAccount`, `balance`, `claimOnce` and `claimUntilTarget` are
  replaced by `configure`, `getFaucetInfo`, `inspectRecipient`, `requestDrop`,
  `cancel`, `jobStatus`, `jobResultAck` and `shutdown`. No password parameter
  survives anywhere.
- The UI's Qt Remote Objects interface, from 10 properties and 16 slots to 3 and
  8, and the view from 19 screen states to one: an address field and a button.
- Reads no longer serialize behind a running drop, so the app does not appear
  frozen for the 300 s reconciliation bound — the freeze is what provoked the
  force-quit that lost in-session idempotency.
- Terminal job status derives only from `error.code`. An `outcome_unknown` whose
  message mentions cancellation stays `outcome_unknown`, so a submitted
  transaction can never be hidden behind a "cancelled" label. In the UI, errors
  dispatch on the structured code and never on message text; the previous
  substring classifiers were deleted rather than ported.
- Receipts are pinned to the protocol amount rather than rendering whatever
  figure arrives, and render only when a transaction hash is present and
  `balance_before + 150 == balance_after` in exact string arithmetic. A
  "succeeded" status that fails that check is shown as unproven.
- `faucet-module/flake.nix` patches nixpkgs' cargo-vendor fetcher to send a
  descriptive User-Agent, because crates.io answers 403 to the bare tool name
  nixpkgs sends, and pins the regenerated `cargoHash` for the dependency set
  left after dropping `wallet`, `zeroize` and `tempfile`. This is the local half
  of [`logos-module-builder#159`](https://github.com/logos-co/logos-module-builder/issues/159),
  which is still open upstream. `nix build ./faucet-module#lez-faucet-ffi`
  succeeds with the patch, and both release workflows ran to completion on
  2026-08-03, so the GitHub Actions path is no longer the unproven one.
- The Basecamp catalog page, its aggregate index and its deploy script moved out
  to a separate public repository. Half of what they described was a different
  app.

### Added

- A Solve/Submit/Confirm progress rail. It is drawn whether or not a claim is
  running, so the shape of the request — including that confirmation can take
  minutes — is disclosed before the button is pressed rather than after. The
  nine core phases fold onto three lamps in `FaucetFlow.railStage` rather than
  in a QML binding, so "an unheard-of phase lights nothing" and "a proven credit
  leaves every lamp lit" are tested rules.
- Live phase reporting. A new ABI function, `lez_faucet_current_phase`, reads a
  lock-free channel keyed by operation token; it takes no lock and never touches
  the drop permit, so it is safe to call while a drop is blocked. `jobStatus`
  polls it on demand for a live drop job, snapshotting the token and releasing
  the job mutex before the call. Cancel now genuinely disappears once the claim
  reaches submitting or reconciling.
- Recipient inspection that reports a malformed ID, a valid-but-uninitialized
  account, and an account owned by another program as separate states, and shows
  the exact `wallet auth-transfer init` command for the account's owner to run,
  with a **Copy command** action.
- A cross-layer contract test. The three layers were tested separately, so
  nothing checked that the JSON one layer emits is the JSON the next can read;
  its fixtures are generated by a Rust example rather than hand-written.
- Request-key tombstones in their own never-evicted table, separate from the 128
  bounded job records. Evicting a tombstone would let a replayed key start a
  second claim; a repeated key returns its original job ID even after the record
  was acked and 200 further jobs churned through.
- A `ci.yml` workflow running the Rust, UI and core-module suites on every pull
  request, reporting the skip count rather than letting a self-skipping QML test
  read as a pass. All three existing workflows were dispatch- or schedule-only,
  so a merge was unconditionally green while four working suites went unrun.
- Before and after screenshots of the change, in `docs/screenshots/`.

### Fixed

- The reconciliation model rested on a false premise. A claim that loses the
  proof-of-work race is accepted by `sendTransaction` and then discarded when
  the block is built, so it is never included. Our transaction appearing on
  chain is therefore itself proof that it paid out, and "included but the
  balance never moved" became terminal and unattributable instead of being the
  authorization to retry. A second attempt is now authorized only by absence:
  our transaction is not on chain, the balance is unchanged, and another
  claimant has taken the challenge. That removed the one route by which a single
  press could send two claims.
- `submitted` is latched in the drop routine. Cancelling after submission could
  report "cancelled before anything was submitted", and exhausting the retries
  could say "Nothing was sent", both with a transaction already on chain.
- The job mutex is held across the worker thread spawn and its assignment. A
  worker that finished in between could be skipped by a concurrent reap, leaving
  a joinable handle for the job destructor to destroy — which calls
  `std::terminate()` and takes the host application down with it.
- The module's JSON reader was a brace-counting substring scanner. Given a
  payload whose error details merely contained `"ok":true`, it reported a failed
  operation as successful. It is now a recursive-descent parser.
- The core no longer holds a lock across an await.

### Security

- The proof-of-work search starts at an unpredictable nonce. Scanning from zero
  made two clients funding the same recipient build byte-identical transactions
  sharing one hash, which would defeat the inclusion test that success depends
  on.
- Addresses are bounded before parsing. The pinned base58 decoder subtracts with
  overflow on 133 or more leading `1` characters and validates length only after
  decoding, so an unbounded paste panics; unwinding out of `extern "C"` would
  abort the host application. Every FFI entry point also catches unwinds.
- The claim transaction hash is computed locally, before submission, so a lost
  submission response never leaves the outcome unattributable. Success requires
  observing the client's own transaction included *and* the recipient at exactly
  `balance_before + 150`; having submitted is never reported as success.
- An unresolved claim blocks any further drop to that recipient for the session,
  so a manual second click after an unknown outcome cannot become a second
  claim.

## [0.2.0] — 2026-07-23

Added funding of public accounts created elsewhere, alongside the built-in
local account. This release still owned a wallet: its first screen asked for a
"Wallet password" that the pinned wallet API accepted and ignored, and 0.3.0 is
what removed it.

### Added

- Funding an existing public account created in another LEZ wallet, including a
  node or CLI wallet, selected through a local/external mode toggle.
- A live test proving an external recipient is credited by exactly 150, and a
  guide for verifying an existing account's balance independently.

### Fixed

- Hardened the external-recipient funding path.
- Preserved exact balances read from the CLI, which are `u128` and must not be
  rounded on their way to the screen.

## [0.1.0] — 2026-07-23

First release: a wallet-owning faucet for Logos Basecamp on Apple Silicon macOS
(`darwin-arm64`). It created a wallet, derived and initialized one public
account, and then claimed 150 testnet LEZ into it.

### Added

- The asynchronous Rust faucet core, the C++ Basecamp core module
  (`lez_faucet`), and the QML view module (`lez_faucet_ui`).
- The full claim lifecycle against the pinned upstream LEZ revision: runtime
  program-ID fingerprint check, account initialization, challenge solve,
  submission, and reconciliation before any receipt.
- Portable `.lgx` packaging and the rolling catalog index that Basecamp installs
  from.

### Fixed

- Bounded the job table and preserved exact balances.
- Made wallet creation transactional, cancelled an active solve on shutdown, and
  made a failed account initialization retryable.
