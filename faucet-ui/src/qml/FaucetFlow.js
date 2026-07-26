// SPDX-License-Identifier: MIT OR Apache-2.0

var maxU128 = "340282366920938463463374607431768211455"

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

function targetPending(balance, target) {
    return isU128Decimal(balance, true) && isU128Decimal(target, false)
        && compareDecimals(balance, target) < 0
}

function normalizePublicAccountId(value) {
    var normalized = String(value === undefined || value === null ? "" : value).trim()
    if (normalized.indexOf("Public/") === 0)
        normalized = normalized.slice(7).trim()
    else if (normalized.indexOf("/") >= 0)
        return ""
    return normalized
}

function externalRecipientVerified(input, verifiedAccountId) {
    var normalized = normalizePublicAccountId(input)
    return normalized !== "" && normalized === String(verifiedAccountId || "")
}

function canClaimForRecipient(externalMode, input, verifiedAccountId, confirmed) {
    return !externalMode ||
        (externalRecipientVerified(input, verifiedAccountId) && Boolean(confirmed))
}

function recipientModeScreen(externalMode, hasLocalAccount) {
    return externalMode || hasLocalAccount ? "ready" : "initialization_required"
}

function externalPreflightRecovery(failedOperationKind, error) {
    if (String(failedOperationKind || "") !== "external_balance")
        return null
    var structured = error && typeof error === "object" ? error : {}
    var code = String(structured.code || "")
    var detail = String(structured.message || error || "Could not verify this public account")
    var lower = detail.toLowerCase()
    var actionable
    var issue = "invalid"
    var initializationCommand = ""
    if (code === "recipient_uninitialized" || lower.indexOf("uninitialized") >= 0) {
        issue = "uninitialized"
        initializationCommand = String(structured.initialization_command || "")
        actionable = detail
    } else if (code === "recipient_wrong_owner" ||
               lower.indexOf("authenticated-transfer") >= 0 ||
               lower.indexOf("program owner") >= 0) {
        issue = "wrong_owner"
        actionable = detail
    } else if (code === "invalid_public_account_id") {
        actionable = detail
    } else if (classifyError(detail) === "offline") {
        issue = "offline"
        actionable = "The faucet could not reach the LEZ network. Check your connection, then re-check this account."
    } else {
        actionable = "Could not verify this public account. Check the account ID and try again. " + detail
    }
    return {
        screen: "ready",
        clearVerification: true,
        message: actionable,
        issue: issue,
        initializationCommand: initializationCommand
    }
}

function reconnectRecipient(envelopeRecipient, backendRecipient, priorRecipient) {
    var envelope = normalizePublicAccountId(envelopeRecipient)
    if (envelope !== "")
        return envelope
    var backend = normalizePublicAccountId(backendRecipient)
    var prior = normalizePublicAccountId(priorRecipient)
    if (prior !== "" && backend !== prior)
        return prior
    return backend !== "" ? backend : prior
}

function classifyError(message) {
    var text = String(message || "").toLowerCase()
    if (text.indexOf("outcome_unknown") >= 0 ||
            text.indexOf("outcome unknown") >= 0 ||
            text.indexOf("outcome is unknown") >= 0)
        return "outcome_unknown"
    if (text.indexOf("network") >= 0 || text.indexOf("rpc") >= 0 ||
            (text.indexOf("sequencer") >= 0 && text.indexOf("failed") >= 0) ||
            text.indexOf("timed out") >= 0 || text.indexOf("failed to fetch") >= 0 ||
            text.indexOf("not available") >= 0)
        return "offline"
    if (text.indexOf("version skew") >= 0 ||
            text.indexOf("version mismatch") >= 0 ||
            text.indexOf("unexpected program") >= 0 ||
            text.indexOf("different program") >= 0 ||
            text.indexOf("program id mismatch") >= 0)
        return "version_mismatch"
    if (text.indexOf("stale") >= 0 || text.indexOf("challenge changed") >= 0)
        return "stale"
    return "error"
}

function classifyJobError(error) {
    if (error && typeof error === "object" && String(error.outcome).toLowerCase() === "unknown")
        return "outcome_unknown"
    return classifyError(error && error.message ? error.message : error)
}

function nextTargetState(balance, target, stopRequested) {
    if (stopRequested)
        return "stopped"
    return compareDecimals(balance, target) >= 0 ? "complete" : "continue"
}

function reduceJobEnvelope(payload, completedClaims, requiredClaims, balance) {
    var progress = payload && payload.progress ? payload.progress : {}
    var state = String(payload && payload.status ? payload.status : "running").toLowerCase()
    return {
        state: state,
        terminal: state === "completed" || state === "failed" || state === "cancelled",
        completedClaims: progress.completed_claims === undefined
            ? Number(completedClaims) : Number(progress.completed_claims),
        requiredClaims: progress.required_claims === undefined
            ? Number(requiredClaims) : Number(progress.required_claims),
        balance: progress.balance === undefined ? String(balance) : String(progress.balance)
    }
}

function cancelAcknowledged(envelope) {
    var state = String(envelope && envelope.status ? envelope.status : "").toLowerCase()
    return Boolean(envelope && envelope.cancel_requested) ||
        state === "cancelled" || state === "completed" || state === "failed"
}

function ackDisposition(envelope) {
    return envelope && envelope.ok === true ? "clear" : "retry"
}

function genericRetryDecision(failedOperationKind, resumeState, hasAccount) {
    var kind = String(failedOperationKind || "")
    if (kind === "initialize")
        return { action: "initialize", resumeState: String(resumeState || "initialization_required") }
    if (kind.indexOf("external_") === 0 && kind !== "external_balance")
        return { action: "external_balance", resumeState: "ready" }
    return { action: hasAccount ? "balance" : "bootstrap", resumeState: String(resumeState || "") }
}
