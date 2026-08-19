#include "lez_faucet_module.h"
#include "mock_lez_faucet_ffi.h"

#include <atomic>
#include <chrono>
#include <cstdlib>
#include <fstream>
#include <iostream>
#include <memory>
#include <mutex>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

namespace {

[[noreturn]] void failCheck(const char* expression, int line)
{
    std::cerr << "check failed at line " << line << ": " << expression << '\n';
    std::abort();
}

#define CHECK(expression) ((expression) ? static_cast<void>(0) : failCheck(#expression, __LINE__))

constexpr const char* kUrl = "https://testnet.lez.logos.co";
constexpr const char* kAddress = "Public/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r";
constexpr const char* kKey = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
constexpr const char* kOtherKey = "9c1b0a77-5d2e-4f61-8a0b-1c2d3e4f5a6b";

// The module's serializer is deterministic and compact, so an exact
// `"key":"value"` match is an exact structural assertion here. Tests
// deliberately do not reuse the module's parser: a parser bug that made every
// payload agree with itself would otherwise be invisible.
bool has(const std::string& json, const std::string& fragment)
{
    return json.find(fragment) != std::string::npos;
}

bool statusIs(const std::string& json, const std::string& status)
{
    return has(json, "\"status\":\"" + status + "\"");
}

bool codeIs(const std::string& json, const std::string& code)
{
    return has(json, "\"code\":\"" + code + "\"");
}

std::string stringField(const std::string& json, const std::string& key)
{
    const std::string needle = "\"" + key + "\":\"";
    const auto start = json.find(needle);
    CHECK(start != std::string::npos);
    const auto valueStart = start + needle.size();
    const auto end = json.find('"', valueStart);
    CHECK(end != std::string::npos);
    return json.substr(valueStart, end - valueStart);
}

bool isTerminalJson(const std::string& json)
{
    return statusIs(json, "succeeded") || statusIs(json, "failed") || statusIs(json, "cancelled")
        || statusIs(json, "outcome_unknown");
}

std::string waitForTerminal(LezFaucetModule& module, const std::string& jobId, int attempts = 1000)
{
    for (int attempt = 0; attempt < attempts; ++attempt) {
        const std::string status = module.jobStatus(jobId);
        if (isTerminalJson(status)) {
            return status;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
    CHECK(false && "job did not reach a terminal state");
    return {};
}

std::string startAndWait(LezFaucetModule& module, const std::string& started)
{
    CHECK(has(started, "\"ok\":true"));
    return waitForTerminal(module, stringField(started, "job_id"));
}

/// A configured module with the mock reset. Every test starts from here.
void configure(LezFaucetModule& module)
{
    const std::string configured = module.configure(kUrl);
    CHECK(has(configured, "\"ok\":true"));
    CHECK(has(configured, "\"configured\":true"));
}

std::string readFile(const char* path)
{
    std::ifstream input(path);
    CHECK(input.good());
    std::ostringstream buffer;
    buffer << input.rdbuf();
    return buffer.str();
}

// ---- tests -----------------------------------------------------------------

void configureValidatesAndIsRejectedWhileAJobIsLive()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;

    // The module validates before it ever reaches the FFI.
    CHECK(codeIs(module.configure(""), "invalid_sequencer_url"));
    CHECK(codeIs(module.configure(std::string("https://a\x01.example")), "invalid_sequencer_url"));
    CHECK(MockLezFaucetFfi::clientNewCalls() == 0);

    // A structured Rust refusal is passed through, code intact.
    MockLezFaucetFfi::setClientNewFails(true);
    const std::string refused = module.configure("https://evil.example.com");
    CHECK(has(refused, "\"ok\":false"));
    CHECK(codeIs(refused, "invalid_sequencer_url"));
    MockLezFaucetFfi::setClientNewFails(false);

    configure(module);
    CHECK(MockLezFaucetFfi::lastSequencerUrl() == kUrl);

    // Reconfiguring under a live job would swap the handle out from under a
    // worker that is inside the FFI with the old one.
    MockLezFaucetFfi::setInfoDelayMs(120);
    const std::string started = module.getFaucetInfo();
    CHECK(codeIs(module.configure(kUrl), "drop_in_progress"));
    CHECK(statusIs(startAndWait(module, started), "succeeded"));

    // With everything terminal, reconfiguration is allowed and frees the old
    // client exactly once.
    MockLezFaucetFfi::setInfoDelayMs(0);
    configure(module);
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
}

void startersReturnImmediatelyUnderASlowFfi()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);
    MockLezFaucetFfi::setInfoDelayMs(300);
    MockLezFaucetFfi::setInspectDelayMs(300);
    MockLezFaucetFfi::setDropDelayMs(300);

    std::vector<std::string> started;
    const auto before = std::chrono::steady_clock::now();
    started.push_back(module.getFaucetInfo());
    started.push_back(module.inspectRecipient(kAddress));
    started.push_back(module.requestDrop(kAddress, kKey));
    const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now() - before);
    if (elapsed.count() >= 50) {
        std::cerr << "starters blocked for " << elapsed.count() << " ms\n";
        std::abort();
    }

    for (const std::string& response : started) {
        CHECK(has(response, "\"ok\":true"));
        CHECK(statusIs(response, "queued") || statusIs(response, "running"));
        CHECK(statusIs(waitForTerminal(module, stringField(response, "job_id")), "succeeded"));
    }
}

void readsDoNotQueueBehindADrop()
{
    // API_LOCK §A1.7.2: a 300 s reconciliation must not make the app look
    // frozen, so no read may wait on the mutating-job permit.
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    LezFaucetModule module;
    configure(module);

    const std::string drop = module.requestDrop(kAddress, kKey);
    const std::string dropId = stringField(drop, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    const std::string info = startAndWait(module, module.getFaucetInfo());
    CHECK(statusIs(info, "succeeded"));
    CHECK(has(info, "\"pool_balance\":\"1500000\""));
    const std::string inspection = startAndWait(module, module.inspectRecipient(kAddress));
    CHECK(statusIs(inspection, "succeeded"));
    CHECK(has(inspection, "\"eligibility\":\"eligible\""));

    // The drop is still mid-reconciliation the whole time.
    CHECK(!isTerminalJson(module.jobStatus(dropId)));
    MockLezFaucetFfi::releaseDrop();
    CHECK(statusIs(waitForTerminal(module, dropId), "succeeded"));
}

void terminalResultsReplayUntilAcknowledged()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);

    const std::string started = module.inspectRecipient(kAddress);
    const std::string jobId = stringField(started, "job_id");
    CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));

    for (int replay = 0; replay < 5; ++replay) {
        const std::string status = module.jobStatus(jobId);
        CHECK(statusIs(status, "succeeded"));
        CHECK(has(status, "\"eligibility\":\"eligible\""));
    }
    const std::string acknowledged = module.jobResultAck(jobId);
    CHECK(has(acknowledged, "\"acknowledged\":true"));
    CHECK(has(acknowledged, "\"reaped\":true"));
    CHECK(codeIs(module.jobStatus(jobId), "unknown_job"));
    CHECK(codeIs(module.jobResultAck(jobId), "unknown_job"));
    CHECK(MockLezFaucetFfi::inspectCalls() == 1);
}

void nonTerminalAcknowledgementIsStructured()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setInfoDelayMs(150);
    LezFaucetModule module;
    configure(module);
    const std::string jobId = stringField(module.getFaucetInfo(), "job_id");
    CHECK(codeIs(module.jobResultAck(jobId), "job_not_terminal"));
    CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));
}

void aRepeatedRequestKeyReplaysTheOriginalJob()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    LezFaucetModule module;
    configure(module);

    const std::string first = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(first, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    // While it runs: the same key returns the same job, and starts nothing.
    const std::string replayWhileLive = module.requestDrop(kAddress, kKey);
    CHECK(has(replayWhileLive, "\"ok\":true"));
    CHECK(stringField(replayWhileLive, "job_id") == jobId);
    CHECK(stringField(replayWhileLive, "request_key") == kKey);

    // A different address with the same key is a conflict, not a replay.
    CHECK(codeIs(module.requestDrop("Public/OtherAccount", kKey), "idempotency_conflict"));
    // A different key while one drop is live is refused, never queued.
    CHECK(codeIs(module.requestDrop(kAddress, kOtherKey), "drop_in_progress"));

    MockLezFaucetFfi::releaseDrop();
    CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));

    // And after it finishes.
    const std::string replayAfter = module.requestDrop(kAddress, kKey);
    CHECK(stringField(replayAfter, "job_id") == jobId);
    CHECK(MockLezFaucetFfi::dropCalls() == 1);
    CHECK(MockLezFaucetFfi::lastRequestKey() == kKey);
}

void tombstonesSurviveJobReaping()
{
    // API_LOCK §A1.7.4: job records are bounded and reapable; a request-key
    // tombstone is a separate table that is never evicted. Forgetting a key
    // would turn a bridge retry into a second 150 LEZ claim.
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);

    const std::string first = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(first, "job_id");
    CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));
    CHECK(has(module.jobResultAck(jobId), "\"reaped\":true"));
    CHECK(codeIs(module.jobStatus(jobId), "unknown_job"));

    // Fill and drain the job table so nothing of the original record survives.
    for (int iteration = 0; iteration < 200; ++iteration) {
        const std::string readId = stringField(module.getFaucetInfo(), "job_id");
        waitForTerminal(module, readId);
        module.jobResultAck(readId);
    }

    const std::string replay = module.requestDrop(kAddress, kKey);
    CHECK(has(replay, "\"ok\":true"));
    CHECK(stringField(replay, "job_id") == jobId);
    CHECK(stringField(replay, "status") == "succeeded");
    CHECK(MockLezFaucetFfi::dropCalls() == 1);
}

void anErrorMessageShapedLikeSuccessIsStillAnError()
{
    // The old brace-counting scanner found the first `"ok"` anywhere in the
    // document, so a nested one — in an error's own details, or quoted inside
    // its message — decided that a FAILED operation had succeeded. This payload
    // is that exact shape.
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropResponse(
        "{\"error\":{\"code\":\"pool_depleted\","
        "\"message\":\"the sequencer echoed {\\\"ok\\\":true,\\\"result\\\":150} and refused\","
        "\"retryable\":false,\"details\":{\"ok\":true,\"echo\":\"ok\"}},\"ok\":false}");
    LezFaucetModule module;
    configure(module);

    const std::string failed = startAndWait(module, module.requestDrop(kAddress, kKey));
    CHECK(statusIs(failed, "failed"));
    CHECK(codeIs(failed, "pool_depleted"));
    CHECK(!statusIs(failed, "succeeded"));

    // The same trap in the other direction: a genuine success whose payload
    // contains the text "false" must still be a success.
    MockLezFaucetFfi::setDropResponse(
        "{\"ok\":true,\"result\":{\"request_key\":\"k\",\"recipient\":{\"account_id\":\"CbgR\","
        "\"address\":\"Public/CbgR\"},\"amount\":\"150\",\"balance_before\":\"0\","
        "\"balance_after\":\"150\",\"tx_hash\":\"0f\",\"stale_challenge_retries\":0,"
        "\"note\":\"\\\"ok\\\":false\"}}");
    const std::string succeeded = startAndWait(module, module.requestDrop(kAddress, kOtherKey));
    CHECK(statusIs(succeeded, "succeeded"));
    CHECK(has(succeeded, "\"balance_after\":\"150\""));
}

void malformedFfiPayloadsFailClosed()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);

    for (const char* payload : {
             "{\"ok\":true,\"result\":",          // truncated
             "{\"ok\":\"true\",\"result\":{}}",   // ok is not a boolean
             "not json at all",
             "{\"ok\":false}",                    // failure without an error object
             "{}",
         }) {
        MockLezFaucetFfi::setInfoResponse(payload);
        const std::string failed = startAndWait(module, module.getFaucetInfo());
        CHECK(statusIs(failed, "failed"));
        CHECK(codeIs(failed, "internal_error"));
    }

    // A success carrying a balance that is not a canonical decimal is a failure
    // of the whole operation: every one of these values is money.
    MockLezFaucetFfi::setInfoResponse(
        "{\"ok\":true,\"result\":{\"prize_amount\":\"150\",\"pool_balance\":\"1.5e6\","
        "\"claims_remaining\":\"10000\"}}");
    const std::string rejected = startAndWait(module, module.getFaucetInfo());
    CHECK(statusIs(rejected, "failed"));
    CHECK(codeIs(rejected, "internal_error"));
}

void anUnknownOutcomeThatMentionsCancellingIsNotCancelled()
{
    // API_LOCK §9: status comes from the code and from nothing else. Reading
    // "cancel" out of this message and filing it as `cancelled` would tell the
    // user nothing was sent while 150 LEZ was in flight.
    MockLezFaucetFfi::reset();
    // The drop is already past submission: it notices the cancel and keeps
    // reconciling anyway, exactly as the real client does.
    MockLezFaucetFfi::setDropWaitsForCancel(true);
    MockLezFaucetFfi::setDropResponse(
        "{\"ok\":false,\"error\":{\"code\":\"outcome_unknown\","
        "\"message\":\"The claim was submitted and could not be confirmed. Cancelling now cannot "
        "recall it; cancel requests after submission are not honoured.\",\"retryable\":false,"
        "\"phase\":\"reconciling\",\"details\":{\"outcome\":\"unknown\","
        "\"balance_before\":\"9007199254740993\",\"tx_hash\":\"0f0f\","
        "\"submitted_challenge_fingerprint\":\"ab12\"}}}");
    LezFaucetModule module;
    configure(module);

    const std::string started = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(started, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));
    // Even with a cancel actually requested, the structured code decides.
    CHECK(has(module.cancel(jobId), "\"status\":\"cancelling\""));
    const std::string terminal = waitForTerminal(module, jobId);
    CHECK(statusIs(terminal, "outcome_unknown"));
    CHECK(!statusIs(terminal, "cancelled"));
    CHECK(codeIs(terminal, "outcome_unknown"));
    CHECK(has(terminal, "\"cancel_requested\":true"));
    CHECK(has(terminal, "\"phase\":\"reconciling\""));
    CHECK(has(terminal, "\"tx_hash\":\"0f0f\""));
}

void cancellationBeforeSubmissionIsCancelledAndTokenScoped()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForCancel(true);
    MockLezFaucetFfi::setDropHonoursCancel(true);
    LezFaucetModule module;
    configure(module);

    const std::string started = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(started, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    const std::string cancelling = module.cancel(jobId);
    CHECK(has(cancelling, "\"cancel_requested\":true"));
    const std::string cancelled = waitForTerminal(module, jobId);
    CHECK(statusIs(cancelled, "cancelled"));
    CHECK(codeIs(cancelled, "cancelled"));

    // Tokens are allocated by the module, never 0, and a cancel targets exactly
    // the job it names (API_LOCK §A1.7.5).
    CHECK(MockLezFaucetFfi::lastDropToken() != 0);
    CHECK(MockLezFaucetFfi::lastCancelToken() == MockLezFaucetFfi::lastDropToken());

    // A cancel for an unknown job is structured, not a crash.
    CHECK(codeIs(module.cancel("faucet_info-999"), "unknown_job"));
}

void aQueuedJobCancelledBeforeItStartsNeverReachesTheFfi()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    LezFaucetModule module;
    configure(module);

    // Occupy the single mutating slot, then cancel a read before its worker
    // has a chance to run.
    const std::string drop = module.requestDrop(kAddress, kKey);
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));
    MockLezFaucetFfi::setInfoResponse("{\"ok\":true,\"result\":{\"prize_amount\":\"150\","
                                      "\"pool_balance\":\"0\",\"claims_remaining\":\"0\"}}");

    const std::string started = module.getFaucetInfo();
    const std::string jobId = stringField(started, "job_id");
    module.cancel(jobId);
    const std::string terminal = waitForTerminal(module, jobId);
    // Either it was cancelled before entering the FFI, or it had already
    // finished; it must never be reported as cancelled after succeeding.
    CHECK(statusIs(terminal, "cancelled") || statusIs(terminal, "succeeded"));
    if (statusIs(terminal, "cancelled")) {
        CHECK(codeIs(terminal, "cancelled"));
    }

    MockLezFaucetFfi::releaseDrop();
    CHECK(statusIs(waitForTerminal(module, stringField(drop, "job_id")), "succeeded"));
}

void aRunningDropReportsALivePhaseThatMovesAndSurvivesTermination()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    MockLezFaucetFfi::setPhaseResponse("{\"ok\":true,\"result\":{\"phase\":\"solving\"}}");
    LezFaucetModule module;
    configure(module);

    // A read job has no operation token, so its status never polls a phase.
    CHECK(statusIs(startAndWait(module, module.getFaucetInfo()), "succeeded"));
    CHECK(MockLezFaucetFfi::phaseCalls() == 0);

    const std::string started = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(started, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    // The first poll already carries the live phase, fetched for the drop's
    // own operation token.
    const std::string solving = module.jobStatus(jobId);
    CHECK(statusIs(solving, "running"));
    CHECK(has(solving, "\"phase\":\"solving\""));
    CHECK(MockLezFaucetFfi::lastPhaseToken() == MockLezFaucetFfi::lastDropToken());

    // The phase moves while the same job is still running.
    MockLezFaucetFfi::setPhaseResponse("{\"ok\":true,\"result\":{\"phase\":\"submitting\"}}");
    CHECK(has(module.jobStatus(jobId), "\"phase\":\"submitting\""));

    // A null phase means "no live phase right now"; the envelope keeps the
    // last known phase rather than flickering back to nothing.
    MockLezFaucetFfi::setPhaseResponse("{\"ok\":true,\"result\":{\"phase\":null}}");
    CHECK(has(module.jobStatus(jobId), "\"phase\":\"submitting\""));

    // After the drop succeeds, the terminal envelope still reports the final
    // phase, and no further poll is made for a terminal job.
    MockLezFaucetFfi::releaseDrop();
    const std::string terminal = waitForTerminal(module, jobId);
    CHECK(statusIs(terminal, "succeeded"));
    CHECK(has(terminal, "\"phase\":\"submitting\""));
    const int polls = MockLezFaucetFfi::phaseCalls();
    CHECK(has(module.jobStatus(jobId), "\"phase\":\"submitting\""));
    CHECK(MockLezFaucetFfi::phaseCalls() == polls);
}

void theOutcomePhaseOverridesTheLastPolledPhase()
{
    // A terminal error names the phase it actually failed in. That verdict
    // outranks whatever a poll happened to see last.
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    MockLezFaucetFfi::setPhaseResponse("{\"ok\":true,\"result\":{\"phase\":\"solving\"}}");
    MockLezFaucetFfi::setDropResponse(
        "{\"ok\":false,\"error\":{\"code\":\"submission_rejected\",\"message\":\"The LEZ testnet "
        "sequencer rejected the claim transaction.\",\"retryable\":false,"
        "\"phase\":\"submitting\"}}");
    LezFaucetModule module;
    configure(module);

    const std::string started = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(started, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));
    CHECK(has(module.jobStatus(jobId), "\"phase\":\"solving\""));

    MockLezFaucetFfi::releaseDrop();
    const std::string terminal = waitForTerminal(module, jobId);
    CHECK(statusIs(terminal, "failed"));
    CHECK(codeIs(terminal, "submission_rejected"));
    CHECK(has(terminal, "\"phase\":\"submitting\""));
}

void phasePollingNeverBlocksAConcurrentCaller()
{
    // The real phase read is two atomics, but the module must not depend on
    // that: even a poll that never returns may not hold job->mutex or stall
    // any other caller of this module.
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    MockLezFaucetFfi::setPhaseBlocks(true);
    LezFaucetModule module;
    configure(module);

    const std::string started = module.requestDrop(kAddress, kKey);
    const std::string jobId = stringField(started, "job_id");
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    std::atomic<bool> pollReturned{false};
    std::thread polling([&]() {
        const std::string status = module.jobStatus(jobId);
        CHECK(statusIs(status, "running") || statusIs(status, "cancelling"));
        pollReturned = true;
    });
    CHECK(MockLezFaucetFfi::waitForPhaseStart(1000));

    // While that poll is stuck inside the FFI: a cancel takes job->mutex and
    // must return, and an unrelated read must run to completion. Both would
    // deadlock behind the poll if the lock were held across the call.
    CHECK(has(module.cancel(jobId), "\"cancel_requested\":true"));
    CHECK(statusIs(startAndWait(module, module.getFaucetInfo()), "succeeded"));
    CHECK(!pollReturned.load());

    MockLezFaucetFfi::releasePhase();
    polling.join();
    CHECK(pollReturned.load());
    MockLezFaucetFfi::releaseDrop();
    CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));
}

void shutdownJoinsWorkersBeforeDestroyingTheHandle()
{
    // API_LOCK §A1.7.1. Destroying the client while a worker is inside
    // `lez_faucet_request_drop` is a use-after-free.
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    auto module = std::make_unique<LezFaucetModule>();
    configure(*module);
    CHECK(has(module->requestDrop(kAddress, kKey), "\"ok\":true"));
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    std::atomic<bool> finished{false};
    std::thread stopping([&]() {
        const std::string stopped = module->shutdown();
        CHECK(has(stopped, "\"stopped\":true"));
        finished = true;
    });

    // Shutdown asks Rust to stop, then waits: the worker is still in the FFI,
    // so the handle must not have been destroyed yet.
    CHECK(MockLezFaucetFfi::waitForCancelCall(1000));
    CHECK(!finished.load());
    CHECK(MockLezFaucetFfi::destroyCalls() == 0);

    MockLezFaucetFfi::releaseDrop();
    stopping.join();
    CHECK(finished.load());
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
    CHECK(!MockLezFaucetFfi::destroyWhileCallActive());

    // After shutdown the handle is gone, so nothing new may start. The refusal
    // is `not_configured` and not `module_stopping`: the module is quiescent
    // and reconfigurable, not dying. See
    // `shutdownIsReversibleSoReopeningTheAppWorks` below.
    CHECK(codeIs(module->getFaucetInfo(), "not_configured"));
    CHECK(codeIs(module->requestDrop(kAddress, kOtherKey), "not_configured"));
    module.reset();
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
    CHECK(!MockLezFaucetFfi::destroyWhileCallActive());
}

void shutdownIsReversibleSoReopeningTheAppWorks()
{
    // Basecamp calls `shutdown` when the UI window closes, then keeps this same
    // core module instance loaded and calls `configure` again when the user
    // reopens the app. Shipped 0.3.0 and 0.3.1 latched `m_stopping` inside
    // `joinAllWorkers` and never cleared it, so the second `configure` answered
    // `module_stopping` and the faucet stayed dead until Basecamp itself was
    // quit. Observed in the wild on 2026-08-19: close at 14:43:10, reopen at
    // 14:43:21, "The faucet is shutting down".
    MockLezFaucetFfi::reset();
    auto module = std::make_unique<LezFaucetModule>();
    configure(*module);
    CHECK(statusIs(startAndWait(*module, module->getFaucetInfo()), "succeeded"));

    CHECK(has(module->shutdown(), "\"stopped\":true"));
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);

    // The reopen. This is the assertion the shipped builds would have failed.
    configure(*module);
    CHECK(statusIs(startAndWait(*module, module->getFaucetInfo()), "succeeded"));
    CHECK(has(module->requestDrop(kAddress, kOtherKey), "\"ok\":true"));

    // A second shutdown is equally reversible. `already_stopping` reports
    // `false` here precisely *because* the first one re-armed: the field means
    // "another shutdown is in flight right now", not "this module was stopped
    // once before". That `false` is the regression guard — on the 0.3.1 source
    // it reads `true`, because the flag was never cleared.
    const std::string second = module->shutdown();
    CHECK(has(second, "\"stopped\":true"));
    CHECK(has(second, "\"already_stopping\":false"));
    configure(*module);
    CHECK(statusIs(startAndWait(*module, module->getFaucetInfo()), "succeeded"));

    module.reset();
    CHECK(!MockLezFaucetFfi::destroyWhileCallActive());
}

void theDestructorJoinsWorkersToo()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropWaitsForRelease(true);
    auto module = std::make_unique<LezFaucetModule>();
    configure(*module);
    CHECK(has(module->requestDrop(kAddress, kKey), "\"ok\":true"));
    CHECK(MockLezFaucetFfi::waitForDropStart(1000));

    std::atomic<bool> destroyed{false};
    std::thread destroying([&]() {
        module.reset();
        destroyed = true;
    });
    CHECK(MockLezFaucetFfi::waitForCancelCall(1000));
    CHECK(!destroyed.load());
    CHECK(MockLezFaucetFfi::destroyCalls() == 0);

    MockLezFaucetFfi::releaseDrop();
    destroying.join();
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
    CHECK(!MockLezFaucetFfi::destroyWhileCallActive());
}

void inputsAreValidatedByTheModuleItself()
{
    // API_LOCK §A1.3: Rust validates too, but the module must not trust QML.
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);

    const std::string tooLong(65, '1');
    const std::string embeddedNul = std::string("Public/Cbg\0R", 12);
    for (const std::string& address : {
             std::string(),
             std::string("   "),
             tooLong,
             std::string("Public/") + tooLong,
             embeddedNul,
             std::string("Public/Cbg\nR"),
             std::string("Private/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r"),
             std::string("Shielded/CbgR"),
         }) {
        CHECK(codeIs(module.inspectRecipient(address), "invalid_public_account_id"));
        CHECK(codeIs(module.requestDrop(address, kKey), "invalid_public_account_id"));
    }
    CHECK(MockLezFaucetFfi::inspectCalls() == 0);
    CHECK(MockLezFaucetFfi::dropCalls() == 0);

    for (const char* key : {
             "",
             "not-a-uuid",
             "3F2504E0-4F89-41D3-9A0C-0305E82C3301",
             "3f2504e0-4f89-41d3-9a0c-0305e82c330",
             "3f2504e0-4f89-41d3-9a0c-0305e82c33011",
             "3f2504e04f8941d39a0c0305e82c3301",
             "3f2504e0_4f89-41d3-9a0c-0305e82c3301",
             "3f2504e0-4f89-41d3-9a0c-0305e82c330g",
         }) {
        CHECK(codeIs(module.requestDrop(kAddress, key), "invalid_request_key"));
    }
    CHECK(MockLezFaucetFfi::dropCalls() == 0);

    // A bare base58 id and the `Public/` form are both accepted.
    CHECK(has(module.inspectRecipient("CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r"),
        "\"ok\":true"));
    CHECK(has(module.inspectRecipient(kAddress), "\"ok\":true"));
}

void operationsBeforeConfigurationAreStructured()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    CHECK(codeIs(module.getFaucetInfo(), "not_configured"));
    CHECK(codeIs(module.inspectRecipient(kAddress), "not_configured"));
    CHECK(codeIs(module.requestDrop(kAddress, kKey), "not_configured"));
    CHECK(codeIs(module.jobStatus("faucet_info-1"), "unknown_job"));
    CHECK(MockLezFaucetFfi::infoCalls() == 0);
}

void exactDecimalsSurviveAboveJavaScriptSafeInteger()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setInfoResponse(
        "{\"ok\":true,\"result\":{\"prize_amount\":\"150\","
        "\"pool_balance\":\"340282366920938463463374607431768211455\","
        "\"claims_remaining\":\"2268549112806256423089164049545121409\"}}");
    LezFaucetModule module;
    configure(module);

    const std::string info = startAndWait(module, module.getFaucetInfo());
    CHECK(has(info, "\"pool_balance\":\"340282366920938463463374607431768211455\""));
    CHECK(has(info, "\"claims_remaining\":\"2268549112806256423089164049545121409\""));

    const std::string inspection = startAndWait(module, module.inspectRecipient(kAddress));
    CHECK(has(inspection, "\"balance\":\"9007199254740993\""));

    const std::string receipt = startAndWait(module, module.requestDrop(kAddress, kKey));
    CHECK(has(receipt, "\"balance_before\":\"9007199254740993\""));
    CHECK(has(receipt, "\"balance_after\":\"9007199254741143\""));
}

void everyFfiStringIsFreedExactlyOnce()
{
    MockLezFaucetFfi::reset();
    {
        LezFaucetModule module;
        configure(module);
        startAndWait(module, module.getFaucetInfo());
        startAndWait(module, module.inspectRecipient(kAddress));
        startAndWait(module, module.requestDrop(kAddress, kKey));
    }
    // One string each for configure/info/inspect/drop, plus one per phase poll
    // made while the drop was running. However many there were, every one of
    // them is freed exactly once.
    CHECK(MockLezFaucetFfi::stringAllocations() >= 4);
    CHECK(MockLezFaucetFfi::stringFreeCalls() == MockLezFaucetFfi::stringAllocations());
}

void retainedJobsAreBoundedUntilAcknowledged()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);

    std::vector<std::string> retained;
    retained.reserve(128);
    for (int iteration = 0; iteration < 128; ++iteration) {
        const std::string jobId = stringField(module.getFaucetInfo(), "job_id");
        CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));
        retained.push_back(jobId);
    }
    CHECK(codeIs(module.getFaucetInfo(), "job_limit_reached"));
    for (const std::string& jobId : retained) {
        CHECK(has(module.jobResultAck(jobId), "\"reaped\":true"));
    }
    CHECK(has(module.getFaucetInfo(), "\"ok\":true"));
}

void acknowledgedJobsAreReapedUnderStress()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);
    for (int iteration = 0; iteration < 512; ++iteration) {
        const std::string jobId = stringField(module.inspectRecipient(kAddress), "job_id");
        CHECK(statusIs(waitForTerminal(module, jobId), "succeeded"));
        CHECK(has(module.jobStatus(jobId), "\"eligibility\":\"eligible\""));
        CHECK(has(module.jobResultAck(jobId), "\"reaped\":true"));
        CHECK(codeIs(module.jobStatus(jobId), "unknown_job"));
    }
}

void statusAndAcknowledgementAreConcurrencySafe()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    configure(module);
    const std::string jobId = stringField(module.inspectRecipient(kAddress), "job_id");
    waitForTerminal(module, jobId);

    std::atomic<bool> go{false};
    std::mutex resultsMutex;
    std::vector<std::string> results;
    std::vector<std::thread> callers;
    for (int index = 0; index < 16; ++index) {
        callers.emplace_back([&]() {
            while (!go.load()) {
                std::this_thread::yield();
            }
            const std::string result = module.jobStatus(jobId);
            std::lock_guard<std::mutex> lock(resultsMutex);
            results.push_back(result);
        });
    }
    callers.emplace_back([&]() {
        while (!go.load()) {
            std::this_thread::yield();
        }
        const std::string result = module.jobResultAck(jobId);
        std::lock_guard<std::mutex> lock(resultsMutex);
        results.push_back(result);
    });
    go.store(true);
    for (std::thread& caller : callers) {
        caller.join();
    }
    CHECK(results.size() == 17);
    for (const std::string& result : results) {
        CHECK(has(result, "\"ok\":true") || codeIs(result, "unknown_job"));
    }
    CHECK(codeIs(module.jobStatus(jobId), "unknown_job"));
}

void concurrentDuplicateKeysProduceAtMostOneDrop()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setDropDelayMs(20);
    LezFaucetModule module;
    configure(module);

    std::atomic<bool> go{false};
    std::vector<std::thread> callers;
    for (int index = 0; index < 8; ++index) {
        callers.emplace_back([&]() {
            while (!go.load()) {
                std::this_thread::yield();
            }
            module.requestDrop(kAddress, kKey);
        });
    }
    go.store(true);
    for (std::thread& caller : callers) {
        caller.join();
    }
    // Eight callers, one key: exactly one claim may ever reach the chain.
    CHECK(MockLezFaucetFfi::dropCalls() == 1);
}

void thePublicSurfaceCarriesNoWalletOrSecretApi()
{
    // API_LOCK §2 requires this to be asserted statically. Comment lines are
    // stripped first: prose may say these words in order to say they are gone.
    for (const char* path : {MODULE_HEADER_PATH, MODULE_SOURCE_PATH}) {
        std::istringstream source(readFile(path));
        std::string body;
        std::string line;
        while (std::getline(source, line)) {
            const std::size_t start = line.find_first_not_of(" \t");
            const std::string trimmed = start == std::string::npos ? "" : line.substr(start);
            if (trimmed.rfind("//", 0) == 0 || trimmed.rfind("*", 0) == 0
                || trimmed.rfind("/*", 0) == 0) {
                continue;
            }
            body += line;
            body += '\n';
        }
        for (const char* forbidden : {
                 "password", "mnemonic", "recovery", "secureClear", "claimUntil", "claim_until",
                 "target_balance", "createAndInitializeAccount", "verifyFingerprint",
                 "claimOnce", "lez_faucet_create", "lez_faucet_open", "lez_faucet_destroy",
                 "FaucetHandle", "std::filesystem", "fstream", "ofstream",
             }) {
            if (body.find(forbidden) != std::string::npos) {
                std::cerr << path << " must not mention " << forbidden << '\n';
                std::abort();
            }
        }
    }
}

void metadataAndVersionAgree()
{
    // Two independent copies of the same version that drift silently unless
    // something compares them.
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    CHECK(module.name() == "lez_faucet");
    const std::string version = module.version();
    const std::string metadata = readFile(MODULE_METADATA_PATH);
    CHECK(has(metadata, "\"version\": \"" + version + "\""));
    CHECK(has(metadata, "\"name\": \"lez_faucet\""));
}

} // namespace

int main()
{
    configureValidatesAndIsRejectedWhileAJobIsLive();
    startersReturnImmediatelyUnderASlowFfi();
    readsDoNotQueueBehindADrop();
    terminalResultsReplayUntilAcknowledged();
    nonTerminalAcknowledgementIsStructured();
    aRepeatedRequestKeyReplaysTheOriginalJob();
    tombstonesSurviveJobReaping();
    anErrorMessageShapedLikeSuccessIsStillAnError();
    malformedFfiPayloadsFailClosed();
    anUnknownOutcomeThatMentionsCancellingIsNotCancelled();
    cancellationBeforeSubmissionIsCancelledAndTokenScoped();
    aQueuedJobCancelledBeforeItStartsNeverReachesTheFfi();
    aRunningDropReportsALivePhaseThatMovesAndSurvivesTermination();
    theOutcomePhaseOverridesTheLastPolledPhase();
    phasePollingNeverBlocksAConcurrentCaller();
    shutdownJoinsWorkersBeforeDestroyingTheHandle();
    shutdownIsReversibleSoReopeningTheAppWorks();
    theDestructorJoinsWorkersToo();
    inputsAreValidatedByTheModuleItself();
    operationsBeforeConfigurationAreStructured();
    exactDecimalsSurviveAboveJavaScriptSafeInteger();
    everyFfiStringIsFreedExactlyOnce();
    retainedJobsAreBoundedUntilAcknowledged();
    acknowledgedJobsAreReapedUnderStress();
    statusAndAcknowledgementAreConcurrencySafe();
    concurrentDuplicateKeysProduceAtMostOneDrop();
    thePublicSurfaceCarriesNoWalletOrSecretApi();
    metadataAndVersionAgree();
    std::cout << "lez_faucet module tests passed\n";
    return 0;
}
