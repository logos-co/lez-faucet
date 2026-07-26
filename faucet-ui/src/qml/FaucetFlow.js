// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Pure presentation logic for the LEZ Faucet view.
//
// Everything here is a total function of its arguments so it can be exercised
// without a QML engine. Three rules shape the whole file:
//
//   1. Balances are exact decimal strings from the chain to the screen. They
//      are compared and added as strings. A u128 does not survive a JavaScript
//      Number, and a rounded balance in a receipt is a false receipt.
//   2. Errors are dispatched on their structured `code` and never on the text
//      of their message. A sentence is written for a person; a code is what a
//      program is allowed to branch on.
//   3. A request key is minted in exactly one place, `beginCreditAttempt`, and
//      every re-send reuses it.

var maxU128 = "340282366920938463463374607431768211455"

// The protocol fixes the prize; the view never asks for an amount.
var prizeAmount = "150"

// A public account id is 32 bytes of base58 and so never exceeds 64
// characters. The bound is applied before the string is handed to the core,
// which hands it to a decoder that can be made to overflow by a long enough
// run of leading '1' characters.
var maxAccountIdLength = 64

// ---- exact decimal arithmetic ----------------------------------------------

// The exact prize the deployed Piñata guest pays per claim.
var PRIZE_AMOUNT = "150"

function normalizedDecimal(value) {
    var text = String(value === undefined || value === null ? "" : value)
    if (!/^[0-9]+$/.test(text))
        return ""
    var normalized = text.replace(/^0+/, "")
    return normalized === "" ? "0" : normalized
}

function isU128Decimal(value, allowZero) {
    var normalized = normalizedDecimal(value)
    if (normalized === "" || (!allowZero && normalized === "0"))
        return false
    return normalized.length < maxU128.length ||
        (normalized.length === maxU128.length && normalized <= maxU128)
}

function compareDecimals(left, right) {
    var a = normalizedDecimal(left)
    var b = normalizedDecimal(right)
    if (a === "" || b === "")
        return undefined
    if (a.length !== b.length)
        return a.length < b.length ? -1 : 1
    if (a === b)
        return 0
    return a < b ? -1 : 1
}

// Schoolbook addition on decimal strings.
//
// This exists so the view can check for itself that a receipt's balances
// really differ by the prize. Doing that with Number() would silently agree
// with any two balances above 2^53.
function addDecimals(left, right) {
    var a = normalizedDecimal(left)
    var b = normalizedDecimal(right)
    if (a === "" || b === "")
        return ""
    var carry = 0
    var sum = ""
    var i = a.length - 1
    var j = b.length - 1
    while (i >= 0 || j >= 0 || carry > 0) {
        var digit = carry
        if (i >= 0) { digit += a.charCodeAt(i) - 48; i -= 1 }
        if (j >= 0) { digit += b.charCodeAt(j) - 48; j -= 1 }
        carry = digit > 9 ? 1 : 0
        sum = String(digit % 10) + sum
    }
    return sum === "" ? "0" : sum
}

// ---- address input ---------------------------------------------------------

function normalizePublicAccountId(value) {
    var normalized = String(value === undefined || value === null ? "" : value).trim()
    if (normalized.indexOf("Public/") === 0)
        normalized = normalized.slice(7).trim()
    else if (normalized.indexOf("/") >= 0)
        return "" // An explicit prefix check, so Private/... is refused by design.
    return normalized
}

// Local plausibility only. The core re-validates and is authoritative; this
// just avoids sending obvious nonsense and enforces the length bound early.
function isPlausibleAddressInput(value) {
    var normalized = normalizePublicAccountId(value)
    if (normalized === "" || normalized.length > maxAccountIdLength)
        return false
    return /^[1-9A-HJ-NP-Za-km-z]+$/.test(normalized)
}

// ---- request keys ----------------------------------------------------------

// Mirrors the core's validator: a lowercase RFC 4122 UUID, 36 characters.
function isRequestKey(value) {
    return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/
        .test(String(value === undefined || value === null ? "" : value))
}

// Mint one version-4 UUID.
//
// The key is an idempotency handle inside one process, not a secret: it is
// never transmitted to the chain and confers no authority. It only has to be
// unique among the handful of requests one session makes, which 122 bits from
// the engine's generator comfortably is.
function newRequestKey(random) {
    var next = typeof random === "function" ? random : Math.random
    var digits = "0123456789abcdef"
    var key = ""
    for (var index = 0; index < 36; index += 1) {
        if (index === 8 || index === 13 || index === 18 || index === 23) {
            key += "-"
        } else if (index === 14) {
            key += "4" // version
        } else {
            var nibble = Math.floor(next() * 16) % 16
            if (nibble < 0)
                nibble = 0
            if (index === 19)
                nibble = (nibble & 0x3) | 0x8 // variant
            key += digits.charAt(nibble)
        }
    }
    return key
}

// ---- the credit attempt ----------------------------------------------------
//
// One button press is one attempt, and one attempt is one request key.
//
// This is the single most consequential rule in the view, so the key lives in
// a value object with exactly one constructor. `beginCreditAttempt` is the
// only function in this file that mints a key, and `resendCreditAttempt` takes
// no source of randomness at all, so there is no code path from a re-send to a
// second key.
//
// Why it matters: the core's idempotency is keyed on (requestKey, address). A
// re-send with the stored key returns the *original* job — no second
// transaction, whatever happened to the first call. A re-send with a fresh key
// is a brand new request, and if the first one is already solving or
// submitting, the recipient is credited twice from one click.
//
// The trap is that every reason to re-send looks like a reason to start over.
// A bridge error, a reply with no job id, a reloaded view, a resumed session:
// in each case the view has lost sight of the request, and in none of them has
// it learned that the request did not happen. Absence of an answer is not
// evidence of absence of an effect.
function beginCreditAttempt(address, mint) {
    var minted = typeof mint === "function" ? mint() : newRequestKey()
    return {
        address: String(address === undefined || address === null ? "" : address),
        requestKey: minted,
        sends: 1
    }
}

// Re-send the same attempt. Deliberately takes no mint function.
function resendCreditAttempt(attempt) {
    var previous = attempt && typeof attempt === "object" ? attempt : {}
    return {
        address: String(previous.address || ""),
        requestKey: String(previous.requestKey || ""),
        sends: (previous.sends || 0) + 1
    }
}

// Rejoin the attempt a live job already represents, from the job envelope
// itself. Used after a view reload: the key comes back from the core rather
// than being invented again.
function rejoinCreditAttempt(envelope) {
    var payload = envelope && typeof envelope === "object" ? envelope : {}
    var key = String(payload.request_key || "")
    if (!isRequestKey(key))
        return null
    return { address: "", requestKey: key, sends: 1 }
}

// ---- job envelopes ---------------------------------------------------------

function jobOutcome(envelope) {
    var payload = envelope && typeof envelope === "object" ? envelope : {}
    var status = String(payload.status || "").toLowerCase()
    var error = payload.error && typeof payload.error === "object" ? payload.error : {}
    var code = String(error.code || "")
    if (status === "succeeded")
        return "succeeded"
    if (status === "cancelled")
        return "cancelled"
    // An unproven outcome is its own terminal state whether the core reports it
    // as a status or as the error code of a failure.
    if (status === "outcome_unknown" || code === "outcome_unknown")
        return "outcome_unknown"
    if (status === "failed")
        return "failed"
    return "running"
}

function isTerminalOutcome(outcome) {
    return String(outcome) !== "running"
}

// Cancelling is free only until the transaction is sent. Afterwards the chain
// action cannot be taken back, only reconciled, and offering a button that
// implies otherwise would be a lie.
function cancelAvailable(status, phase, cancelRequested) {
    if (cancelRequested)
        return false
    var state = String(status || "").toLowerCase()
    if (state !== "queued" && state !== "running")
        return false
    var current = String(phase || "")
    return current !== "submitting" && current !== "reconciling"
}

// ---- phases ----------------------------------------------------------------

var phaseSentences = {
    "queued": "Waiting for a free slot…",
    "validating_input": "Checking the account ID…",
    "verifying_programs": "Checking that this app matches the deployed testnet…",
    "inspecting_recipient": "Reading the recipient account…",
    "fetching_challenge": "Reading the faucet's current challenge…",
    "solving": "Solving the proof-of-work challenge. This can take a while.",
    "refreshing_challenge": "Re-checking the challenge before sending anything…",
    "submitting": "Sending the claim to the testnet…",
    "reconciling": "Confirming that the claim actually credited the account…"
}

// Map a phase onto a sentence.
//
// A raw phase name must never reach the screen: `refreshing_challenge` is a
// state machine label, not something a person asked to read. The fallback is
// deliberately generic so that a phase this build has not heard of degrades to
// a sentence rather than leaking the enum.
function phaseSentence(phase) {
    var key = String(phase === undefined || phase === null ? "" : phase)
    return phaseSentences[key] || "Working on your request…"
}

// ---- errors ----------------------------------------------------------------
//
// One entry per stable code. `newAttempt` says whether offering the user a
// fresh attempt (a new button press, and therefore a new request key) is
// honest: it is true only where the core guarantees nothing was sent and a
// repeat could plausibly behave differently.
var errorPresentations = {
    "not_configured": {
        title: "The faucet has not connected yet",
        guidance: "Reopen the faucet to connect to the LEZ testnet.",
        newAttempt: false
    },
    "invalid_sequencer_url": {
        title: "This build cannot reach its testnet",
        guidance: "The faucet talks to one pinned LEZ testnet host and this build could not open it.",
        newAttempt: false
    },
    "invalid_public_account_id": {
        title: "That is not a public LEZ account ID",
        guidance: "Enter the bare Base58 account ID, or Public/<account ID>. Private accounts cannot receive a faucet claim.",
        newAttempt: false
    },
    "invalid_request_key": {
        title: "The request could not be identified",
        guidance: "Close and reopen the faucet, then request again.",
        newAttempt: false
    },
    "recipient_uninitialized": {
        title: "This account has not been initialized",
        guidance: "The owner of the account has to initialize it once before it can receive anything. The faucet never needs any secret of theirs to do that.",
        newAttempt: false
    },
    "recipient_wrong_owner": {
        title: "This account is managed by another program",
        guidance: "The faucet only credits accounts owned by the authenticated-transfer program.",
        newAttempt: false
    },
    "recipient_unreconciled": {
        title: "An earlier claim to this account was never confirmed",
        guidance: "This session will not send another claim to it. Check the account's balance yourself first. Restarting the app clears this refusal, so only restart once you know whether the earlier claim landed.",
        newAttempt: false
    },
    "program_fingerprint_mismatch": {
        title: "This app does not match the deployed testnet",
        guidance: "The testnet is running different programs than this build was made for. Update the app.",
        newAttempt: false
    },
    "pinata_wrong_owner": {
        title: "The pool account is not what this app expects",
        guidance: "Nothing was sent. Update the app.",
        newAttempt: false
    },
    "invalid_challenge": {
        title: "The faucet's challenge could not be read",
        guidance: "Refresh the pool status in a moment.",
        newAttempt: true
    },
    "unsupported_difficulty": {
        title: "The challenge is harder than this build can solve",
        guidance: "Update the app.",
        newAttempt: false
    },
    "pool_depleted": {
        title: "The pool is empty right now",
        guidance: "The pool is a finite shared resource. Nothing more can be claimed from it until it is refilled.",
        newAttempt: false
    },
    "recipient_balance_overflow": {
        title: "This account cannot accept the prize",
        guidance: "Its balance is already at the maximum the chain can represent.",
        newAttempt: false
    },
    "network_unavailable": {
        title: "Could not reach the LEZ testnet",
        guidance: "Check your connection. Nothing was sent.",
        newAttempt: true
    },
    "rpc_timeout": {
        title: "The LEZ testnet did not answer in time",
        guidance: "Nothing was confirmed. You can request again.",
        newAttempt: true
    },
    "solve_timeout": {
        title: "The proof-of-work took too long",
        guidance: "Nothing was sent. Everyone races the same challenge, so a busy pool makes this more likely.",
        newAttempt: true
    },
    "solve_attempt_limit": {
        title: "The proof-of-work ran out of attempts",
        guidance: "Nothing was sent. You can request again.",
        newAttempt: true
    },
    "stale_challenge_exhausted": {
        title: "Other claimants kept winning the challenge",
        guidance: "Nothing was sent. Everyone races one shared challenge; request again in a moment.",
        newAttempt: true
    },
    "submission_rejected": {
        title: "The testnet refused the claim",
        guidance: "The sequencer rejected the transaction outright, so nothing was sent and nothing is pending.",
        newAttempt: true
    },
    "cancelled": {
        title: "Stopped before anything was sent",
        guidance: "No transaction was submitted.",
        newAttempt: true
    },
    "drop_in_progress": {
        title: "A request is already running",
        guidance: "Wait for it to finish before starting another.",
        newAttempt: false
    },
    "idempotency_conflict": {
        title: "That request identifier belongs to another account",
        guidance: "Close and reopen the faucet, then request again.",
        newAttempt: false
    },
    "job_limit_reached": {
        title: "This session has handled too many requests",
        guidance: "Restart the app.",
        newAttempt: false
    },
    "unknown_job": {
        title: "The faucet lost track of this request",
        guidance: "Check the account's balance before requesting again.",
        newAttempt: false
    },
    "job_not_terminal": {
        title: "That request has not finished",
        guidance: "Wait for it to finish.",
        newAttempt: false
    },
    "module_stopping": {
        title: "The faucet is shutting down",
        guidance: "Reopen it to make another request.",
        newAttempt: false
    },
    "internal_error": {
        title: "Something went wrong inside the faucet",
        guidance: "Check the account's balance before requesting again.",
        newAttempt: false
    }
}

// Present a structured error.
//
// The only field consulted for a decision is `code`. Matching on message text
// is how a rewording turns into a behaviour change, and how an attacker-shaped
// or merely unlucky sentence gets to choose which branch the app takes.
// An unrecognised code falls through to a generic presentation that offers no
// retry: failing closed is the only safe default when we do not know what
// happened.
function errorPresentation(error) {
    var structured = error && typeof error === "object" ? error : {}
    var code = String(structured.code || "")
    var known = errorPresentations[code]
    var details = structured.details && typeof structured.details === "object"
        ? structured.details : {}
    return {
        code: code,
        title: known ? known.title : "The request could not be completed",
        guidance: known ? known.guidance
            : "Check the account's balance before requesting again.",
        // Written by the core for a person, and safe to show as-is. It is
        // never used to decide anything.
        message: String(structured.message || ""),
        newAttempt: known ? known.newAttempt : false,
        recognized: Boolean(known),
        initializationCommand: String(details.initialization_command || "")
    }
}

// ---- results ---------------------------------------------------------------

// Turn a terminal success payload into a receipt, or return null.
//
// "Submitted" is not success and "probably credited" is not success. A receipt
// is shown only when the core has handed back a transaction hash and two
// balances that differ by exactly the prize — re-checked here in exact decimal
// arithmetic, because this is the one claim the app makes that a user will act
// on.
function receiptView(result) {
    var payload = result && typeof result === "object" ? result : {}
    var recipient = payload.recipient && typeof payload.recipient === "object"
        ? payload.recipient : {}
    var address = String(recipient.address || "")
    var accountId = String(recipient.account_id || "")
    var amount = normalizedDecimal(payload.amount)
    var before = normalizedDecimal(payload.balance_before)
    var after = normalizedDecimal(payload.balance_after)
    var txHash = String(payload.tx_hash || "").trim()

    if (address === "" || amount === "" || before === "" || after === "")
        return null
    if (txHash === "" || txHash.length > 128 || /\s/.test(txHash))
        return null
    if (addDecimals(before, amount) !== after)
        return null
    // The prize is fixed by the protocol, so a receipt claiming any other
    // figure is not a receipt this app is willing to show. Defence in depth:
    // the core already sends only the constant, and this is the one number a
    // user will act on.
    if (amount !== PRIZE_AMOUNT)
        return null

    var retries = normalizedDecimal(payload.stale_challenge_retries)
    return {
        address: address,
        accountId: accountId,
        amount: amount,
        balanceBefore: before,
        balanceAfter: after,
        txHash: txHash,
        retried: retries !== "" && retries !== "0"
    }
}

// Everything the unproven-outcome panel is allowed to show.
//
// `pinnedAddress` is the canonical address the core returned for this
// recipient earlier in the session. The raw input is never substituted for it:
// what the user typed is not what the chain saw.
function unknownOutcomeView(error, pinnedAddress) {
    var structured = error && typeof error === "object" ? error : {}
    var details = structured.details && typeof structured.details === "object"
        ? structured.details : {}
    var recipient = details.recipient && typeof details.recipient === "object"
        ? details.recipient : {}
    return {
        address: String(recipient.address || pinnedAddress || ""),
        balanceBefore: normalizedDecimal(details.balance_before),
        txHash: String(details.tx_hash || "").trim(),
        message: String(structured.message || "")
    }
}

// ---- inspection ------------------------------------------------------------

function inspectionView(result) {
    var payload = result && typeof result === "object" ? result : {}
    var recipient = payload.recipient && typeof payload.recipient === "object"
        ? payload.recipient : {}
    var eligibility = String(payload.eligibility || "")
    var summaries = {
        "eligible": "This account can receive a faucet claim.",
        "uninitialized": "This account has not been initialized yet, so it cannot receive anything. Its owner runs the command below once; the faucet never needs any secret of theirs.",
        "wrong_owner": "This account is managed by another program, so the faucet cannot credit it."
    }
    return {
        address: String(recipient.address || ""),
        accountId: String(recipient.account_id || ""),
        eligibility: eligibility,
        eligible: eligibility === "eligible",
        balance: normalizedDecimal(payload.balance),
        summary: summaries[eligibility] || "This account could not be classified.",
        initializationCommand: String(payload.initialization_command || "")
    }
}

// ---- pool ------------------------------------------------------------------

var blockedReasons = {
    "pool_depleted": "The pool is empty. Nothing can be claimed until someone refills it.",
    "unsupported_difficulty": "The pool's challenge is harder than this build can solve. Update the app."
}

// Summarize the pool.
//
// The pool is finite, shared and permissionless: anyone on the testnet draws
// from the same balance. `claims_remaining` is therefore a reading taken at a
// moment, not an allowance reserved for this user, and the copy says so.
function poolView(info) {
    var payload = info && typeof info === "object" ? info : {}
    var balance = normalizedDecimal(payload.pool_balance)
    var remaining = normalizedDecimal(payload.claims_remaining)
    var reason = String(payload.blocked_reason || "")
    return {
        known: balance !== "",
        network: String(payload.network || ""),
        poolBalance: balance,
        claimsRemaining: remaining,
        prizeAmount: normalizedDecimal(payload.prize_amount) || prizeAmount,
        difficultyBytes: normalizedDecimal(payload.difficulty_bytes),
        difficultyBits: normalizedDecimal(payload.effective_difficulty_bits),
        poolAddress: payload.pinata_account && typeof payload.pinata_account === "object"
            ? String(payload.pinata_account.address || "") : "",
        canClaim: payload.can_claim === true,
        blockedReason: reason,
        blockedSentence: reason === "" ? ""
            : (blockedReasons[reason] || "The faucet is not accepting claims right now.")
    }
}
