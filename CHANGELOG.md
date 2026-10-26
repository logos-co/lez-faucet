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
revision (`v0.2.4-lez-legacy`, the protocol this client is built against). Each
release is
published as two GitHub releases, `lez_faucet-v<version>` and
`lez_faucet_ui-v<version>`.

## [0.5.0] — 2026-10-22

Adds the **LEZ v0.3 testnet** (`testnet.lez.logos.co`, LEZ `v0.3.1`) as a
second network, next to the legacy testnet, with a selector under the title.
v0.3 has no Piñata and no fee-less public transaction, so a keyless client
cannot claim there alone: on that network the app asks `claimd`, a small claim
service the team hosts (`https://rog-devbox.tail957e2.ts.net:8443`, contract in
`claimd/README.md`), which sends one native transfer of 100,000,000 lepta
(0.1 LEZ) from a small hot account funded from the team's mined supply. The app
stays keyless and stores nothing, and still proves every credit itself: balance
before, its transaction by hash, balance at exactly `before + prize`. Legacy
behaviour is unchanged.

### Added

- **Network row** `lez-v03-testnet` in `lez-faucet-ffi/src/network.rs`, with
  `ClaimMechanism::ClaimApi { base_url, prize }`, `decimals: 9`, and a
  `NetworkFingerprint::PpcImageId` — the v0.3 `privacy_preserving_circuit`
  ImageID, checked live before every v0.3 claim since this build compiles no
  v0.3 program. `ENABLED` lists both rows; `DEFAULT` stays legacy.
- **v0.3 transport** (`src/v03.rs`): raw JSON-RPC over `reqwest` (rustls) for
  `getAccountBalance`, `getTransaction` and `getProgramIds` — the pinned
  v0.2.4 `SequencerClient` cannot parse v0.3's `getAccount` — and the claim
  service client (`GET /v1/info`, `POST /v1/claim`, `GET /v1/claim/{id}`). The
  request key the app already uses for its own idempotency is the service's
  idempotency key too, so a replay cannot pay twice at either end.
- **v0.3 drop**: validate → verify the circuit ID (`verifying_programs`) →
  read the balance (`inspecting_recipient`; no init or owner check, `Private/`
  still refused) → `POST /v1/claim` (`submitting`) → reconcile
  (`reconciling`): poll the service for the hash, `getTransaction` and the
  balance, classified by the same `classify()` as the Piñata path with no
  challenge to consult. A lost `POST` response is resolved through the
  service's ledger before anything is concluded.
- **Error codes** `rate_limited` (`details.retry_after_seconds`),
  `faucet_paused`, and `claim_api_unavailable` (retryable: the service
  recorded nothing). The service's `hot_balance_low` maps onto `pool_depleted`.
- **`getFaucetInfo`** fields `claim_mechanism` (`pinata` | `claim_api`),
  `decimals`, `paused`, and `claim_api_url` on v0.3; `pool_balance` is the hot
  balance there. `blocked_reason` gains `faucet_paused` and `prize_mismatch`.
- **UI**: `networks` (JSON table) and `selectedNetworkId` properties and a
  `selectNetwork(id)` slot on `FaucetBackend`; a ComboBox under the title;
  amounts rendered in the row's unit (`0.1 LEZ` on v0.3, `150 LEZ` on legacy)
  through exact string formatting, never division; a four-lamp rail and its
  own phase sentences on v0.3; the initialization-command panel hidden there.
  Switching clears the pool reading, inspection and any result, and reads the
  new network afresh. Nothing persists the choice.
- **Tests**: `httpmock` tests for the v0.3 happy path, the service's word
  without the chain, rate limiting, replay, a lost `POST`, and the deadline;
  `tests/live_v03_testnet.rs` (`--ignored`, write test gated on
  `LEZ_FAUCET_LIVE_RECIPIENT`, service root override through
  `LEZ_FAUCET_CLAIM_API_URL`); a module test that reconfiguring to the second
  network is refused while a job is live and allowed when idle; a `v03` job
  in the fingerprint workflow comparing the circuit ID with `docs/testnet.md`
  and checking the service's `/v1/info`.

### Changed

- The global `PRIZE` constant is gone from the Rust API and from the C header;
  the prize is `network.prize()` and travels in `faucet_info.prize_amount`.
  `FaucetInfo.pinata_account`, `difficulty_bytes` and
  `effective_difficulty_bits` are optional and absent on v0.3;
  `program_fingerprint` is `{authenticated_transfer, pinata}` on legacy and
  `{privacy_preserving_circuit}` on v0.3. The contract fixture carries both.
- The client accepts `https://testnet.lez.logos.co` as an enabled host. It
  still refuses every other host; a `FaucetClient::for_network` constructor
  exists for tests and tooling and can only name an enabled host or loopback.
- `FaucetBackend` holds a two-row table instead of one pinned row.
- Versions: `lez-faucet-ffi` 0.5.0, both `metadata.json` 0.5.0, module
  `version()` 0.5.0, `faucet-module/flake.nix` 0.5.0 with a regenerated
  `cargoDeps` hash (the lockfile gained `reqwest` and `httpmock`).

## [0.4.0] — 2026-10-21

Moves the app to the **LEZ legacy testnet**. The main testnet
(`testnet.lez.logos.co`) upgraded to LEZ v0.3.x at the start of October, and
v0.3 ships no `pinata` or `authenticated_transfer` program, so every 0.3.x
build has refused every claim there since with "This app does not match the
deployed testnet" (issue #20). The chain it used to claim on continues as
`testnet-legacy.lez.logos.co` at `v0.2.4-lez-legacy`, with the Piñata pool at
its full 1,500,000; that is the stable testnet residents are expected to use,
and this release points at it.

### Changed

- **Target network.** Sequencer `https://testnet-legacy.lez.logos.co`,
  explorer `https://explorer.testnet-legacy.lez.logos.co`. The host pin moved
  with it: the client now refuses `testnet.lez.logos.co` outright rather than
  failing its fingerprint check against it. The prize stays 150.
- **LEZ pin** `v0.2.2` (`d6e4ae69`) → `v0.2.4-lez-legacy` (`399793b4`), in
  `Cargo.toml`, `scaffold.toml` and `faucet-module/flake.nix`. The builtin
  program ImageIDs are unchanged by this tag; what moved underneath is
  `logos-blockchain` to its `0.3.1` tag, which needs Rust 1.98
  (`rust-version` is now declared; the flake toolchain is 1.98.1),
  `logos-blockchain-circuits` v0.5.3 → v0.5.7 (new prebuilt archive hashes in
  the flake), and a transitive `rocksdb` that needs `libclang` at build time
  (`bindgenHook`).
- **Networks are a table.** `lez-faucet-ffi/src/network.rs` holds each
  network's sequencer, explorer, LEZ tag and commit, expected ImageIDs and
  claim mechanism; `FaucetBackend.cpp` mirrors the display half. One row is
  enabled. An offline test refuses a row whose ImageIDs disagree with the
  programs compiled into the build. A v0.3.x row needs a non-Piñata claim
  mechanism first; the file says what, so that the next network is an
  addition and not a rewrite.
- `getFaucetInfo` reports `network` as `lez-legacy-testnet` (was
  `lez-public-testnet`) and gains `network_name`, `lez_version` and
  `explorer_url`.

### Added

- The UI names the network it is pointed at, under the title, with the LEZ
  tag once the core has verified it.
- A **Copy explorer link** action on a receipt, built only from the pinned
  explorer root and a well-formed 64-hex transaction hash.
- `FaucetBackend.rep` gains read-only `networkName` and `explorerUrl`, which
  is why this is 0.4.0 and not 0.3.3.

### Fixed

- The daily `Testnet fingerprint` watchdog, `scripts/check-program-fingerprint.sh`
  and `scripts/lez-balance.sh` default to the legacy sequencer. Closes #20.

## [0.3.2] — 2026-08-19

Fixes a core module that stopped working for the rest of the Basecamp session
once its window was closed. Reopening the app is the whole repro.

### Fixed

- `LezFaucetModule::shutdown()` now clears `m_stopping` before returning.
  `joinAllWorkers()` sets that flag so no new job can enter the FFI while the
  client handle is being freed, and nothing in the class ever cleared it again.
  That is correct for `~LezFaucetModule()`, where stopping is terminal, but
  Basecamp calls `shutdown` when the **UI window closes** and then keeps the
  same core module instance loaded, calling `configure` again when the user
  reopens the app. The second `configure` hit the latched flag and answered
  `module_stopping`, so the faucet showed "This app is shutting down" for the
  remaining lifetime of the host process, curable only by quitting Basecamp.
  The UI's guidance for that code — "Reopen it to make another request" — was
  the one action that could not work.

  The clear is placed after the client is destroyed rather than in
  `configure()`: the flag is cleared by the function that set it, and by that
  point every worker is joined and `m_client` is null, so a `startJob` racing
  in behind it finds no client and is refused with `not_configured`. It cannot
  reach a freed handle, which is what API_LOCK §A1.7.1 protects. Calls made
  between a `shutdown` and the next `configure` therefore now report
  `not_configured` rather than `module_stopping` — the module is quiescent and
  reconfigurable, not dying.

  Present in 0.3.0 and 0.3.1, and independent of the LEZ pin: this is a
  lifecycle bug, not a protocol one. It was invisible to the suite because
  `shutdownJoinsWorkersBeforeDestroyingTheHandle` asserted the latched
  behaviour as intended, modelling `shutdown` as terminal the way the
  destructor is. `shutdownIsReversibleSoReopeningTheAppWorks` now covers the
  close/reopen cycle Basecamp actually performs, and fails on the 0.3.1 source.

- `shutdown`'s `already_stopping` field consequently reports whether another
  shutdown is *in flight*, not whether one has ever run. Nothing consumes it;
  the test suite pins the new meaning.

### Changed

- `docs/community-install.md` tells users to restart Basecamp after installing
  a new version. Replacing a core module's shared library on disk does not
  affect the copy the running process already loaded, so an upgrade taken
  without a restart leaves the old build serving every call — including the
  build a user upgraded specifically to escape. The network-mismatch entry
  previously sent that user to open an issue instead, which is the wrong
  advice for the most likely cause.

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

Verified on chain, not only by fingerprint: claim
`4581848e26271a512ec2dfacbdfad568b1dede31d96821685536958006968b88` credited a
freshly initialized account from 0 to 150 and moved the pool from 1498800 to
1498650. The same test replays its request key afterwards and gets the original
receipt without a second credit, so idempotency holds on the upgraded chain too.

### Added

- A `Testnet fingerprint` workflow that runs daily. Its gate compares the
  ImageIDs *compiled into the build* against the sequencer, via the existing
  `faucet_info_matches_the_pinned_protocol` live test — the same comparison the
  runtime gate makes. `scripts/check-program-fingerprint.sh` runs alongside it
  as a fast advisory diagnostic, comparing the sequencer against the table in
  `docs/testnet.md`; it needs no toolchain and names the drift precisely, but it
  cannot be the gate, because editing that table would silence it while leaving
  every build unable to claim. A real mismatch opens or updates an issue, since
  a scheduled failure reddens no pull request and would otherwise notify nobody.

### Fixed

- `classify`'s correctness argument in `client.rs` cited sequencer behaviour
  that v0.2.2 changed. That release added a two-tier chain state with peer block
  adoption and orphaning, reads answer from the reorg-able head, and the hash
  index behind `get_transaction` is never pruned when a block is orphaned. So a
  transaction can read as included after its block was reverted. The decision
  table already refused rather than guessed in that case — it returns
  `Unattributable` and submits nothing further — but the comment called it
  unreachable, and the stale upstream path it cited
  (`core/src/program.rs:464`) had moved to `core/src/program/mod.rs:470`.
  Documented rather than redesigned: reads against a finalized tier would need
  RPC the sequencer does not expose.
- `docs/testnet.md`, `README.md` and `docs/community-install.md` told users to
  point the pinned wallet CLI at an existing wallet home. v0.2.2 replaced
  `WalletConfig`'s `sequencer_addr` with a `sequencers` list and added
  `calibration_limit`, with no serde alias, default or migration, so a home
  created before the upgrade fails to parse. Now called out, along with two
  other traps found while creating a test account: the upstream flake's `wallet`
  output is the FFI shared library rather than the CLI, and
  `auth-transfer init` exits non-zero even when it succeeds.
- Three stale `v0.2.0` references in `scripts/scaffold-setup.sh`, one of which
  is printed at runtime. The layout assumption behind those symlinks is still
  correct at v0.2.2.
- Rollback prose in `docs/community-install.md` and `docs/releasing.md` listed
  only 0.2.0 and 0.1.0; the index is rebuilt from every non-draft release, so
  0.3.0 is listed too.

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
