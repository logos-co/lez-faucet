import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../../src/qml/FaucetFlow.js", import.meta.url), "utf8");
const flow = {};
vm.createContext(flow);
vm.runInContext(source, flow);

test("decimal target comparisons stay exact above 2^53", () => {
  assert.equal(flow.targetPending("9007199254740992", "9007199254740993"), true);
  assert.equal(flow.targetPending("9007199254740993", "9007199254740993"), false);
  assert.equal(flow.compareDecimals("9007199254740993", "9007199254740992"), 1);
  assert.equal(flow.isU128Decimal("340282366920938463463374607431768211455", false), true);
  assert.equal(flow.isU128Decimal("340282366920938463463374607431768211456", false), false);
});

test("existing public recipients normalize without accepting private prefixes", () => {
  const account = "BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf";
  assert.equal(flow.normalizePublicAccountId(account), account);
  assert.equal(flow.normalizePublicAccountId(`  Public/${account}  `), account);
  assert.equal(flow.normalizePublicAccountId(`Private/${account}`), "");
  assert.equal(flow.normalizePublicAccountId("Public/"), "");
});

test("external claims require an exact verified target and explicit confirmation", () => {
  const account = "BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf";
  assert.equal(flow.externalRecipientVerified(`Public/${account}`, account), true);
  assert.equal(flow.externalRecipientVerified(`${account}x`, account), false);
  assert.equal(flow.canClaimForRecipient(true, account, account, false), false);
  assert.equal(flow.canClaimForRecipient(true, account, account, true), true);
  assert.equal(flow.canClaimForRecipient(true, `${account}x`, account, true), false);
  assert.equal(flow.canClaimForRecipient(false, "", "", false), true);
});

test("switching recipient modes returns to local initialization when needed", () => {
  assert.equal(flow.recipientModeScreen(true, false), "ready");
  assert.equal(flow.recipientModeScreen(false, false), "initialization_required");
  assert.equal(flow.recipientModeScreen(false, true), "ready");
});

test("existing-account mode declares its local client prerequisite without importing keys", () => {
  const qml = readFileSync(new URL("../../src/qml/FaucetView.qml", import.meta.url), "utf8");
  const backend = readFileSync(new URL("../../src/FaucetBackend.cpp", import.meta.url), "utf8");
  assert.match(qml, /local faucet wallet is used only as the LEZ network client/);
  assert.match(qml, /does not import or own the recipient key/);
  assert.match(backend, /if \(!m_clientOpen\)[\s\S]*Open or create the local faucet wallet/);
});

test("stop-after-current wins before another exact target claim", () => {
  assert.equal(flow.nextTargetState("150", "1000", true), "stopped");
  assert.equal(flow.nextTargetState("9007199254740993", "9007199254740992", false), "complete");
  assert.equal(flow.nextTargetState("9007199254740992", "9007199254740993", false), "continue");
});

test("version skew and network failures route to distinct blocking states", () => {
  assert.equal(flow.classifyError("LEZ version skew for pinata"), "version_mismatch");
  assert.equal(flow.classifyError("failed to fetch sequencer program IDs"), "offline");
  assert.equal(flow.classifyError("challenge changed too often"), "stale");
  assert.equal(flow.classifyError("invalid account"), "error");
  assert.equal(flow.classifyError("claim outcome is unknown: timed out"), "outcome_unknown");
});

test("terminal envelopes retain their last confirmed progress", () => {
  const update = flow.reduceJobEnvelope({
    status: "cancelled",
    progress: { completed_claims: 2, required_claims: 7, balance: "300" },
  }, 1, 7, "150");
  assert.equal(update.terminal, true);
  assert.equal(update.state, "cancelled");
  assert.equal(update.completedClaims, 2);
  assert.equal(update.requiredClaims, 7);
  assert.equal(update.balance, "300");
});

test("a stop is shown only after native acknowledgement", () => {
  assert.equal(flow.cancelAcknowledged({ ok: true, status: "running", cancel_requested: false }), false);
  assert.equal(flow.cancelAcknowledged({ ok: true, status: "cancelling", cancel_requested: true }), true);
  assert.equal(flow.cancelAcknowledged({ ok: true, status: "completed" }), true);
});

test("failed result acknowledgement is retried before local state is cleared", () => {
  assert.equal(flow.ackDisposition({ ok: false, error: "temporary bridge failure" }), "retry");
  assert.equal(flow.ackDisposition({ ok: true, acknowledged: true }), "clear");
});

test("generic initialization failure retries initialize without reopening the live wallet", () => {
  const firstInitialization = flow.genericRetryDecision("initialize", "initialization_required", false);
  assert.equal(firstInitialization.action, "initialize");
  assert.equal(firstInitialization.resumeState, "initialization_required");

  const resumedInitialization = flow.genericRetryDecision("initialize", "ready", false);
  assert.equal(resumedInitialization.action, "initialize");
  assert.equal(resumedInitialization.resumeState, "ready");
  assert.equal(flow.genericRetryDecision("", "welcome", false).action, "bootstrap");
  assert.equal(flow.genericRetryDecision("external_balance", "ready", false).action, "external_balance");

  const qml = readFileSync(new URL("../../src/qml/FaucetView.qml", import.meta.url), "utf8");
  assert.match(qml, /function retryFromError\(\)[\s\S]*retry\.action === "initialize"[\s\S]*initializeAccount\(retry\.resumeState\)/);
  assert.match(qml, /onClicked: root\.retryFromError\(\)/);
  assert.doesNotMatch(qml, /onClicked: root\.hasAccount \? root\.refreshBalance\(\) : root\.beginBootstrap\(\)/);
});

test("unknown claim outcome is routed from structured native error metadata", () => {
  assert.equal(flow.classifyJobError({
    code: "ffi_error",
    message: "request timed out",
    outcome: "unknown",
    tx_hash: "0xabc",
  }), "outcome_unknown");
  assert.equal(flow.classifyJobError({ message: "request timed out" }), "offline");
});

test("the remote interface never exposes mnemonic or password properties", () => {
  const rep = readFileSync(new URL("../../src/FaucetBackend.rep", import.meta.url), "utf8");
  assert.doesNotMatch(rep, /PROP\([^\n]*(mnemonic|password)/i);
  assert.match(rep, /SLOT\(QString startCreate\(QString password\)\)/);
  assert.match(rep, /SLOT\(QString jobStatus\(QString jobId\)\)/);
  assert.match(rep, /SLOT\(QString cancelJob\(QString jobId\)\)/);
  assert.match(rep, /SLOT\(QString acknowledgeJob\(QString jobId\)\)/);
  assert.match(rep, /PROP\(QString activeJobId READONLY\)/);
  assert.match(rep, /PROP\(QString activeRecipientId READONLY\)/);
  assert.match(rep, /SLOT\(QString startExternalBalance\(QString accountId\)\)/);
  assert.match(rep, /SLOT\(QString startExternalClaimOnce\(QString accountId\)\)/);
  assert.match(rep, /SLOT\(QString startExternalClaimUntilTarget\(QString accountId, QString target\)\)/);
});

test("QML reconnects jobs, explicitly acknowledges secrets, and uses valid design tokens", () => {
  const qml = readFileSync(new URL("../../src/qml/FaucetView.qml", import.meta.url), "utf8");
  assert.match(qml, /backend\.startClaimUntilTarget\(targetText\)/);
  assert.match(qml, /backend\.startExternalBalance\(normalizedRecipient\)/);
  assert.match(qml, /backend\.startExternalClaimOnce\(activeJobRecipient\)/);
  assert.match(qml, /backend\.startExternalClaimUntilTarget\(activeJobRecipient, targetText\)/);
  assert.match(qml, /enabled: root\.recipientCanClaim/);
  assert.match(qml, /I confirm this is the initialized public account I intend to fund/);
  assert.match(qml, /Fund an existing public account instead/);
  assert.match(qml, /backend\.jobStatus\(polledJobId\)/);
  assert.match(qml, /backend\.cancelJob\(activeJobId\)/);
  assert.match(qml, /backend\.acknowledgeJob\(acknowledgedJobId\)/);
  assert.match(qml, /resumeJob\(pendingJobId, pendingJobKind\)/);
  assert.match(qml, /Connection interrupted; retrying this operation/);
  assert.match(qml, /textInput\.maximumLength: 39/);
  assert.doesNotMatch(qml, /Number\(balanceText\)|claimsRequired/);
  assert.doesNotMatch(qml, /radiusMedium/);
  assert.match(qml, /Component\.onDestruction: mnemonicText = ""/);
  assert.doesNotMatch(qml, /console\.(log|warn|error)/);
});

test("account identity comes from core results and sequencer override is supported", () => {
  const backend = readFileSync(new URL("../../src/FaucetBackend.cpp", import.meta.url), "utf8");
  assert.doesNotMatch(backend, /QSettings/);
  assert.match(backend, /result\.value\(QStringLiteral\("account_id"\)\)/);
  assert.match(backend, /qEnvironmentVariable\("LEZ_FAUCET_SEQUENCER_URL"\)/);
  assert.match(backend, /m_terminalResponses\.insert\(jobId, response\)/);
  assert.match(backend, /applyProgress\(kind, envelope\);[\s\S]*if \(terminal\)/);
  assert.match(backend, /invokeCore\(QStringLiteral\("jobResultAck"\), \{jobId\}\)[\s\S]*alreadyReaped[\s\S]*if \(!succeeded\(coreEnvelope\) && !alreadyReaped\)[\s\S]*clearTerminalResponse\(jobId\)/);
  assert.match(backend, /normalized != m_verifiedExternalRecipient/);
  assert.match(backend, /kind == QStringLiteral\("external_balance"\)[\s\S]*m_verifiedExternalRecipient = m_jobRecipients\.value\(jobId\)/);
  assert.match(backend, /Open or create the local faucet wallet before funding an existing account/);
});
