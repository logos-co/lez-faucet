import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../../src/qml/FaucetFlow.js", import.meta.url), "utf8");
const flow = {};
vm.createContext(flow);
vm.runInContext(source, flow);

test("claim count rounds up to the 150 LEZ prize", () => {
  assert.equal(flow.claimsRequired(0, 1000), 7);
  assert.equal(flow.claimsRequired(900, 1000), 1);
  assert.equal(flow.claimsRequired(1050, 1000), 0);
});

test("stop-after-current wins before another target claim", () => {
  assert.equal(flow.nextTargetState(150, 1000, true), "stopped");
  assert.equal(flow.nextTargetState(1050, 1000, false), "complete");
  assert.equal(flow.nextTargetState(150, 1000, false), "continue");
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
});

test("QML reconnects jobs, explicitly acknowledges secrets, and uses valid design tokens", () => {
  const qml = readFileSync(new URL("../../src/qml/FaucetView.qml", import.meta.url), "utf8");
  assert.match(qml, /backend\.startClaimUntilTarget\(targetText, requiredClaims\)/);
  assert.match(qml, /backend\.jobStatus\(polledJobId\)/);
  assert.match(qml, /backend\.cancelJob\(activeJobId\)/);
  assert.match(qml, /backend\.acknowledgeJob\(acknowledgedJobId\)/);
  assert.match(qml, /resumeJob\(pendingJobId, pendingJobKind\)/);
  assert.match(qml, /Connection interrupted; retrying this operation/);
  assert.match(qml, /textInput\.validator: IntValidator/);
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
  assert.match(backend, /invokeCore\(QStringLiteral\("jobResultAck"\), \{jobId\}\)[\s\S]*if \(!succeeded\(coreEnvelope\)\)[\s\S]*clearTerminalResponse\(jobId\)/);
});
