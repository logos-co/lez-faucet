// SPDX-License-Identifier: MIT OR Apache-2.0

function claimsRequired(balance, target) {
    var current = Number(balance)
    var desired = Number(target)
    if (!isFinite(current) || !isFinite(desired) || current < 0 || desired < 0)
        return -1
    if (current >= desired)
        return 0
    return Math.ceil((desired - current) / 150)
}

function classifyError(message) {
    var text = String(message || "").toLowerCase()
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

function nextTargetState(balance, target, stopRequested) {
    if (stopRequested)
        return "stopped"
    return Number(balance) >= Number(target) ? "complete" : "continue"
}
