// Cross-layer contract test.
//
// The Rust core, the C++ module and this view are developed and tested
// separately, so nothing else checks that the JSON one layer emits is the JSON
// the next layer can actually read. The fixtures here are not hand-written:
// they are produced by `cargo run --example dump_contract_fixtures`, so they
// are literally what serde emits. If a field is renamed or a shape changes,
// this fails instead of the app quietly rendering a blank panel.
import { test } from "node:test"
import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { fileURLToPath } from "node:url"
import { dirname, join } from "node:path"
import vm from "node:vm"

const here = dirname(fileURLToPath(import.meta.url))
const root = join(here, "..", "..")

const flow = {}
vm.createContext(flow)
vm.runInContext(readFileSync(join(root, "src/qml/FaucetFlow.js"), "utf8"), flow)

const fixtures = JSON.parse(
  readFileSync(join(root, "tests/fixtures/core-contract.json"), "utf8"))

test("the pool panel reads real core faucet info", () => {
  const pool = flow.poolView(fixtures.faucet_info)
  assert.equal(pool.poolBalance, "1476900")
  assert.equal(pool.claimsRemaining, "9846")
  assert.equal(pool.canClaim, true)
  // Exact strings, never coerced through Number.
  assert.equal(typeof pool.poolBalance, "string")
})

test("an eligible recipient reads from real core output", () => {
  const view = flow.inspectionView(fixtures.inspection_eligible)
  assert.equal(view.eligibility, "eligible")
  assert.equal(view.address, "Public/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r")
  assert.equal(view.balance, "6000")
})

test("an uninitialized recipient surfaces the core's own command", () => {
  const view = flow.inspectionView(fixtures.inspection_uninitialized)
  assert.equal(view.eligibility, "uninitialized")
  assert.match(view.initializationCommand, /auth-transfer init/)
  assert.doesNotMatch(view.initializationCommand, /mnemonic|password|private/i)
})

test("a real receipt is accepted and proves the exact credit", () => {
  const receipt = flow.receiptView(fixtures.receipt)
  assert.ok(receipt, "the view must accept the receipt the core actually emits")
  assert.equal(receipt.balanceBefore, "6000")
  assert.equal(receipt.balanceAfter, "6150")
  assert.equal(receipt.txHash.length, 64)
})

test("a receipt whose arithmetic does not hold is refused", () => {
  // Same shape, wrong sum. The view must not render a credit it cannot verify.
  const tampered = { ...fixtures.receipt, balance_after: "6151" }
  assert.equal(flow.receiptView(tampered), null)
  // A receipt without a transaction hash is likewise not a receipt.
  const noHash = { ...fixtures.receipt, tx_hash: "" }
  assert.equal(flow.receiptView(noHash), null)
})

test("real core errors are recognised and dispatched on their code", () => {
  const presented = flow.errorPresentation(fixtures.error_uninitialized)
  assert.ok(presented.recognized, "a real core error code must be recognised")
  assert.ok(presented.title)
  // The core's own command is surfaced from details, not reconstructed here.
  assert.match(presented.initializationCommand || "", /^$|auth-transfer init/)
})

test("an unproven outcome is routed to its own panel, and fails closed if not", () => {
  // outcome_unknown is deliberately absent from the error table: jobOutcome
  // routes it to the dedicated panel that shows the evidence and offers no
  // button. Assert that routing, and assert that the generic fallback would
  // still refuse a new attempt if it ever arrived here by another path.
  assert.equal(
    flow.jobOutcome({ status: "failed", error: fixtures.error_outcome_unknown }),
    "outcome_unknown")
  assert.equal(
    flow.errorPresentation(fixtures.error_outcome_unknown).newAttempt, false,
    "the unrecognised-code fallback must fail closed")
})

test("no outcome that may already have credited invites a new attempt", () => {
  // This is the rule that stops one press becoming two credits.
  assert.equal(
    flow.errorPresentation(fixtures.error_outcome_unknown).newAttempt, false)
  for (const code of ["outcome_unknown", "recipient_unreconciled"]) {
    assert.equal(flow.errorPresentation({ code: code }).newAttempt, false,
      `${code} leaves a transaction that may have paid out`)
  }

  // By contrast, a rejected submission provably sent nothing, so offering
  // another attempt is correct rather than reckless. In the pinned sequencer,
  // every error return in sendTransaction happens before the mempool push
  // (lez/sequencer/service/src/service.rs), so a rejection means the
  // transaction never entered the network.
  assert.equal(
    flow.errorPresentation({ code: "submission_rejected" }).newAttempt, true)
})

test("the unproven-outcome panel uses the core's canonical recipient", () => {
  const view = flow.unknownOutcomeView(fixtures.error_outcome_unknown, "")
  assert.equal(view.address, "Public/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r")
  assert.equal(view.balanceBefore, "6000")
  assert.equal(view.txHash.length, 64)
})

test("omitted optional fields behave exactly like null", () => {
  // The core omits absent fields; the module does too. Earlier layers emitted
  // null. Both must be read identically or a panel silently goes blank.
  const omitted = fixtures.inspection_eligible
  const nulled = { ...omitted, initialization_command: null }
  assert.deepEqual(flow.inspectionView(nulled), flow.inspectionView(omitted))

  const envOmitted = { status: "running" }
  const envNulled = { status: "running", result: null, error: null }
  assert.equal(flow.jobOutcome(envNulled), flow.jobOutcome(envOmitted))
})

test("every phase the core can emit renders as a sentence", () => {
  // Sourced from the Rust DropPhase enum, not from what the view happens to
  // handle, so a new phase upstream fails here rather than leaking its raw
  // enum name into the UI.
  const phases = readFileSync(join(root, "..", "lez-faucet-ffi/src/error.rs"), "utf8")
    .split("pub enum DropPhase")[1].split("}")[0]
    .match(/^\s{4}([A-Z]\w+),/gm)
    .map((line) => line.trim().replace(",", "")
      .replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase())

  assert.ok(phases.length >= 8, `expected the full phase list, got ${phases}`)
  for (const phase of phases) {
    const sentence = flow.phaseSentence(phase)
    assert.ok(sentence && sentence.length > 0, `${phase} has no sentence`)
    assert.ok(!sentence.includes("_"),
      `${phase} leaked its raw enum name: ${sentence}`)
  }
})
