#include "lez_faucet_module.h"
#include "mock_lez_faucet_ffi.h"

#include <cassert>
#include <chrono>
#include <iostream>
#include <string>
#include <thread>

namespace {

std::string stringField(const std::string& json, const std::string& key)
{
    const std::string needle = "\"" + key + "\":\"";
    const auto start = json.find(needle);
    assert(start != std::string::npos);
    const auto valueStart = start + needle.size();
    const auto end = json.find('"', valueStart);
    assert(end != std::string::npos);
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
    assert(false && "job did not reach a terminal state");
    return {};
}

std::string startAndWait(LezFaucetModule& module, const std::string& started)
{
    assert(started.find("\"ok\":true") != std::string::npos);
    return waitForTerminal(module, stringField(started, "job_id"));
}

void lifecycleAndAcknowledgedMnemonic()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string started = module.create("config.json", "wallet.json", "https://testnet", "secret");
    const std::string jobId = stringField(started, "job_id");
    const std::string completed = waitForTerminal(module, jobId);
    assert(hasStatus(completed, "completed"));
    assert(completed.find("alpha beta gamma") != std::string::npos);
    assert(module.jobStatus(jobId).find("alpha beta gamma") != std::string::npos);
    const std::string acknowledged = module.jobResultAck(jobId);
    assert(acknowledged.find("\"acknowledged\":true") != std::string::npos);
    assert(module.jobStatus(jobId).find("alpha beta gamma") == std::string::npos);
    assert(module.jobStatus(jobId).find("\"result\":null") != std::string::npos);
    assert(MockLezFaucetFfi::stringFreeCalls() == 1);

    const std::string destroyed = startAndWait(module, module.destroy());
    assert(hasStatus(destroyed, "completed"));
    assert(MockLezFaucetFfi::destroyCalls() == 1);
}

void resultCannotBeAcknowledgedWhileRunning()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setFingerprintDelayMs(100);
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string started = module.verifyFingerprint();
    const std::string jobId = stringField(started, "job_id");
    assert(module.jobResultAck(jobId).find("\"code\":\"job_not_terminal\"") != std::string::npos);
    assert(hasStatus(waitForTerminal(module, jobId), "completed"));
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
    assert(elapsed.count() < 50);
    assert(hasStatus(started, "queued") || hasStatus(started, "running"));
    assert(hasStatus(waitForTerminal(module, stringField(started, "job_id")), "completed"));
}

void directOperationsReturnNormalizedResultsAndFreeFfiStrings()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string opened = startAndWait(
        module, module.open("config.json", "wallet.json", "https://testnet"));
    assert(opened.find("\"result\":{\"opened\":true}") != std::string::npos);

    const std::string initialized = startAndWait(module, module.createAndInitializeAccount());
    assert(initialized.find("\"account_id\":\"MockAccount\"") != std::string::npos);

    const std::string balance = startAndWait(module, module.balance("MockAccount"));
    assert(balance.find("\"result\":0") != std::string::npos);

    const std::string claimed = startAndWait(module, module.claimOnce("MockAccount"));
    assert(claimed.find("\"balance_after\":150") != std::string::npos);
    assert(MockLezFaucetFfi::stringFreeCalls() == 4);
}

void claimLoopReportsProgressAndUsesAtomicClaims()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string completed = startAndWait(module, module.claimUntilTarget("MockAccount", "1000", 7));
    assert(hasStatus(completed, "completed"));
    assert(completed.find("\"completed_claims\":7") != std::string::npos);
    assert(completed.find("\"final_balance\":\"1050\"") != std::string::npos);
    assert(MockLezFaucetFfi::claimCalls() == 7);
    assert(MockLezFaucetFfi::rustClaimUntilCalls() == 0);
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
    assert(MockLezFaucetFfi::claimCalls() == 1);
    assert(module.cancel(jobId).find("\"cancel_requested\":true") != std::string::npos);
    assert(MockLezFaucetFfi::cancelCalls() == 1);
    const std::string cancelled = waitForTerminal(module, jobId);
    assert(hasStatus(cancelled, "cancelled"));
    assert(cancelled.find("between confirmed claims") != std::string::npos);
    assert(MockLezFaucetFfi::claimCalls() == 1);
    assert(MockLezFaucetFfi::balance() == 150);
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
    assert(hasStatus(cancelled, "cancelled"));
    assert(cancelled.find("Piñata solve cancelled") != std::string::npos);
    assert(MockLezFaucetFfi::balance() == 0);
}

void errorsAreStructured()
{
    MockLezFaucetFfi::reset();
    LezFaucetModule module;
    const std::string failed = startAndWait(module, module.balance("MockAccount"));
    assert(hasStatus(failed, "failed"));
    assert(failed.find("\"code\":\"wallet_not_open\"") != std::string::npos);
    assert(module.cancel("missing").find("\"code\":\"unknown_job\"") != std::string::npos);
}

void unknownClaimOutcomeRemainsStructured()
{
    MockLezFaucetFfi::reset();
    MockLezFaucetFfi::setClaimOutcomeUnknown(true);
    LezFaucetModule module;
    startAndWait(module, module.open("config.json", "wallet.json", "https://testnet"));
    const std::string failed = startAndWait(module, module.claimOnce("MockAccount"));
    assert(hasStatus(failed, "failed"));
    assert(failed.find("\"code\":\"outcome_unknown\"") != std::string::npos);
    assert(failed.find("\"outcome\":\"unknown\"") != std::string::npos);
    assert(failed.find("\"tx_hash\":null") != std::string::npos);
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
    errorsAreStructured();
    unknownClaimOutcomeRemainsStructured();
    std::cout << "lez_faucet module tests passed\n";
    return 0;
}
