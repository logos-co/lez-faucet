# claimd — the LEZ v0.3 claim service

`claimd` is the server half of the faucet on the **LEZ v0.3 public testnet** (`https://testnet.lez.logos.co`,
LEZ v0.3.1). v0.3 has no Piñata program and no fee-less public transaction: every user transaction has to be
signed by a funded fee payer. So on v0.3 the Basecamp app stays keyless and asks this service for a drop; the
service signs one native transfer from a small **hot account** and pays the fee. The app still proves the
outcome on chain itself (`getTransaction` + `getAccountBalance`), exactly as it does for the legacy Piñata path.

The hot account is funded from the team treasury, which is itself fed by L1 mining on the same box. The
treasury key is never loaded by this service.

- Prize: **100,000,000 lepta = 0.1 LEZ** (1 LEZ = 10^9 lepta). Configurable (`CLAIMD_PRIZE`).
- Public URL (Tailscale Funnel in front of `127.0.0.1:8787`): `https://rog-devbox.tail957e2.ts.net:8443`.
- State: SQLite (`CLAIMD_DB`). One serial payout worker (one signer nonce).

## HTTP API (v1)

All responses are JSON. Errors use `{"error":{"code":"<snake_case>","message":"...","retryable":bool}}` with an
HTTP status that matches the code (see table). Amounts are **decimal strings** of lepta (u128).

### `GET /v1/info`

```json
{
  "network": "lez-v03-testnet",
  "sequencer_url": "https://testnet.lez.logos.co",
  "lez_version": "v0.3.1",
  "prize": "100000000",
  "decimals": 9,
  "hot_account": "Public/<base58>",
  "hot_balance": "1234567890",
  "claims_remaining": 7,
  "paused": false,
  "limits": { "per_recipient_per_day": 1, "per_ip_per_day": 2, "per_hour": 3, "per_day": 10 }
}
```

`claims_remaining` = min(global remaining today, floor((hot_balance − reserve) / prize)). It is informational
and may change between calls.

### `POST /v1/claim`

Request:
```json
{ "recipient": "Public/<base58 32-byte account id>", "request_id": "<lowercase uuid v4>" }
```

`request_id` is the idempotency key, chosen by the client. The same `request_id` with the same `recipient`
always returns the current state of that one claim and never pays twice. The same `request_id` with a different
`recipient` is `409 idempotency_conflict`.

Response `202` (new or in-flight) / `200` (already terminal):
```json
{
  "request_id": "…",
  "recipient": "Public/…",
  "status": "queued" | "submitted" | "included" | "failed",
  "prize": "100000000",
  "tx_hash": "<64 hex>" | null,
  "balance_before": "…" | null,
  "balance_after": "…" | null,
  "error": null | { "code": "…", "message": "…" },
  "created_at": "<RFC3339>",
  "updated_at": "<RFC3339>"
}
```

Status meaning:
- `queued`: accepted, waiting for the worker. `tx_hash` is null.
- `submitted`: the transfer was signed and sent; `tx_hash` is set and is the hash the app should poll with
  `getTransaction`. The service is waiting for inclusion.
- `included`: the sequencer reported the transaction in a block and `balance_after == balance_before + prize`
  was read back. Terminal.
- `failed`: terminal. `error.code` is one of `submission_rejected`, `outcome_unknown` (sent but inclusion not
  observed within the poll window — the app must reconcile on chain), `hot_balance_low`, `internal_error`.

Rejections (no claim is recorded):

| HTTP | code | when |
| --- | --- | --- |
| 400 | `invalid_public_account_id` | not `Public/` + 32-byte base58, or a `Private/` id |
| 400 | `invalid_request_key` | not a lowercase uuid |
| 409 | `idempotency_conflict` | same `request_id`, different `recipient` |
| 429 | `rate_limited` | per-recipient / per-IP / global cap hit; `details.retry_after_seconds` is set |
| 503 | `faucet_paused` | pause file present |
| 503 | `hot_balance_low` | `hot_balance < prize + fee reserve` |

### `GET /v1/claim/{request_id}`

Same body as `POST`. `404 unknown_job` if never seen.

### `GET /healthz`

`200 {"ok":true,"hot_balance":"…","paused":false,"sequencer":"ok"}`; `503` with `"ok":false` if the sequencer
is unreachable or the hot balance is below the floor.

## Policy

- One claim per recipient per 24 h. Two per client IP per 24 h (IP from `X-Forwarded-For`, first hop, when the
  request comes through Funnel; the raw peer address otherwise). Global caps: 3 per hour, 10 per day.
- Refuse new claims when `hot_balance < prize + reserve`. Reserve is `gas_limit·base_fee_exec + gas_stor·base_fee_stor`
  (`lez/programs/fee/core/src/assess.rs`), read from `getFeeState`; the wallet config's `gas_limit` sets it.
- `CLAIMD_PAUSE_FILE` (default `<state dir>/paused`): when present, `POST /v1/claim` returns `faucet_paused`.
- Idempotency ledger is durable (SQLite); a restart resumes `queued`/`submitted` rows (re-poll by `tx_hash`,
  never re-sign a row that has a `tx_hash`).

## Operations (rog)

- Binary: `~/lez-faucet/bin/claimd`, built in docker by `~/lez-faucet/build-claimd.sh` (same recipe as
  `~/lez-team/build-wallet.sh`, ubuntu:26.04, LEZ v0.3.1 crates).
- Wallet home: `~/lez-faucet/claimd-home/` (`LEE_WALLET_HOME_DIR`), its own seed; secrets in
  `~/lez-faucet/secrets/` (600, never printed).
- Service: `lez-faucet-claimd.service` (systemd, `User=danish`, `Restart=always`), logs via `journalctl -u lez-faucet-claimd`.
- Top-up: `~/lez-faucet/claimd-topup.sh` (cron hourly): when hot < 300,000,000 lepta, send from the treasury in
  `~/lez-team` up to 1,000,000,000.
- Funnel: `tailscale.exe funnel --bg --https=8443 http://127.0.0.1:8787` (Windows host; Funnel must be enabled
  for the tailnet).

## Environment

| var | default | meaning |
| --- | --- | --- |
| `CLAIMD_BIND` | `127.0.0.1:8787` | listen address |
| `CLAIMD_SEQUENCER` | `https://testnet.lez.logos.co` | LEZ sequencer JSON-RPC |
| `CLAIMD_DB` | `~/lez-faucet/state/claimd.sqlite` | SQLite path |
| `CLAIMD_PRIZE` | `100000000` | lepta per claim |
| `CLAIMD_PER_RECIPIENT_PER_DAY` / `CLAIMD_PER_IP_PER_DAY` / `CLAIMD_PER_HOUR` / `CLAIMD_PER_DAY` | `1` / `2` / `3` / `10` | caps |
| `CLAIMD_PAUSE_FILE` | `~/lez-faucet/state/paused` | pause switch |
| `LEE_WALLET_HOME_DIR` | — | hot wallet home (required) |
| `CLAIMD_POLL_SECS` | `300` | max wait for inclusion before `outcome_unknown` |

## Implementation notes (additive; the API above is the contract)

- Layout: `src/policy.rs` (validation, IP hashing, reserve formula), `src/db.rs` (SQLite ledger), `src/worker.rs`
  (the single payout task), `src/http.rs` (axum handlers), `src/chain.rs` (the `Chain` trait the rest of the
  service talks to), `src/lez.rs` (the real backend, `--features lez`). `cargo test` runs the policy / limits /
  idempotency / worker / HTTP tests against an in-memory SQLite and a mocked chain without compiling the LEZ
  crates; the host build is `cargo build --release --features lez --locked` (done by `build-claimd.sh` in docker).
- Signing happens on a dedicated thread owning the `wallet::WalletCore`; the payout is
  `NativeTokenTransfer::send_public_transfer(Public(hot), PublicNoSign(recipient), prize)` (the same path as
  `wallet auth-transfer send` between two public accounts), payer = signer = hot. Reads use a clone of the
  wallet's JSON-RPC client (`getAccountBalance`, `getTransaction`, `getFeeState`, `checkHealth`).
- The POST hot-balance check also counts claims still in flight: a new claim is refused with `hot_balance_low`
  when `hot_balance < prize · (pending + 1) + reserve`. Reserve = `gas_limit · base_fee_exec_ceiling +
  100,000 · base_fee_stor_ceiling` (next-block ceilings from `getFeeState`; genesis 8/8 → ceiling 9/9 → 18.9M lepta
  at `gas_limit` 2,000,000). Measured real fee of one public transfer on 2026-10-22: 2,968 lepta.
- Rate-limit windows count every claim that reached the chain (`status != failed` or `tx_hash` set); a claim that
  failed before signing (`hot_balance_low`, `submission_rejected`, `internal_error` without a hash) does not
  consume the recipient's or IP's quota.
- Two additive error codes not in the table above: `400 invalid_request` (malformed JSON body) and
  `503 sequencer_unavailable` (retryable; `GET /v1/info` or `POST /v1/claim` could not read the hot balance).
- `GET /healthz` adds `hot_floor` (= prize + reserve) to the documented fields.
- Extra environment: `CLAIMD_HOT_ACCOUNT` (default label `faucet-hot`; or `Public/<id>`), `CLAIMD_IP_SALT`
  (secret for the IP hash; when unset a random one is created once at `<state dir>/ip_salt`, 0600),
  `CLAIMD_POLL_INTERVAL_SECS` (default `5`), `RUST_LOG` (tracing filter, default `info`). The service refuses to
  start if `wallet_config.json` points somewhere other than `CLAIMD_SEQUENCER`.
- On rog `CLAIMD_POLL_SECS=600`: the sequencer has shown ~5-minute gaps between blocks, longer than the spec
  default of 300 s.
