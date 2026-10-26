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
//   4. The prize, the unit and the shape of a claim come from the selected
//      network's row, never from a constant here. The legacy testnet pays 150
//      whole LEZ through Piñata; the v0.3 testnet pays 100,000,000 lepta
//      (0.1 LEZ) through a hosted claim service. One file has to read both.

var maxU128 = "340282366920938463463374607431768211455"

// A public account id is 32 bytes of base58 and so never exceeds 64
// characters. The bound is applied before the string is handed to the core,
// which hands it to a decoder that can be made to overflow by a long enough
// run of leading '1' characters.
var maxAccountIdLength = 64

// ---- exact decimal arithmetic ----------------------------------------------

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

// Group a decimal string into thousands so a six-figure balance can be read at
// a glance.
//
// String surgery, never arithmetic. These values are u128 in general, so the
// obvious implementation — divide, floor, format — would round the number
// before it ever reached the screen, and rule 1 of this file exists precisely
// to stop that. Anything that is not a plain run of digits is handed back
// untouched, so a malformed balance degrades to whatever the core sent rather
// than to a plausible-looking lie.
function groupDigits(value) {
    var text = String(value === undefined || value === null ? "" : value)
    if (!/^[0-9]+$/.test(text))
        return text
    var grouped = ""
    var counted = 0
    for (var index = text.length - 1; index >= 0; index -= 1) {
        grouped = text.charAt(index) + grouped
        counted += 1
        if (counted % 3 === 0 && index > 0)
            grouped = "," + grouped
    }
    return grouped
}

// How many decimal places a network's unit has, as a small non-negative
// integer, or -1 for anything unusable. A count is not a balance, but this
// file still never calls Number(): a two-digit string is read off its digits.
function decimalPlaces(decimals) {
    if (typeof decimals === "number")
        return decimals >= 0 && decimals <= 38 && Math.floor(decimals) === decimals ? decimals : -1
    var text = String(decimals === undefined || decimals === null ? "" : decimals).trim()
    if (!/^[0-9]{1,2}$/.test(text))
        return -1
    var places = text.charCodeAt(text.length - 1) - 48
    if (text.length === 2)
        places += (text.charCodeAt(0) - 48) * 10
    return places <= 38 ? places : -1
}

// Render an exact smallest-unit integer as LEZ with `decimals` places.
//
// String surgery again: the digits are split, the whole part grouped, and
// trailing zeros of the fraction dropped, so "100000000" at 9 places is "0.1"
// and "150" at 0 places is "150". Nothing is divided. Anything that is not a
// plain run of digits, or a decimals count this build cannot place, renders
// as "" rather than as a number that was never on the chain.
function formatAmount(value, decimals) {
    var text = normalizedDecimal(value)
    var places = decimalPlaces(decimals)
    if (text === "" || places < 0)
        return ""
    if (places === 0)
        return groupDigits(text)
    while (text.length <= places)
        text = "0" + text
    var whole = text.slice(0, text.length - places)
    var fraction = text.slice(text.length - places).replace(/0+$/, "")
    return fraction === "" ? groupDigits(whole) : groupDigits(whole) + "." + fraction
}

// ---- networks --------------------------------------------------------------
//
// The backend publishes its network table as JSON; one row is one enabled
// network. The row is the view's source for the prize, the unit and the claim
// mechanism, because those are what the backend configured the core for.

function networkView(row) {
    var payload = row && typeof row === "object" ? row : {}
    var mechanism = String(payload.mechanism || "")
    return {
        id: String(payload.id || ""),
        name: String(payload.name || ""),
        sequencerUrl: String(payload.sequencerUrl || ""),
        explorerUrl: String(payload.explorerUrl || ""),
        lezVersion: String(payload.lezVersion || ""),
        prize: normalizedDecimal(payload.prize),
        decimals: decimalPlaces(payload.decimals) < 0 ? 0 : decimalPlaces(payload.decimals),
        mechanism: mechanism,
        isClaimApi: mechanism === "claim_api",
        claimApiUrl: String(payload.claimApiUrl || "")
    }
}

function parseNetworks(json) {
    var rows
    try {
        rows = JSON.parse(String(json === undefined || json === null ? "[]" : json))
    } catch (parseFailure) {
        return []
    }
    if (!Array.isArray(rows))
        return []
    var views = []
    for (var index = 0; index < rows.length; index += 1) {
        var view = networkView(rows[index])
        if (view.id !== "" && view.prize !== "")
            views.push(view)
    }
    return views
}

function findNetwork(networks, id) {
    var rows = Array.isArray(networks) ? networks : []
    var wanted = String(id === undefined || id === null ? "" : id)
    for (var index = 0; index < rows.length; index += 1) {
        if (rows[index].id === wanted)
            return rows[index]
    }
    return null
}

// What one press costs and what it pays, for the row, as a person reads it.
function mechanismSentence(mechanism) {
    if (String(mechanism) === "claim_api")
        return "A hosted faucet service sends the transfer; this app holds no key and checks the credit on chain itself."
    if (String(mechanism) === "pinata")
        return "One unsigned proof-of-work claim; no key needed."
    return ""
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
//
// The core reports the same phase names on both networks, but they mean
// different work: on the legacy testnet `submitting` sends a proof-of-work
// claim this app built, on the v0.3 testnet it asks the faucet service. So the
// sentences and the rail are chosen per mechanism. "pinata" is the default so
// that a caller who does not say is read as the legacy flow.

var phaseSentences = {
    "pinata": {
        "queued": "Waiting for a free slot…",
        "validating_input": "Checking the account ID…",
        "verifying_programs": "Checking that this app matches the deployed testnet…",
        "inspecting_recipient": "Reading the recipient account…",
        "fetching_challenge": "Reading the faucet's current challenge…",
        "solving": "Solving the proof-of-work challenge. This can take a while.",
        "refreshing_challenge": "Re-checking the challenge before sending anything…",
        "submitting": "Sending the claim to the testnet…",
        "reconciling": "Confirming that the claim actually credited the account. This can take a few minutes."
    },
    "claim_api": {
        "queued": "Waiting for a free slot…",
        "validating_input": "Checking the account ID…",
        "verifying_programs": "Checking the network…",
        "inspecting_recipient": "Reading your balance…",
        "submitting": "Asking the faucet service for a drop…",
        "reconciling": "Waiting for the transfer to be included and the balance to move. This can take a few minutes."
    }
}

function mechanismKey(mechanism) {
    var key = String(mechanism === undefined || mechanism === null ? "" : mechanism)
    return phaseSentences[key] ? key : "pinata"
}

// Map a phase onto a sentence.
//
// A raw phase name must never reach the screen: `refreshing_challenge` is a
// state machine label, not something a person asked to read. The fallback is
// deliberately generic so that a phase this build has not heard of degrades to
// a sentence rather than leaking the enum.
function phaseSentence(phase, mechanism) {
    var key = String(phase === undefined || phase === null ? "" : phase)
    return phaseSentences[mechanismKey(mechanism)][key] || "Working on your request…"
}

// The stages a person is shown, and the lamp each core phase belongs to.
//
// The legacy rail is three lamps because "solve, submit, confirm" is what
// actually happens to a Piñata claim, and because the third one can take five
// minutes and must be visible before the button is pressed rather than after.
// The v0.3 rail is four: there is no proof-of-work, and the step that was
// "submit" is now a request to the faucet service, which is worth naming as
// its own thing. The core reports nine phases; folding them onto a few is a
// presentation decision and so it lives here with the sentences.
//
// An unrecognised phase maps to -1 and lights nothing. That is deliberate and
// matches phaseSentence's fallback: a phase this build has not heard of must
// degrade to an unlit rail, never to a confidently wrong lamp.
var stageRails = {
    "pinata": [
        { title: "Solve", detail: "Proof-of-work, usually seconds" },
        { title: "Submit", detail: "One unsigned claim, no key needed" },
        { title: "Confirm", detail: "Up to five minutes. Keep the app open." }
    ],
    "claim_api": [
        { title: "Checking the network", detail: "The deployed programs match this app" },
        { title: "Reading your balance", detail: "The baseline the receipt is measured from" },
        { title: "Asking the faucet service", detail: "It sends one transfer; no key leaves this app" },
        { title: "Waiting for inclusion", detail: "Up to five minutes. Keep the app open." }
    ]
}

var stageOfPhase = {
    "pinata": {
        "queued": 0,
        "validating_input": 0,
        "verifying_programs": 0,
        "inspecting_recipient": 0,
        "fetching_challenge": 0,
        "solving": 0,
        "refreshing_challenge": 0,
        "submitting": 1,
        "reconciling": 2
    },
    "claim_api": {
        "queued": 0,
        "validating_input": 0,
        "verifying_programs": 0,
        "inspecting_recipient": 1,
        "submitting": 2,
        "reconciling": 3
    }
}

function stagesFor(mechanism) {
    return stageRails[mechanismKey(mechanism)]
}

function phaseStage(phase, mechanism) {
    var key = String(phase === undefined || phase === null ? "" : phase)
    var stage = stageOfPhase[mechanismKey(mechanism)][key]
    return stage === undefined ? -1 : stage
}

// Which lamp the rail lights, for the whole life of the screen and not just the
// part where a job is polling.
//
// The rail has three stories to tell and only one of them is "a claim is
// running". Before the press it is a promise, so nothing is lit. A proven
// credit is a finished run, so everything stays lit next to the receipt: the
// end state is stated here rather than falling out of the poller stopping,
// which would darken the rail at the exact moment it has the most to say. A
// failure or an unproven outcome is not a finished run and does not earn it.
function railStage(panel, live, phase, lampCount, mechanism) {
    if (String(panel) === "receipt")
        return lampCount
    return live ? phaseStage(phase, mechanism) : -1
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
    "rate_limited": {
        title: "The faucet service is limiting claims right now",
        guidance: "It pays each account once a day and only a few claims an hour in total. Nothing was sent; try again later.",
        newAttempt: false
    },
    "faucet_paused": {
        title: "The faucet service is paused",
        guidance: "Its operator has paused payouts for now. Nothing was sent.",
        newAttempt: false
    },
    "claim_api_unavailable": {
        title: "Could not reach the faucet service",
        guidance: "Nothing was sent. Check your connection and request again.",
        newAttempt: true
    },
    "unknown_network": {
        title: "This build does not know that network",
        guidance: "Pick one of the networks listed.",
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
        initializationCommand: String(details.initialization_command || ""),
        // Seconds until the faucet service will accept another claim, as the
        // service said it; "" when it did not say.
        retryAfterSeconds: normalizedDecimal(details.retry_after_seconds)
    }
}

// ---- results ---------------------------------------------------------------

// Turn a terminal success payload into a receipt, or return null.
//
// "Submitted" is not success and "probably credited" is not success. A receipt
// is shown only when the core has handed back a transaction hash and two
// balances that differ by exactly the prize — re-checked here in exact decimal
// arithmetic, because this is the one claim the app makes that a user will act
// on. `expectedAmount` is the selected network's prize; a receipt for any other
// figure, or for an unknown prize, is not a receipt this app will show.
function receiptView(result, expectedAmount) {
    var payload = result && typeof result === "object" ? result : {}
    var prize = normalizedDecimal(expectedAmount)
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
    // The prize is fixed by the network, so a receipt claiming any other
    // figure is not a receipt this app is willing to show. Defence in depth:
    // the core already sends only its row's prize, and this is the one number
    // a user will act on.
    if (prize === "" || amount !== prize)
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
    "unsupported_difficulty": "The pool's challenge is harder than this build can solve. Update the app.",
    "faucet_paused": "The faucet service is paused by its operator. Nothing can be claimed until it resumes.",
    "prize_mismatch": "The faucet service pays a different amount than this build expects. Update the app."
}

// Summarize the pool.
//
// The pool is finite, shared and permissionless: anyone on the testnet draws
// from the same balance. `claims_remaining` is therefore a reading taken at a
// moment, not an allowance reserved for this user, and the copy says so. On a
// claim-service network the service's hot balance plays the pool's part and
// the same reading applies.
function poolView(info) {
    var payload = info && typeof info === "object" ? info : {}
    var balance = normalizedDecimal(payload.pool_balance)
    var remaining = normalizedDecimal(payload.claims_remaining)
    var reason = String(payload.blocked_reason || "")
    var mechanism = String(payload.claim_mechanism || "")
    var places = decimalPlaces(payload.decimals)
    return {
        known: balance !== "",
        network: String(payload.network || ""),
        networkName: String(payload.network_name || ""),
        lezVersion: String(payload.lez_version || ""),
        explorerUrl: String(payload.explorer_url || ""),
        mechanism: mechanism,
        isClaimApi: mechanism === "claim_api",
        claimApiUrl: String(payload.claim_api_url || ""),
        decimals: places < 0 ? 0 : places,
        paused: payload.paused === true,
        poolBalance: balance,
        claimsRemaining: remaining,
        prizeAmount: normalizedDecimal(payload.prize_amount),
        difficultyBits: normalizedDecimal(payload.effective_difficulty_bits),
        poolAddress: payload.pinata_account && typeof payload.pinata_account === "object"
            ? String(payload.pinata_account.address || "") : "",
        canClaim: payload.can_claim === true,
        blockedReason: reason,
        blockedSentence: reason === "" ? ""
            : (blockedReasons[reason] || "The faucet is not accepting claims right now.")
    }
}

// The explorer page for one transaction, or "" when either half is unusable.
//
// Only an https explorer root and a 64-hex-digit hash produce a link: this
// string is handed to the clipboard as something to open, so it must never be
// assembled out of whatever an envelope happened to carry.
function explorerTransactionUrl(explorerUrl, txHash) {
    var base = String(explorerUrl || "").trim().replace(/\/+$/, "")
    var hash = String(txHash || "").trim()
    if (!/^https:\/\/[A-Za-z0-9.-]+$/.test(base) || !/^[0-9a-f]{64}$/.test(hash))
        return ""
    return base + "/transaction/" + hash
}
