import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import vm from "node:vm";

const sourceRoot = new URL("../../src/", import.meta.url);
const source = readFileSync(new URL("qml/FaucetFlow.js", sourceRoot), "utf8");
const flow = {};
vm.createContext(flow);
vm.runInContext(source, flow);

const qml = readFileSync(new URL("qml/FaucetView.qml", sourceRoot), "utf8");
const rep = readFileSync(new URL("FaucetBackend.rep", sourceRoot), "utf8");
const backend = readFileSync(new URL("FaucetBackend.cpp", sourceRoot), "utf8");
const backendHeader = readFileSync(new URL("FaucetBackend.h", sourceRoot), "utf8");

// Comments in these files talk *about* Number() and parseInt on purpose; the
// prohibition is on calling them.
function withoutComments(text) {
  return text
    .split("\n")
    .filter((line) => !/^\s*\/\//.test(line))
    .map((line) => line.replace(/\s+\/\/.*$/, ""))
    .join("\n");
}

function everySourceFile(directory) {
  return readdirSync(directory).flatMap((entry) => {
    const path = join(directory, entry);
    return statSync(path).isDirectory() ? everySourceFile(path) : [path];
  });
}

// Every phase the core can report, taken from lez-faucet-ffi/src/error.rs.
const dropPhases = [
  "validating_input",
  "verifying_programs",
  "inspecting_recipient",
  "fetching_challenge",
  "solving",
  "refreshing_challenge",
  "submitting",
  "reconciling",
];

// ---------------------------------------------------------------------------
// exact decimal arithmetic
// ---------------------------------------------------------------------------

test("decimal comparisons stay exact above 2^53", () => {
  assert.equal(flow.compareDecimals("9007199254740993", "9007199254740992"), 1);
  assert.equal(flow.compareDecimals("9007199254740992", "9007199254740993"), -1);
  assert.equal(flow.compareDecimals("9007199254740993", "9007199254740993"), 0);
  assert.equal(flow.isU128Decimal("340282366920938463463374607431768211455", false), true);
  assert.equal(flow.isU128Decimal("340282366920938463463374607431768211456", false), false);
  assert.equal(flow.isU128Decimal("0", true), true);
  assert.equal(flow.isU128Decimal("0", false), false);
});

test("decimal addition is exact where Number would round", () => {
  assert.equal(flow.addDecimals("9007199254740992", "150"), "9007199254741142");
  assert.equal(flow.addDecimals("340282366920938463463374607431768211305", "150"),
    "340282366920938463463374607431768211455");
  assert.equal(flow.addDecimals("0", "150"), "150");
  assert.equal(flow.addDecimals("999", "1"), "1000");
  assert.equal(flow.addDecimals("abc", "1"), "");
  // The identity Number() silently gets wrong.
  assert.notEqual(String(Number("9007199254740993") + 150), flow.addDecimals("9007199254740993", "150"));
});

test("grouping a balance for reading never changes its value", () => {
  // The separators are inserted by walking the string, so the digits that come
  // out are exactly the digits that went in. A divide-and-format
  // implementation would pass the first few of these and silently corrupt the
  // rest.
  assert.equal(flow.groupDigits("1476750"), "1,476,750");
  assert.equal(flow.groupDigits("150"), "150");
  assert.equal(flow.groupDigits("6000"), "6,000");
  assert.equal(flow.groupDigits("0"), "0");
  assert.equal(flow.groupDigits("1"), "1");
  assert.equal(flow.groupDigits("999"), "999");
  assert.equal(flow.groupDigits("1000"), "1,000");

  // Above 2^53 a Number would round; every digit must survive untouched.
  assert.equal(flow.groupDigits("9007199254740993"), "9,007,199,254,740,993");
  assert.equal(
    flow.groupDigits("340282366920938463463374607431768211455"),
    "340,282,366,920,938,463,463,374,607,431,768,211,455",
  );
  for (const value of ["9007199254740993", "340282366920938463463374607431768211455"]) {
    assert.equal(flow.groupDigits(value).split(",").join(""), value);
  }

  // Anything that is not a plain run of digits comes back as it arrived, so a
  // malformed balance degrades to whatever the core sent rather than to a
  // plausible-looking number.
  assert.equal(flow.groupDigits(""), "");
  assert.equal(flow.groupDigits("12.5"), "12.5");
  assert.equal(flow.groupDigits("-150"), "-150");
  assert.equal(flow.groupDigits("not a balance"), "not a balance");
  assert.equal(flow.groupDigits(null), "");
  assert.equal(flow.groupDigits(undefined), "");
});

// ---------------------------------------------------------------------------
// address input
// ---------------------------------------------------------------------------

test("addresses normalize without accepting private prefixes or unbounded input", () => {
  const account = "BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf";
  assert.equal(flow.normalizePublicAccountId(account), account);
  assert.equal(flow.normalizePublicAccountId(`  Public/${account}  `), account);
  assert.equal(flow.normalizePublicAccountId(`Private/${account}`), "");
  assert.equal(flow.normalizePublicAccountId("Public/"), "");

  assert.equal(flow.isPlausibleAddressInput(account), true);
  assert.equal(flow.isPlausibleAddressInput(`Public/${account}`), true);
  assert.equal(flow.isPlausibleAddressInput(`Private/${account}`), false);
  assert.equal(flow.isPlausibleAddressInput(""), false);
  // The pinned base58 decoder underflows on a long run of leading '1's, so the
  // length bound has to be applied before anything parses it.
  assert.equal(flow.isPlausibleAddressInput("1".repeat(133)), false);
  assert.equal(flow.isPlausibleAddressInput("1".repeat(64)), true);
  assert.equal(flow.isPlausibleAddressInput("1".repeat(65)), false);
  assert.equal(flow.isPlausibleAddressInput(`${account}\u0000`), false);
  assert.equal(flow.isPlausibleAddressInput("Brrhdd Voucvgk"), false);
  // Surrounding whitespace is trimmed rather than rejected, because pasting an
  // address routinely picks it up.
  assert.equal(flow.isPlausibleAddressInput(`  ${account}  `), true);
  assert.equal(flow.isPlausibleAddressInput("0OIl"), false);
});

// ---------------------------------------------------------------------------
// one request key per click, reused by every retry
// ---------------------------------------------------------------------------

test("minted request keys satisfy the core's validator", () => {
  for (let index = 0; index < 200; index += 1)
    assert.equal(flow.isRequestKey(flow.newRequestKey()), true);
  // Version and variant nibbles are fixed even for a degenerate generator.
  assert.equal(flow.isRequestKey(flow.newRequestKey(() => 0)), true);
  assert.equal(flow.isRequestKey(flow.newRequestKey(() => 0.9999999)), true);
  assert.equal(flow.newRequestKey(() => 0).charAt(14), "4");
  assert.equal("89ab".indexOf(flow.newRequestKey(() => 0).charAt(19)) >= 0, true);
  assert.equal(flow.isRequestKey("3F2504E0-4F89-41D3-9A0C-0305E82C3301"), false);
  assert.equal(flow.isRequestKey("not-a-uuid"), false);
});

test("a retry reuses the stored request key and never mints a second one", () => {
  const account = "BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf";
  let mints = 0;
  const mint = () => {
    mints += 1;
    return flow.newRequestKey();
  };

  const first = flow.beginCreditAttempt(account, mint);
  assert.equal(mints, 1);
  assert.equal(flow.isRequestKey(first.requestKey), true);

  // Every reason to re-send — a bridge error, a reply with no job id, a view
  // reload, a resumed session — goes through here. None of them may mint.
  let attempt = first;
  for (let retry = 0; retry < 5; retry += 1) {
    attempt = flow.resendCreditAttempt(attempt);
    assert.equal(attempt.requestKey, first.requestKey);
    assert.equal(attempt.address, first.address);
  }
  assert.equal(mints, 1, "a retry must never mint a request key");
  assert.equal(attempt.sends, 6);

  // A genuinely new press of the button is a new attempt and a new key.
  const second = flow.beginCreditAttempt(account, mint);
  assert.equal(mints, 2);
  assert.notEqual(second.requestKey, first.requestKey);
});

test("resendCreditAttempt has no way to reach a random source", () => {
  const body = String(flow.resendCreditAttempt);
  assert.doesNotMatch(body, /newRequestKey|Math\.random|mint/);
  // beginCreditAttempt is the single mint site in the whole file.
  assert.equal(source.split("newRequestKey(").length - 1 >= 1, true);
  assert.doesNotMatch(
    source.replace(/function newRequestKey[\s\S]*?\n}\n/, ""),
    /Math\.random/,
  );
});

test("a resumed job recovers its original key from the envelope", () => {
  const key = flow.newRequestKey();
  const rejoined = flow.rejoinCreditAttempt({ ok: true, job_id: "7", request_key: key });
  assert.equal(rejoined.requestKey, key);
  // No key in the envelope means no attempt to resume — never an invented one.
  assert.equal(flow.rejoinCreditAttempt({ ok: true, job_id: "7" }), null);
  assert.equal(flow.rejoinCreditAttempt({ request_key: "nope" }), null);
});

test("the view mints exactly once, in the button handler", () => {
  assert.match(qml, /function requestCredit\(\)[\s\S]*FaucetFlow\.beginCreditAttempt\(addressInput, FaucetFlow\.newRequestKey\)[\s\S]*sendCreditRequest\(\)/);
  // startRequestDrop is called from one place, always with the stored key.
  assert.equal(qml.split("backend.startRequestDrop(").length - 1, 1);
  assert.match(qml, /backend\.startRequestDrop\(creditAttempt\.address, creditAttempt\.requestKey\)/);
  assert.equal(qml.split("FaucetFlow.newRequestKey").length - 1, 1);
  // The reconnect path re-sends rather than restarting.
  assert.match(qml, /id: reconnectTimer[\s\S]*onTriggered: root\.sendCreditRequest\(\)/);
});

// ---------------------------------------------------------------------------
// error dispatch is structural
// ---------------------------------------------------------------------------

test("errors are dispatched on the code and never on the message", () => {
  // A message stuffed with every substring the old classifier keyed on must
  // not move the presentation away from what the code says.
  const noisy = {
    code: "rpc_timeout",
    message: "version skew: stale challenge, rpc offline, outcome unknown, network failed to fetch",
  };
  const presented = flow.errorPresentation(noisy);
  assert.equal(presented.code, "rpc_timeout");
  assert.equal(presented.title, flow.errorPresentation({ code: "rpc_timeout", message: "" }).title);
  assert.equal(presented.newAttempt, true);

  // ...and the reverse: a bland message under a terminal code stays terminal.
  const unknown = flow.errorPresentation({ code: "outcome_unknown", message: "done" });
  assert.equal(unknown.newAttempt, false);

  // Identical text, different codes, different answers.
  const sameText = "the request did not complete";
  assert.notEqual(
    flow.errorPresentation({ code: "network_unavailable", message: sameText }).title,
    flow.errorPresentation({ code: "pool_depleted", message: sameText }).title,
  );
  assert.equal(flow.errorPresentation({ code: "network_unavailable", message: sameText }).newAttempt, true);
  assert.equal(flow.errorPresentation({ code: "pool_depleted", message: sameText }).newAttempt, false);
});

test("an unrecognised code fails closed and offers no retry", () => {
  const presented = flow.errorPresentation({ code: "brand_new_code", message: "who knows" });
  assert.equal(presented.recognized, false);
  assert.equal(presented.newAttempt, false);
  assert.equal(presented.title, "The request could not be completed");
  assert.equal(flow.errorPresentation(null).newAttempt, false);
  assert.equal(flow.errorPresentation("a bare string").newAttempt, false);
});

test("no outcome that may already have credited the account invites a retry", () => {
  for (const code of [
    "outcome_unknown",
    "included_without_credit",
    "recipient_unreconciled",
    "idempotency_conflict",
    "unknown_job",
    "internal_error",
    "pool_depleted",
    "program_fingerprint_mismatch",
    "unsupported_difficulty",
    "recipient_uninitialized",
    "recipient_wrong_owner",
    "drop_in_progress",
  ]) {
    assert.equal(flow.errorPresentation({ code }).newAttempt, false, `${code} must not invite a retry`);
  }
});

test("the substring classifiers are gone from the flow module", () => {
  assert.doesNotMatch(source, /classifyError|classifyJobError/);
  assert.doesNotMatch(source, /indexOf\("(stale|rpc|network|version|timed out|outcome)/i);
  assert.doesNotMatch(source, /message[^\n]*toLowerCase/);
  assert.doesNotMatch(qml, /classifyError|classifyJobError/);
});

test("an uninitialized recipient surfaces the core's own command", () => {
  const command = "wallet auth-transfer init --account-id Public/Brrhdd";
  const presented = flow.errorPresentation({
    code: "recipient_uninitialized",
    message: "Public/Brrhdd exists but has not been initialized yet.",
    details: { initialization_command: command },
  });
  assert.equal(presented.initializationCommand, command);
  assert.equal(presented.newAttempt, false);
  assert.match(qml, /backend\.copyText\(command\)/);
});

// ---------------------------------------------------------------------------
// phases never render raw
// ---------------------------------------------------------------------------

test("no phase ever renders as its raw enum name", () => {
  for (const phase of dropPhases.concat(["queued"])) {
    const sentence = flow.phaseSentence(phase);
    assert.notEqual(sentence, phase, `${phase} must not render raw`);
    assert.doesNotMatch(sentence, /_/, `${phase} rendered with an underscore: ${sentence}`);
    assert.equal(sentence.length > 12, true, `${phase} needs a real sentence`);
  }
  // Anything this build has not heard of degrades to a sentence, not a label.
  for (const unheard of ["", null, undefined, "some_future_phase", "SOLVING"]) {
    assert.equal(flow.phaseSentence(unheard), "Working on your request…");
  }
  // The view renders the mapped sentence, never envelope.phase.
  assert.match(qml, /statusText = FaucetFlow\.phaseSentence\(envelope\.phase\)/);
  assert.doesNotMatch(qml, /statusText = String\(payload\.phase\)|text: [^\n]*\.phase\b/);
});

test("every phase lands on exactly one of the three lamps", () => {
  // The rail promises the user a shape for the whole request, so a phase the
  // core can report must never leave every lamp dark while a claim is plainly
  // running.
  for (const phase of dropPhases.concat(["queued"])) {
    const stage = flow.phaseStage(phase);
    assert.equal(
      Number.isInteger(stage) && stage >= 0 && stage <= 2,
      true,
      `${phase} maps to ${stage}, which is not one of the three lamps`,
    );
  }

  // The order the lamps light in is the order the work happens in.
  assert.equal(flow.phaseStage("queued"), 0);
  assert.equal(flow.phaseStage("solving"), 0);
  assert.equal(flow.phaseStage("refreshing_challenge"), 0);
  assert.equal(flow.phaseStage("submitting"), 1);
  assert.equal(flow.phaseStage("reconciling"), 2);

  // Reconciling is the only phase on the last lamp. That lamp is the one
  // carrying the five-minute caution, so anything else landing on it would
  // put the warning in front of a step that does not deserve it.
  const onLastLamp = dropPhases.filter((phase) => flow.phaseStage(phase) === 2);
  assert.deepEqual(onLastLamp, ["reconciling"]);

  // Every phase that cannot be cancelled is at or past the submit lamp: the
  // rail and the cancel button tell the same story about the point of no
  // return.
  for (const phase of dropPhases) {
    if (!flow.cancelAvailable("running", phase, false))
      assert.equal(flow.phaseStage(phase) >= 1, true, `${phase} is uncancellable but lights an early lamp`);
  }

  // An unrecognised phase lights nothing rather than lighting the wrong lamp.
  for (const unheard of ["", null, undefined, "some_future_phase", "SOLVING"]) {
    assert.equal(flow.phaseStage(unheard), -1);
  }
});

// Asserted against railStage itself rather than against the QML binding's
// characters: the binding was previously pinned by a source regex, which said
// nothing about what the rail does and broke the moment the expression moved.
test("the rail lights nothing before a claim and everything after a proven one", () => {
  const LAMPS = 3;

  // Before the first press the rail is a promise, not a report.
  assert.equal(flow.railStage("none", false, "", LAMPS), -1);

  // While a claim runs it tracks the phase.
  assert.equal(flow.railStage("none", true, "solving", LAMPS), flow.phaseStage("solving"));
  assert.equal(flow.railStage("none", true, "submitting", LAMPS), flow.phaseStage("submitting"));
  assert.equal(flow.railStage("none", true, "reconciling", LAMPS), flow.phaseStage("reconciling"));

  // A proven credit keeps every lamp lit beside the receipt. The poller has
  // stopped by then, so without this the rail would go dark exactly when it
  // has the most to say.
  assert.equal(flow.railStage("receipt", false, "", LAMPS), LAMPS);

  // Only a receipt earns it. An error or an unproven outcome is not a
  // finished run, and must not be dressed as one.
  for (const panel of ["error", "unknown"]) {
    assert.equal(flow.railStage(panel, false, "", LAMPS), -1, `${panel} must not light the rail`);
  }

  // An unrecognised phase mid-run still lights nothing rather than guessing.
  assert.equal(flow.railStage("none", true, "some_future_phase", LAMPS), -1);
});

test("cancelling is offered only while nothing has been sent", () => {
  assert.equal(flow.cancelAvailable("running", "solving", false), true);
  assert.equal(flow.cancelAvailable("queued", "", false), true);
  assert.equal(flow.cancelAvailable("running", "refreshing_challenge", false), true);
  assert.equal(flow.cancelAvailable("running", "submitting", false), false);
  assert.equal(flow.cancelAvailable("running", "reconciling", false), false);
  assert.equal(flow.cancelAvailable("running", "solving", true), false);
  assert.equal(flow.cancelAvailable("succeeded", "", false), false);
});

test("the live phase drives the cancel button and the reconciliation warning", () => {
  // The poller feeds the envelope's phase into cancelAvailable, and the Cancel
  // button is bound to nothing else — so once the phase reaches submitting or
  // reconciling, the button that would imply "this can still be taken back"
  // actually disappears.
  assert.match(qml, /cancellable = FaucetFlow\.cancelAvailable\(\s*envelope\.status, envelope\.phase, envelope\.cancel_requested\)/);
  assert.match(qml, /visible: root\.cancellable\s*\n\s*text: qsTr\("Cancel"\)/);

  // The 300 s reconciliation bound is disclosed in plain language: in the
  // phase sentence, and in the keep-the-app-open warning shown while it runs.
  assert.match(flow.phaseSentence("reconciling"), /minute/);
  assert.match(qml, /up to five minutes/);
  assert.match(qml, /visible: root\.creditPhase === "reconciling"/);

  // The stored raw phase is compared, never rendered.
  assert.doesNotMatch(qml, /text:[^\n]*creditPhase/);
});

// ---------------------------------------------------------------------------
// success means credited
// ---------------------------------------------------------------------------

test("terminal outcomes are read from the status and the error code", () => {
  assert.equal(flow.jobOutcome({ status: "running", phase: "solving" }), "running");
  assert.equal(flow.jobOutcome({ status: "queued" }), "running");
  assert.equal(flow.jobOutcome({ status: "succeeded" }), "succeeded");
  assert.equal(flow.jobOutcome({ status: "cancelled" }), "cancelled");
  assert.equal(flow.jobOutcome({ status: "outcome_unknown" }), "outcome_unknown");
  assert.equal(flow.jobOutcome({ status: "failed", error: { code: "outcome_unknown" } }), "outcome_unknown");
  assert.equal(flow.jobOutcome({ status: "failed", error: { code: "rpc_timeout" } }), "failed");
  assert.equal(flow.isTerminalOutcome("running"), false);
  assert.equal(flow.isTerminalOutcome("succeeded"), true);
});

test("a receipt is shown only for a proven, exact credit", () => {
  const recipient = {
    account_id: "BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf",
    address: "Public/BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf",
  };
  const proven = {
    request_key: flow.newRequestKey(),
    recipient,
    amount: "150",
    balance_before: "9007199254740993",
    balance_after: "9007199254741143",
    tx_hash: "5fb9fd56265effefabe13021d4031b62ec915d83312c24bd8dfa130afe053dc7",
    stale_challenge_retries: 0,
  };
  const view = flow.receiptView(proven);
  assert.equal(view.address, recipient.address);
  assert.equal(view.balanceBefore, "9007199254740993");
  assert.equal(view.balanceAfter, "9007199254741143");
  assert.equal(view.txHash, proven.tx_hash);
  assert.equal(view.retried, false);
  assert.equal(flow.receiptView({ ...proven, stale_challenge_retries: 2 }).retried, true);

  // No hash is no proof, whatever the balances say.
  assert.equal(flow.receiptView({ ...proven, tx_hash: "" }), null);
  assert.equal(flow.receiptView({ ...proven, tx_hash: null }), null);
  // Balances that do not differ by exactly the prize are not this claim's.
  assert.equal(flow.receiptView({ ...proven, balance_after: "9007199254741293" }), null);
  assert.equal(flow.receiptView({ ...proven, balance_after: "9007199254740993" }), null);
  // A rounding-equal pair that only Number() would accept.
  assert.equal(flow.receiptView({
    ...proven,
    balance_before: "9007199254740992",
    balance_after: "9007199254741143",
  }), null);
  assert.equal(flow.receiptView(null), null);
  assert.equal(flow.receiptView({ ...proven, recipient: {} }), null);
});

test("an unproven outcome shows the evidence and offers no retry", () => {
  const view = flow.unknownOutcomeView({
    code: "outcome_unknown",
    message: "The claim was submitted but its outcome could not be confirmed in time.",
    details: {
      outcome: "unknown",
      balance_before: "6000",
      tx_hash: "5fb9fd56265effefabe13021d4031b62ec915d83312c24bd8dfa130afe053dc7",
      submitted_challenge_fingerprint: "abc",
    },
  }, "Public/BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf");
  assert.equal(view.address, "Public/BrrhddVoucvgkLx3Cpe4uyf4HTv8PWJ72JtvJ39ieSCf");
  assert.equal(view.balanceBefore, "6000");
  assert.match(view.txHash, /^[0-9a-f]{64}$/);
  assert.equal(flow.errorPresentation({ code: "outcome_unknown" }).newAttempt, false);

  // The panel has no retry control at all.
  const panel = qml.slice(qml.indexOf("This claim could not be confirmed"));
  const panelEnd = panel.indexOf("// Failure.");
  assert.doesNotMatch(panel.slice(0, panelEnd), /LogosButton/);
});

test("the view never invents a receipt from an unproven success", () => {
  assert.match(
    qml,
    /var proven = FaucetFlow\.receiptView\(resultOf\(envelope\)\)[\s\S]*if \(proven\)[\s\S]*presentUnknown\(/,
  );
});

// ---------------------------------------------------------------------------
// copy and pool framing
// ---------------------------------------------------------------------------

test("the pool reads as finite, shared and instantaneous", () => {
  const view = flow.poolView({
    network: "lez-public-testnet",
    pool_balance: "12000",
    claims_remaining: "80",
    prize_amount: "150",
    difficulty_bytes: 3,
    effective_difficulty_bits: 24,
    can_claim: true,
    pinata_account: { account_id: "abc", address: "Public/abc" },
  });
  assert.equal(view.known, true);
  assert.equal(view.poolBalance, "12000");
  assert.equal(view.claimsRemaining, "80");
  assert.equal(view.canClaim, true);
  assert.equal(view.blockedSentence, "");

  const depleted = flow.poolView({ pool_balance: "0", claims_remaining: "0", can_claim: false, blocked_reason: "pool_depleted" });
  assert.equal(depleted.canClaim, false);
  assert.match(depleted.blockedSentence, /finite|empty/i);
  assert.match(flow.poolView({ pool_balance: "1", blocked_reason: "unsupported_difficulty" }).blockedSentence, /Update the app/);
  assert.notEqual(flow.poolView({ pool_balance: "1", blocked_reason: "something_new" }).blockedSentence, "");
  assert.equal(flow.poolView(null).known, false);
});

test("copy never promises an unlimited pool or a client-side protection", () => {
  const copy = `${qml}\n${source}`;
  for (const forbidden of [
    /unlimited/i,
    /infinite/i,
    /always available/i,
    /as much as you need/i,
    /rate limit/i,
    /anti-abuse/i,
    /protected against/i,
  ]) {
    assert.doesNotMatch(copy, forbidden, `forbidden copy: ${forbidden}`);
  }
  assert.match(qml, /finite, shared resource/);
  assert.match(qml, /at this instant/);
  // The button says what it does, and asks rather than promises.
  assert.match(qml, /text: qsTr\("Request 150 LEZ"\)/);
  assert.doesNotMatch(qml, /qsTr\("Get [^"]*LEZ"\)|qsTr\("Receive[^"]*"\)|qsTr\("Claim 150 LEZ"\)/);
  // The one thing this app cannot do for the user is stated, not hidden.
  assert.match(qml, /If you quit while a claim is running it cannot reconcile that claim afterwards/);
});

test("chain-derived text renders as plain text", () => {
  const textElements = qml.match(/LogosText \{[\s\S]*?\n {8,}\}/g) || [];
  assert.equal(textElements.length > 10, true, "expected the view to render several text elements");
  for (const element of textElements) {
    assert.match(element, /textFormat: Text\.PlainText/, `not plain text:\n${element}`);
  }
});

test("balances are never coerced to numbers anywhere in the view", () => {
  // A u128 does not survive a JavaScript Number, so no balance may ever meet
  // one: they are compared and added as decimal strings end to end.
  for (const code of [withoutComments(qml), withoutComments(source)]) {
    assert.doesNotMatch(code, /parseInt|parseFloat/);
    assert.doesNotMatch(code, /\bNumber\s*\(/);
  }
  assert.doesNotMatch(qml, /console\.(log|warn|error)/);
});

// ---------------------------------------------------------------------------
// the deleted surface
// ---------------------------------------------------------------------------

test("no faucet-ui source mentions the deleted account-owning surface", () => {
  // Static proof that the removal is complete rather than merely hidden behind
  // a screen that is no longer reachable.
  const forbidden = [
    "password",
    "mnemonic",
    "wallet",
    "recovery",
    "claim_until",
    "target",
    "startCreate",
    "startOpen",
  ];
  const files = everySourceFile(new URL(".", sourceRoot).pathname);
  assert.equal(files.length > 0, true);
  for (const file of files) {
    const contents = readFileSync(file, "utf8").toLowerCase();
    for (const word of forbidden) {
      assert.equal(
        contents.includes(word.toLowerCase()),
        false,
        `${file} still mentions "${word}"`,
      );
    }
  }
});

test("the remote interface is exactly the locked surface", () => {
  const expected = [
    "PROP(bool busy READONLY)",
    "PROP(QString activeJobId READONLY)",
    "PROP(QString sequencerUrl READONLY)",
    "SLOT(QString bootstrap())",
    "SLOT(QString startFaucetInfo())",
    "SLOT(QString startInspectRecipient(QString address))",
    "SLOT(QString startRequestDrop(QString address, QString requestKey))",
    "SLOT(QString cancelJob(QString jobId))",
    "SLOT(QString jobStatus(QString jobId))",
    "SLOT(QString acknowledgeJob(QString jobId))",
    "SLOT(QString copyText(QString text))",
  ];
  const declared = rep.split("\n")
    .map((line) => line.trim())
    .filter((line) => line.startsWith("PROP(") || line.startsWith("SLOT("));
  assert.deepEqual(declared, expected);

  // The credential surface is asserted absent, not merely unused.
  assert.doesNotMatch(rep, /startCreate|startOpen|startBalance|startClaim|ExternalRecipient/i);
});

test("the backend touches no filesystem and pins its sequencer", () => {
  for (const banned of [
    /QStandardPaths/,
    /QSettings/,
    /QDir/,
    /QFile/,
    /QFileInfo/,
    /m_configPath/,
    /m_storagePath/,
    /mkpath/,
    // A redirectable sequencer can fabricate the pool, the fingerprint and the
    // success, and harvest every address entered.
    /LEZ_FAUCET_SEQUENCER_URL/,
    /qEnvironmentVariable/,
    /getenv/,
  ]) {
    assert.doesNotMatch(backend, banned, `FaucetBackend.cpp still references ${banned}`);
    assert.doesNotMatch(backendHeader, banned, `FaucetBackend.h still references ${banned}`);
  }
  assert.match(backend, /constexpr auto SEQUENCER_URL = "https:\/\/testnet\.lez\.logos\.co"/);
  // Real JSON parsing, never a brace scan.
  assert.match(backend, /QJsonDocument::fromJson/);
  // The module does not trust its caller: bounds and key shape are re-checked.
  assert.match(backend, /MAX_ACCOUNT_ID_LENGTH = 64/);
  assert.match(backend, /boundedAddress\(address\)/);
  assert.match(backend, /isRequestKey\(requestKey\)/);
  // Reads must not queue behind a live credit request.
  assert.match(backend, /kind == QStringLiteral\("drop"\) && !activeJobId\(\)\.isEmpty\(\)/);
});

test("errors never quote the user's input back at them", () => {
  // Local validation failures name the expected shape, never the rejected text.
  assert.doesNotMatch(backend, /\.arg\(address\)|\.arg\(accountId\)|arg\(requestKey\)/);
  assert.doesNotMatch(qml, /errorText[^\n]*addressInput|\.arg\(root\.addressInput\)/);
  // Only the canonical address the core returned is ever displayed.
  assert.match(qml, /text: root\.inspection \? root\.inspection\.address : ""/);
  assert.match(qml, /text: root\.receipt \? root\.receipt\.address : ""/);
  assert.doesNotMatch(qml, /text: root\.addressInput/);
});
