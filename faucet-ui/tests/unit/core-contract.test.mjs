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

const backend = readFileSync(join(root, "src/FaucetBackend.cpp"), "utf8")

test("the pool panel reads real core faucet info", () => {
  const pool = flow.poolView(fixtures.faucet_info)
  assert.equal(pool.poolBalance, "1476900")
  assert.equal(pool.claimsRemaining, "9846")
  assert.equal(pool.canClaim, true)
  assert.equal(pool.network, "lez-legacy-testnet")
  assert.equal(pool.networkName, "LEZ legacy testnet")
  assert.equal(pool.lezVersion, "v0.2.4-lez-legacy")
  assert.equal(pool.explorerUrl, "https://explorer.testnet-legacy.lez.logos.co")
  assert.equal(pool.mechanism, "pinata")
  assert.equal(pool.decimals, 0)
  assert.equal(pool.prizeAmount, "150")
  assert.equal(pool.poolAddress, "Public/EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7")
  assert.equal(pool.difficultyBits, "24")
  // Exact strings, never coerced through Number.
  assert.equal(typeof pool.poolBalance, "string")
})

test("the pool panel reads real v0.3 faucet info as a hot balance in lepta", () => {
  const pool = flow.poolView(fixtures.faucet_info_v03)
  assert.equal(pool.network, "lez-v03-testnet")
  assert.equal(pool.mechanism, "claim_api")
  assert.equal(pool.isClaimApi, true)
  assert.equal(pool.decimals, 9)
  assert.equal(pool.prizeAmount, "100000000")
  assert.equal(flow.formatAmount(pool.prizeAmount, pool.decimals), "0.1")
  assert.equal(pool.poolBalance, "1234567890")
  assert.equal(pool.claimsRemaining, "7")
  assert.equal(pool.canClaim, true)
  assert.equal(pool.paused, false)
  assert.equal(pool.claimApiUrl, "https://rog-devbox.tail957e2.ts.net:8443")
  assert.equal(pool.explorerUrl, "https://explorer.testnet.lez.logos.co")
  // Piñata-only fields are absent from the wire, and read as empty.
  assert.equal("pinata_account" in fixtures.faucet_info_v03, false)
  assert.equal("difficulty_bytes" in fixtures.faucet_info_v03, false)
  assert.equal(pool.poolAddress, "")
  assert.equal(pool.difficultyBits, "")
})

test("the backend's network table mirrors the core's row for row", () => {
  // The core emits its table; the backend carries a hand-written copy with
  // the same fields. Every value the view displays must agree between them.
  assert.equal(fixtures.default_network, "lez-legacy-testnet")
  assert.equal(fixtures.networks.length, 2)
  for (const row of fixtures.networks) {
    const view = flow.networkView(row)
    assert.equal(view.id, row.id)
    assert.ok(view.prize !== "", `${row.id} has a prize`)
    for (const value of [row.id, row.name, row.sequencerUrl, row.explorerUrl,
      row.lezVersion, row.prize, row.mechanism]) {
      assert.ok(backend.includes(`"${value}"`), `FaucetBackend.cpp lacks "${value}" for ${row.id}`)
    }
    assert.ok(backend.includes(`"${row.claimApiUrl || ""}",`), `FaucetBackend.cpp lacks the claim API root for ${row.id}`)
    assert.ok(new RegExp(`"${row.prize}",\\s*${row.decimals},`).test(backend),
      `FaucetBackend.cpp has prize ${row.prize} at ${row.decimals} decimals for ${row.id}`)
  }
  const [legacy, v03] = fixtures.networks.map(flow.networkView)
  assert.equal(flow.formatAmount(legacy.prize, legacy.decimals), "150")
  assert.equal(flow.formatAmount(v03.prize, v03.decimals), "0.1")
  assert.equal(v03.isClaimApi, true)
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

test("a v0.3 recipient is eligible with a balance and no command", () => {
  const view = flow.inspectionView(fixtures.inspection_v03)
  assert.equal(view.eligibility, "eligible")
  assert.equal(view.eligible, true)
  assert.equal(view.balance, "0")
  assert.equal(view.initializationCommand, "")
})

test("a real receipt is accepted and proves the exact credit", () => {
  const legacyPrize = flow.networkView(fixtures.networks[0]).prize
  const receipt = flow.receiptView(fixtures.receipt, legacyPrize)
  assert.ok(receipt, "the view must accept the receipt the core actually emits")
  assert.equal(receipt.balanceBefore, "6000")
  assert.equal(receipt.balanceAfter, "6150")
  assert.equal(receipt.txHash.length, 64)

  const v03Prize = flow.networkView(fixtures.networks[1]).prize
  const lepta = flow.receiptView(fixtures.receipt_v03, v03Prize)
  assert.ok(lepta, "the view must accept the v0.3 receipt the core actually emits")
  assert.equal(lepta.amount, "100000000")
  assert.equal(flow.formatAmount(lepta.balanceBefore, 9), "0.25")
  assert.equal(flow.formatAmount(lepta.balanceAfter, 9), "0.35")
  // Each receipt is a receipt only on its own network.
  assert.equal(flow.receiptView(fixtures.receipt, v03Prize), null)
  assert.equal(flow.receiptView(fixtures.receipt_v03, legacyPrize), null)
})

test("a receipt whose arithmetic does not hold is refused", () => {
  const legacyPrize = flow.networkView(fixtures.networks[0]).prize
  // Same shape, wrong sum. The view must not render a credit it cannot verify.
  const tampered = { ...fixtures.receipt, balance_after: "6151" }
  assert.equal(flow.receiptView(tampered, legacyPrize), null)
  // A receipt without a transaction hash is likewise not a receipt.
  const noHash = { ...fixtures.receipt, tx_hash: "" }
  assert.equal(flow.receiptView(noHash, legacyPrize), null)
})

test("the claim service's refusals arrive as codes the view already knows", () => {
  for (const [name, newAttempt] of [
    ["error_rate_limited", false],
    ["error_faucet_paused", false],
    ["error_claim_api_unavailable", true],
  ]) {
    const presented = flow.errorPresentation(fixtures[name])
    assert.ok(presented.recognized, `${name} must be recognised`)
    assert.equal(presented.newAttempt, newAttempt, name)
    assert.equal(presented.initializationCommand, "")
  }
  assert.equal(flow.errorPresentation(fixtures.error_rate_limited).retryAfterSeconds, "86000")
  assert.equal(fixtures.error_claim_api_unavailable.retryable, true)
  assert.equal(fixtures.error_rate_limited.retryable, false)
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
