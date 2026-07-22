import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../src/qml/FaucetFlow.js", import.meta.url), "utf8");
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
});

test("the remote interface never exposes mnemonic or password properties", () => {
  const rep = readFileSync(new URL("../src/FaucetBackend.rep", import.meta.url), "utf8");
  assert.doesNotMatch(rep, /PROP\([^\n]*(mnemonic|password)/i);
  assert.match(rep, /SLOT\(QString startCreate\(QString password\)\)/);
  assert.match(rep, /SLOT\(QString jobStatus\(QString jobId\)\)/);
  assert.match(rep, /SLOT\(QString cancelJob\(QString jobId\)\)/);
});

test("QML polls jobs, cooperatively cancels target claims, and clears mnemonic", () => {
  const qml = readFileSync(new URL("../src/qml/FaucetView.qml", import.meta.url), "utf8");
  assert.match(qml, /backend\.startClaimUntilTarget\(targetText, requiredClaims\)/);
  assert.match(qml, /backend\.jobStatus\(polledJobId\)/);
  assert.match(qml, /backend\.cancelJob\(activeJobId\)/);
  assert.match(qml, /function acknowledgeMnemonic\(\) \{\s*mnemonicText = ""/);
  assert.match(qml, /Component\.onDestruction: mnemonicText = ""/);
  assert.doesNotMatch(qml, /console\.(log|warn|error)/);
});
