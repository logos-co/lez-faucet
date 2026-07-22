#include "lez_faucet_module.h"
#include "mock_lez_faucet_ffi.h"

#include <chrono>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <mutex>
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

bool hasStatus(const std::string& json, const std::string& status)
{
    return json.find("\"status\":\"" + status + "\"") != std::string::npos;
}

std::string waitForTerminal(LezFaucetModule& module, const std::string& jobId)
{
    for (int attempt = 0; attempt < 500; ++attempt) {
        const std::string status = module.jobStatus(jobId);
        if (hasStatus(status, "completed") || hasStatus(status, "failed")
            || hasStatus(status, "cancelled")) {
            return status;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
    CHECK(false && "job did not reach a terminal state");
    return {};
}

std::string startAndWait(LezFaucetModule& module, const std::string& started)
{
    CHECK(started.find("\"ok\":true") != std::string::npos);
    return waitForTerminal(module, stringField(started, "job_id"));
}

void lifecycleAndAcknowledgedMnemonic()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string started = module.create("config.json", "wallet.json", "https://testnet", "secret");
    const std::string jobId = stringField(started, "job_id");
    const std::string completed = waitForTerminal(module, jobId);
    CHECK(hasStatus(completed, "completed"));
    CHECK(completed.find("alpha beta gamma") != std::string::npos);
    CHECK(module.jobStatus(jobId).find("alpha beta gamma") != std::string::npos);
    const std::string acknowledged = module.jobResultAck(jobId);
    CHECK(acknowledged.find("\"acknowledged\":true") != std::string::npos);
    CHECK(acknowledged.find("\"reaped\":true") != std::string::npos);
    CHECK(module.jobStatus(jobId).find("\"code\":\"unknown_job\"") != std::string::npos);
    CHECK(MockLezFaucetFfi::stringFreeCalls() == 1);

    const std::string destroyed = startAndWait(module, module.destroy());
    CHECK(hasStatus(destroyed, "completed"));
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
}

void resultCannotBeAcknowledgedWhileRunning()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setFingerprintDelayMs(100);
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string started = module.verifyFingerprint();
    const std::string jobId = stringField(started, "job_id");
    CHECK(module.jobResultAck(jobId).find("\"code\":\"job_not_terminal\"") != std::string::npos);
    CHECK(hasStatus(waitForTerminal(module, jobId), "completed"));
}

void operationsNeverBlockCaller()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    MockLezFaucetFfi::setFingerprintDelayMs(120);

    const auto before = std::chrono::steady_clock::now();
    const std::string started = module.verifyFingerprint();
    const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now() - before);
    if (elapsed.count() >= 50) {
        std::cerr << "operation starter blocked for " << elapsed.count() << " ms\n";
        std::abort();
    }
    CHECK(hasStatus(started, "queued") || hasStatus(started, "running"));
    CHECK(hasStatus(waitForTerminal(module, stringField(started, "job_id")), "completed"));
}

void directOperationsReturnNormalizedResultsAndFreeFfiStrings()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string opened = startAndWait(
        module, module.open("config.json", "wallet.json", "https://testnet"));
    CHECK(opened.find("\"result\":{\"opened\":true}") != std::string::npos);

    const std::string initialized = startAndWait(module, module.createAndInitializeAccount());
    CHECK(initialized.find("\"account_id\":\"MockAccount\"") != std::string::npos);

    const std::string balance = startAndWait(module, module.balance("MockAccount"));
    CHECK(balance.find("\"result\":\"0\"") != std::string::npos);

    const std::string claimed = startAndWait(module, module.claimOnce("MockAccount"));
    CHECK(claimed.find("\"balance_after\":\"150\"") != std::string::npos);
    CHECK(MockLezFaucetFfi::stringFreeCalls() == 4);
}

void claimLoopReportsProgressAndUsesAtomicClaims()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string completed = startAndWait(module, module.claimUntilTarget("MockAccount", "1000", 7));
    CHECK(hasStatus(completed, "completed"));
    CHECK(completed.find("\"completed_claims\":7") != std::string::npos);
    CHECK(completed.find("\"final_balance\":\"1050\"") != std::string::npos);
    CHECK(MockLezFaucetFfi::claimCalls() == 7);
    CHECK(MockLezFaucetFfi::rustClaimUntilCalls() == 0);
}

void cancellationWaitsForInflightClaimThenStops()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setClaimDelayMs(100);
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string started = module.claimUntilTarget("MockAccount", "1000", 7);
    const std::string jobId = stringField(started, "job_id");
    for (int attempt = 0; attempt < 100 && MockLezFaucetFfi::claimCalls() == 0; ++attempt) {
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
    CHECK(MockLezFaucetFfi::claimCalls() == 1);
    CHECK(module.cancel(jobId).find("\"cancel_requested\":true") != std::string::npos);
    CHECK(MockLezFaucetFfi::cancelCalls() == 1);
    const std::string cancelled = waitForTerminal(module, jobId);
    CHECK(hasStatus(cancelled, "cancelled"));
    CHECK(cancelled.find("between confirmed claims") != std::string::npos);
    CHECK(MockLezFaucetFfi::claimCalls() == 1);
    CHECK(MockLezFaucetFfi::balance() == 150);
}

void cancellationCanStopBeforeSubmission()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setClaimDelayMs(100);
    MockLezFaucetFfi::setCancelStopsClaim(true);
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string started = module.claimOnce("MockAccount");
    const std::string jobId = stringField(started, "job_id");
    for (int attempt = 0; attempt < 100 && MockLezFaucetFfi::claimCalls() == 0; ++attempt) {
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
    module.cancel(jobId);
    const std::string cancelled = waitForTerminal(module, jobId);
    CHECK(hasStatus(cancelled, "cancelled"));
    CHECK(cancelled.find("Piñata solve cancelled") != std::string::npos);
    CHECK(MockLezFaucetFfi::balance() == 0);
}

void shutdownCancelsActiveSolveBeforeJoining()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setCancelStopsClaim(true);
    MockLezFaucetFfi::setClaimWaitsForCancel(true);
    auto module = std::make_unique<LezFaucetModule>();
    startAndWait(*module, module->open("config.json", "wallet.json", "https://testnet"));
    const std::string started = module->claimOnce("MockAccount");
    CHECK(started.find("\"ok\":true") != std::string::npos);
    CHECK(MockLezFaucetFfi::waitForClaimStart(500));

    const auto before = std::chrono::steady_clock::now();
    module.reset();
    const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now() - before);

    CHECK(elapsed.count() < 500);
    CHECK(MockLezFaucetFfi::cancelCalls() == 1);
    CHECK(MockLezFaucetFfi::balance() == 0);
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
    CHECK(!MockLezFaucetFfi::destroyWhileClaimActive());
}

void shutdownWaitsForSubmittedClaimReconciliation()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setClaimWaitsForReconciliationRelease(true);
    auto module = std::make_unique<LezFaucetModule>();
    startAndWait(*module, module->open("config.json", "wallet.json", "https://testnet"));
    const std::string started = module->claimOnce("MockAccount");
    CHECK(started.find("\"ok\":true") != std::string::npos);
    CHECK(MockLezFaucetFfi::waitForClaimStart(500));

    std::atomic<bool> shutdownFinished{false};
    std::thread shutdown([&]() {
        module.reset();
        shutdownFinished = true;
    });
    CHECK(MockLezFaucetFfi::waitForCancelCall(500));
    CHECK(!shutdownFinished.load());
    CHECK(MockLezFaucetFfi::destroyCalls() == 0);
    CHECK(!MockLezFaucetFfi::destroyWhileClaimActive());

    MockLezFaucetFfi::releaseClaimReconciliation();
    shutdown.join();
    CHECK(shutdownFinished.load());
    CHECK(MockLezFaucetFfi::cancelCalls() == 1);
    CHECK(MockLezFaucetFfi::balance() == 150);
    CHECK(MockLezFaucetFfi::destroyCalls() == 1);
    CHECK(!MockLezFaucetFfi::destroyWhileClaimActive());
}

void errorsAreStructured()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string failed = startAndWait(module, module.balance("MockAccount"));
    CHECK(hasStatus(failed, "failed"));
    CHECK(failed.find("\"code\":\"wallet_not_open\"") != std::string::npos);
    CHECK(module.cancel("missing").find("\"code\":\"unknown_job\"") != std::string::npos);
}

void unknownClaimOutcomeRemainsStructured()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setClaimOutcomeUnknown(true);
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string failed = startAndWait(module, module.claimOnce("MockAccount"));
    CHECK(hasStatus(failed, "failed"));
    CHECK(failed.find("\"code\":\"outcome_unknown\"") != std::string::npos);
    CHECK(failed.find("\"outcome\":\"unknown\"") != std::string::npos);
    CHECK(failed.find("\"tx_hash\":null") != std::string::npos);
}

void exactDecimalsSurviveAboveJavaScriptSafeInteger()
{
    MockLezFaucetFfi::reset();
    constexpr uint64_t initial = 9'007'199'254'740'993ULL;
    MockLezFaucetFfi::setBalance(initial);
    LezFaucetModule module;
    const std::string openStarted = module.open("config.json", "wallet.json", "https://testnet");
    const std::string openId = stringField(openStarted, "job_id");
    CHECK(hasStatus(waitForTerminal(module, openId), "completed"));
    CHECK(module.jobResultAck(openId).find("\"reaped\":true") != std::string::npos);

    const std::string balanceStarted = module.balance("MockAccount");
    const std::string balanceId = stringField(balanceStarted, "job_id");
    const std::string balance = waitForTerminal(module, balanceId);
    CHECK(balance.find("\"result\":\"9007199254740993\"") != std::string::npos);
    CHECK(module.jobResultAck(balanceId).find("\"reaped\":true") != std::string::npos);

    const std::string target = "9007199254741293";
    const std::string started = module.claimUntilTarget("MockAccount", target, 2);
    const std::string jobId = stringField(started, "job_id");
    const std::string completed = waitForTerminal(module, jobId);
    CHECK(hasStatus(completed, "completed"));
    CHECK(completed.find("\"initial_balance\":\"9007199254740993\"") != std::string::npos);
    CHECK(completed.find("\"final_balance\":\"9007199254741293\"") != std::string::npos);
    CHECK(completed.find("\"required_claims\":2") != std::string::npos);
    CHECK(MockLezFaucetFfi::claimCalls() == 2);
}

void claimRunsHaveAHardBound()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));

    const std::string excessiveMax = startAndWait(
        module, module.claimUntilTarget("MockAccount", "150", 101));
    CHECK(hasStatus(excessiveMax, "failed"));
    CHECK(excessiveMax.find("\"code\":\"invalid_max_claims\"") != std::string::npos);

    const std::string excessiveTarget = startAndWait(
        module, module.claimUntilTarget("MockAccount", "15150", 100));
    CHECK(hasStatus(excessiveTarget, "failed"));
    CHECK(excessiveTarget.find("\"code\":\"claim_limit_exceeded\"") != std::string::npos);
    CHECK(MockLezFaucetFfi::claimCalls() == 0);
}

void acknowledgedJobsAreReapedUnderStress()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string openStarted = module.open("config.json", "wallet.json", "https://testnet");
    const std::string openId = stringField(openStarted, "job_id");
    waitForTerminal(module, openId);
    module.jobResultAck(openId);

    for (int iteration = 0; iteration < 512; ++iteration) {
        const std::string started = module.balance("MockAccount");
        const std::string jobId = stringField(started, "job_id");
        CHECK(hasStatus(waitForTerminal(module, jobId), "completed"));
        CHECK(module.jobStatus(jobId).find("\"result\":\"0\"") != std::string::npos);
        CHECK(module.jobResultAck(jobId).find("\"reaped\":true") != std::string::npos);
        CHECK(module.jobStatus(jobId).find("\"code\":\"unknown_job\"") != std::string::npos);
    }
}

void retainedJobsAreBoundedUntilAcknowledged()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string openStarted = module.open("config.json", "wallet.json", "https://testnet");
    const std::string openId = stringField(openStarted, "job_id");
    waitForTerminal(module, openId);
    module.jobResultAck(openId);

    std::vector<std::string> retained;
    retained.reserve(128);
    for (int iteration = 0; iteration < 128; ++iteration) {
        const std::string started = module.balance("MockAccount");
        const std::string jobId = stringField(started, "job_id");
        CHECK(hasStatus(waitForTerminal(module, jobId), "completed"));
        retained.push_back(jobId);
    }
    CHECK(module.balance("MockAccount").find("\"code\":\"job_limit_reached\"")
        != std::string::npos);
    for (const std::string& jobId : retained) {
        CHECK(module.jobResultAck(jobId).find("\"reaped\":true") != std::string::npos);
    }
    CHECK(module.balance("MockAccount").find("\"ok\":true") != std::string::npos);
}

void statusAndAcknowledgementAreConcurrencySafe()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string started = module.create(
        "config.json", "wallet.json", "https://testnet", "secret");
    const std::string jobId = stringField(started, "job_id");
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
        CHECK(result.find("\"ok\":true") != std::string::npos
            || result.find("\"code\":\"unknown_job\"") != std::string::npos);
    }
    CHECK(module.jobStatus(jobId).find("\"code\":\"unknown_job\"") != std::string::npos);
}

} // namespace

int main()
{
    lifecycleAndAcknowledgedMnemonic();
    resultCannotBeAcknowledgedWhileRunning();
    operationsNeverBlockCaller();
    directOperationsReturnNormalizedResultsAndFreeFfiStrings();
    claimLoopReportsProgressAndUsesAtomicClaims();
    cancellationWaitsForInflightClaimThenStops();
    cancellationCanStopBeforeSubmission();
    shutdownCancelsActiveSolveBeforeJoining();
    shutdownWaitsForSubmittedClaimReconciliation();
    errorsAreStructured();
    unknownClaimOutcomeRemainsStructured();
    exactDecimalsSurviveAboveJavaScriptSafeInteger();
    claimRunsHaveAHardBound();
    acknowledgedJobsAreReapedUnderStress();
    retainedJobsAreBoundedUntilAcknowledged();
    statusAndAcknowledgementAreConcurrencySafe();
    std::cout << "lez_faucet module tests passed\n";
    return 0;
}
