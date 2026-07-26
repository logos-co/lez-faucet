#include "mock_lez_faucet_ffi.h"

extern "C" {
#include <lez_faucet_ffi.h>
}

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <string>
#include <thread>

struct FaucetHandle {
    int marker;
};

namespace {

FaucetHandle g_handle{42};
std::atomic<uint64_t> g_balance{0};
std::atomic<bool> g_balanceRecipientError{false};
std::atomic<int> g_claimDelayMs{0};
std::atomic<int> g_fingerprintDelayMs{0};
std::atomic<bool> g_cancelStopsClaim{false};
std::atomic<bool> g_claimWaitsForCancel{false};
std::atomic<bool> g_claimWaitsForReconciliationRelease{false};
std::atomic<bool> g_claimOutcomeUnknown{false};
std::atomic<bool> g_cancelled{false};
std::atomic<bool> g_claimActive{false};
std::atomic<bool> g_destroyWhileClaimActive{false};
std::atomic<bool> g_reconciliationReleased{false};
std::atomic<int> g_claimCalls{0};
std::atomic<int> g_stringFreeCalls{0};
std::atomic<int> g_destroyCalls{0};
std::atomic<int> g_rustClaimUntilCalls{0};
std::atomic<int> g_cancelCalls{0};
std::string g_lastBalanceAccountId;
std::string g_lastClaimAccountId;
std::mutex g_eventMutex;
std::condition_variable g_eventCondition;

char* copyString(const std::string& value)
{
    auto* copy = static_cast<char*>(std::malloc(value.size() + 1));
    std::memcpy(copy, value.c_str(), value.size() + 1);
    return copy;
}

} // namespace

namespace MockLezFaucetFfi {

void reset()
{
    g_balance = 0;
    g_balanceRecipientError = false;
    g_claimDelayMs = 0;
    g_fingerprintDelayMs = 0;
    g_cancelStopsClaim = false;
    g_claimWaitsForCancel = false;
    g_claimWaitsForReconciliationRelease = false;
    g_claimOutcomeUnknown = false;
    g_cancelled = false;
    g_claimActive = false;
    g_destroyWhileClaimActive = false;
    g_reconciliationReleased = false;
    g_claimCalls = 0;
    g_stringFreeCalls = 0;
    g_destroyCalls = 0;
    g_rustClaimUntilCalls = 0;
    g_cancelCalls = 0;
    g_lastBalanceAccountId.clear();
    g_lastClaimAccountId.clear();
}

void setBalance(uint64_t value) { g_balance = value; }
void setBalanceRecipientError(bool value) { g_balanceRecipientError = value; }
uint64_t balance() { return g_balance.load(); }
void setClaimDelayMs(int value) { g_claimDelayMs = value; }
void setFingerprintDelayMs(int value) { g_fingerprintDelayMs = value; }
void setCancelStopsClaim(bool value) { g_cancelStopsClaim = value; }
void setClaimWaitsForCancel(bool value) { g_claimWaitsForCancel = value; }
void setClaimWaitsForReconciliationRelease(bool value)
{
    g_claimWaitsForReconciliationRelease = value;
}
void setClaimOutcomeUnknown(bool value) { g_claimOutcomeUnknown = value; }
bool waitForClaimStart(int timeoutMs)
{
    std::unique_lock<std::mutex> lock(g_eventMutex);
    return g_eventCondition.wait_for(lock, std::chrono::milliseconds(timeoutMs), [] {
        return g_claimActive.load();
    });
}
bool waitForCancelCall(int timeoutMs)
{
    std::unique_lock<std::mutex> lock(g_eventMutex);
    return g_eventCondition.wait_for(lock, std::chrono::milliseconds(timeoutMs), [] {
        return g_cancelCalls.load() > 0;
    });
}
void releaseClaimReconciliation()
{
    g_reconciliationReleased = true;
    g_eventCondition.notify_all();
}
int claimCalls() { return g_claimCalls.load(); }
int stringFreeCalls() { return g_stringFreeCalls.load(); }
int destroyCalls() { return g_destroyCalls.load(); }
int rustClaimUntilCalls() { return g_rustClaimUntilCalls.load(); }
int cancelCalls() { return g_cancelCalls.load(); }
bool destroyWhileClaimActive() { return g_destroyWhileClaimActive.load(); }
std::string lastBalanceAccountId()
{
    std::lock_guard<std::mutex> lock(g_eventMutex);
    return g_lastBalanceAccountId;
}
std::string lastClaimAccountId()
{
    std::lock_guard<std::mutex> lock(g_eventMutex);
    return g_lastClaimAccountId;
}

} // namespace MockLezFaucetFfi

extern "C" {

FfiCreateOutput lez_faucet_create(const char*, const char*, const char*, const char*)
{
    return FfiCreateOutput{
        &g_handle,
        copyString("{\"ok\":true,\"mnemonic\":\"alpha beta gamma\",\"security_warning\":\"testnet only\"}")
    };
}

FfiCreateOutput lez_faucet_open(const char*, const char*, const char*)
{
    return FfiCreateOutput{
        &g_handle,
        copyString("{\"ok\":true,\"result\":{\"opened\":true}}")
    };
}

void lez_faucet_destroy(FaucetHandle*)
{
    if (g_claimActive.load()) {
        g_destroyWhileClaimActive = true;
    }
    ++g_destroyCalls;
}

void lez_faucet_string_free(char* value)
{
    ++g_stringFreeCalls;
    std::free(value);
}

void lez_faucet_cancel(FaucetHandle*)
{
    ++g_cancelCalls;
    g_cancelled = true;
    g_eventCondition.notify_all();
}

char* lez_faucet_verify_fingerprint(FaucetHandle*)
{
    std::this_thread::sleep_for(std::chrono::milliseconds(g_fingerprintDelayMs.load()));
    return copyString("{\"ok\":true,\"result\":{\"pinata\":\"66f6\"}}");
}

char* lez_faucet_create_and_initialize_account(FaucetHandle*)
{
    return copyString("{\"ok\":true,\"result\":{\"account_id\":\"MockAccount\",\"balance\":0}}");
}

char* lez_faucet_get_balance(FaucetHandle*, const char* accountId)
{
    {
        std::lock_guard<std::mutex> lock(g_eventMutex);
        g_lastBalanceAccountId = accountId == nullptr ? "" : accountId;
    }
    if (g_balanceRecipientError.load()) {
        return copyString(
            "{\"ok\":false,\"error\":{\"code\":\"recipient_uninitialized\","
            "\"message\":\"Public/MockAccount is valid but uninitialized\","
            "\"account_id\":\"Public/MockAccount\","
            "\"initialization_command\":\"wallet auth-transfer init --account-id Public/MockAccount\"}}");
    }
    return copyString("{\"ok\":true,\"result\":" + std::to_string(g_balance.load()) + "}");
}

char* lez_faucet_claim_once(FaucetHandle*, const char* accountId)
{
    {
        std::lock_guard<std::mutex> lock(g_eventMutex);
        g_lastClaimAccountId = accountId == nullptr ? "" : accountId;
    }
    g_claimActive = true;
    ++g_claimCalls;
    g_eventCondition.notify_all();
    if (g_claimWaitsForCancel.load()) {
        std::unique_lock<std::mutex> lock(g_eventMutex);
        g_eventCondition.wait_for(lock, std::chrono::seconds(2), [] {
            return g_cancelled.load();
        });
    }
    if (g_claimWaitsForReconciliationRelease.load()) {
        std::unique_lock<std::mutex> lock(g_eventMutex);
        g_eventCondition.wait_for(lock, std::chrono::seconds(2), [] {
            return g_reconciliationReleased.load();
        });
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(g_claimDelayMs.load()));
    if (g_cancelStopsClaim.load() && g_cancelled.exchange(false)) {
        g_claimActive = false;
        g_eventCondition.notify_all();
        return copyString("{\"ok\":false,\"error\":\"Piñata solve cancelled\"}");
    }
    if (g_claimOutcomeUnknown.load()) {
        g_claimActive = false;
        g_eventCondition.notify_all();
        return copyString("{\"ok\":false,\"outcome\":\"unknown\",\"tx_hash\":null,"
            "\"error\":\"timed out reconciling claim outcome\"}");
    }
    const uint64_t before = g_balance.fetch_add(150);
    const uint64_t after = before + 150;
    g_claimActive = false;
    g_eventCondition.notify_all();
    return copyString("{\"ok\":true,\"result\":{\"tx_hash\":\"mock-tx-" + std::to_string(after)
        + "\",\"balance_before\":" + std::to_string(before)
        + ",\"balance_after\":" + std::to_string(after)
        + ",\"stale_challenge_retries\":0}}");
}

char* lez_faucet_claim_until_target(FaucetHandle*, const char*, const char*, size_t,
                                    FfiProgressCallback, void*)
{
    ++g_rustClaimUntilCalls;
    return copyString("{\"ok\":false,\"error\":\"native module must not call Rust claim-until\"}");
}

} // extern "C"
