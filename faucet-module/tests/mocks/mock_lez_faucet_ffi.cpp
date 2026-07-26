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

struct LezFaucetClient {
    int marker;
};

namespace {

LezFaucetClient g_client{42};

std::mutex g_mutex;
std::condition_variable g_condition;

std::atomic<bool> g_clientNewFails{false};
std::atomic<int> g_infoDelayMs{0};
std::atomic<int> g_inspectDelayMs{0};
std::atomic<int> g_dropDelayMs{0};
std::atomic<bool> g_dropWaitsForCancel{false};
std::atomic<bool> g_dropHonoursCancel{false};
std::atomic<bool> g_dropWaitsForRelease{false};
std::atomic<bool> g_dropReleased{false};
std::atomic<bool> g_dropStarted{false};
std::atomic<bool> g_phaseBlocks{false};
std::atomic<bool> g_phaseReleased{false};
std::atomic<bool> g_phaseStarted{false};
std::atomic<int> g_activeCalls{0};
std::atomic<bool> g_destroyWhileCallActive{false};

std::atomic<int> g_clientNewCalls{0};
std::atomic<int> g_destroyCalls{0};
std::atomic<int> g_stringAllocations{0};
std::atomic<int> g_stringFreeCalls{0};
std::atomic<int> g_infoCalls{0};
std::atomic<int> g_inspectCalls{0};
std::atomic<int> g_dropCalls{0};
std::atomic<int> g_cancelCalls{0};
std::atomic<int> g_phaseCalls{0};
std::atomic<uint64_t> g_lastDropToken{0};
std::atomic<uint64_t> g_lastCancelToken{0};
std::atomic<uint64_t> g_lastPhaseToken{0};
std::atomic<uint64_t> g_cancelledToken{0};

// Guarded by g_mutex.
std::string g_lastAddress;
std::string g_lastRequestKey;
std::string g_lastSequencerUrl;
std::string g_infoResponse;
std::string g_inspectResponse;
std::string g_dropResponse;
std::string g_phaseResponse;

constexpr const char* kDefaultInfoResponse =
    "{\"ok\":true,\"result\":{\"network\":\"lez-public-testnet\","
    "\"sequencer_url\":\"https://testnet.lez.logos.co/\","
    "\"pinata_account\":{\"account_id\":\"EfQh\",\"address\":\"Public/EfQh\"},"
    "\"prize_amount\":\"150\",\"pool_balance\":\"1500000\","
    "\"claims_remaining\":\"10000\",\"difficulty_bytes\":3,"
    "\"effective_difficulty_bits\":24,\"can_claim\":true,"
    "\"program_fingerprint\":{\"authenticated_transfer\":\"aa\",\"pinata\":\"bb\"}}}";

constexpr const char* kDefaultInspectResponse =
    "{\"ok\":true,\"result\":{\"recipient\":{\"account_id\":\"CbgR\",\"address\":\"Public/CbgR\"},"
    "\"eligibility\":\"eligible\",\"balance\":\"9007199254740993\","
    "\"program_owner\":\"aa\"}}";

constexpr const char* kDefaultPhaseResponse = "{\"ok\":true,\"result\":{\"phase\":null}}";

constexpr const char* kDefaultDropResponse =
    "{\"ok\":true,\"result\":{\"request_key\":\"3f2504e0-4f89-41d3-9a0c-0305e82c3301\","
    "\"recipient\":{\"account_id\":\"CbgR\",\"address\":\"Public/CbgR\"},"
    "\"amount\":\"150\",\"balance_before\":\"9007199254740993\","
    "\"balance_after\":\"9007199254741143\",\"tx_hash\":\"0f0f\","
    "\"stale_challenge_retries\":0}}";

char* copyString(const std::string& value)
{
    ++g_stringAllocations;
    auto* copy = static_cast<char*>(std::malloc(value.size() + 1));
    std::memcpy(copy, value.c_str(), value.size() + 1);
    return copy;
}

/// Scope guard that records any handle destruction racing a live FFI call.
class ActiveCall {
public:
    ActiveCall() { ++g_activeCalls; }
    ~ActiveCall() { --g_activeCalls; }
    ActiveCall(const ActiveCall&) = delete;
    ActiveCall& operator=(const ActiveCall&) = delete;
};

std::string responseOr(const std::string& configured, const char* fallback)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    return configured.empty() ? std::string(fallback) : configured;
}

} // namespace

namespace MockLezFaucetFfi {

void reset()
{
    std::lock_guard<std::mutex> lock(g_mutex);
    g_clientNewFails = false;
    g_infoDelayMs = 0;
    g_inspectDelayMs = 0;
    g_dropDelayMs = 0;
    g_dropWaitsForCancel = false;
    g_dropHonoursCancel = false;
    g_dropWaitsForRelease = false;
    g_dropReleased = false;
    g_dropStarted = false;
    g_phaseBlocks = false;
    g_phaseReleased = false;
    g_phaseStarted = false;
    g_activeCalls = 0;
    g_destroyWhileCallActive = false;
    g_clientNewCalls = 0;
    g_destroyCalls = 0;
    g_stringAllocations = 0;
    g_stringFreeCalls = 0;
    g_infoCalls = 0;
    g_inspectCalls = 0;
    g_dropCalls = 0;
    g_cancelCalls = 0;
    g_phaseCalls = 0;
    g_lastDropToken = 0;
    g_lastCancelToken = 0;
    g_lastPhaseToken = 0;
    g_cancelledToken = 0;
    g_lastAddress.clear();
    g_lastRequestKey.clear();
    g_lastSequencerUrl.clear();
    g_infoResponse.clear();
    g_inspectResponse.clear();
    g_dropResponse.clear();
    g_phaseResponse.clear();
}

void setClientNewFails(bool value) { g_clientNewFails = value; }
void setInfoDelayMs(int value) { g_infoDelayMs = value; }
void setInspectDelayMs(int value) { g_inspectDelayMs = value; }
void setDropDelayMs(int value) { g_dropDelayMs = value; }

void setInfoResponse(const std::string& json)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    g_infoResponse = json;
}

void setInspectResponse(const std::string& json)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    g_inspectResponse = json;
}

void setDropResponse(const std::string& json)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    g_dropResponse = json;
}

void setDropWaitsForCancel(bool value) { g_dropWaitsForCancel = value; }
void setDropHonoursCancel(bool value) { g_dropHonoursCancel = value; }
void setDropWaitsForRelease(bool value) { g_dropWaitsForRelease = value; }

void setPhaseResponse(const std::string& json)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    g_phaseResponse = json;
}

void setPhaseBlocks(bool value) { g_phaseBlocks = value; }

void releasePhase()
{
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_phaseReleased = true;
    }
    g_condition.notify_all();
}

void releaseDrop()
{
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_dropReleased = true;
    }
    g_condition.notify_all();
}

bool waitForDropStart(int timeoutMs)
{
    std::unique_lock<std::mutex> lock(g_mutex);
    return g_condition.wait_for(lock, std::chrono::milliseconds(timeoutMs),
        [] { return g_dropStarted.load(); });
}

bool waitForCancelCall(int timeoutMs)
{
    std::unique_lock<std::mutex> lock(g_mutex);
    return g_condition.wait_for(lock, std::chrono::milliseconds(timeoutMs),
        [] { return g_cancelCalls.load() > 0; });
}

bool waitForPhaseStart(int timeoutMs)
{
    std::unique_lock<std::mutex> lock(g_mutex);
    return g_condition.wait_for(lock, std::chrono::milliseconds(timeoutMs),
        [] { return g_phaseStarted.load(); });
}

int clientNewCalls() { return g_clientNewCalls.load(); }
int destroyCalls() { return g_destroyCalls.load(); }
int stringAllocations() { return g_stringAllocations.load(); }
int stringFreeCalls() { return g_stringFreeCalls.load(); }
int infoCalls() { return g_infoCalls.load(); }
int inspectCalls() { return g_inspectCalls.load(); }
int dropCalls() { return g_dropCalls.load(); }
int cancelCalls() { return g_cancelCalls.load(); }
int phaseCalls() { return g_phaseCalls.load(); }
uint64_t lastDropToken() { return g_lastDropToken.load(); }
uint64_t lastCancelToken() { return g_lastCancelToken.load(); }
uint64_t lastPhaseToken() { return g_lastPhaseToken.load(); }
bool destroyWhileCallActive() { return g_destroyWhileCallActive.load(); }

std::string lastAddress()
{
    std::lock_guard<std::mutex> lock(g_mutex);
    return g_lastAddress;
}

std::string lastRequestKey()
{
    std::lock_guard<std::mutex> lock(g_mutex);
    return g_lastRequestKey;
}

std::string lastSequencerUrl()
{
    std::lock_guard<std::mutex> lock(g_mutex);
    return g_lastSequencerUrl;
}

} // namespace MockLezFaucetFfi

extern "C" {

LezFaucetClientOutput lez_faucet_client_new(const char* sequencer_url)
{
    ++g_clientNewCalls;
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_lastSequencerUrl = sequencer_url == nullptr ? "" : sequencer_url;
    }
    if (g_clientNewFails.load()) {
        return LezFaucetClientOutput{
            nullptr,
            copyString("{\"ok\":false,\"error\":{\"code\":\"invalid_sequencer_url\","
                       "\"message\":\"That sequencer is not the LEZ public testnet this app is "
                       "built for.\",\"retryable\":false}}")
        };
    }
    return LezFaucetClientOutput{&g_client, copyString("{\"ok\":true}")};
}

void lez_faucet_client_destroy(LezFaucetClient*)
{
    if (g_activeCalls.load() > 0) {
        g_destroyWhileCallActive = true;
    }
    ++g_destroyCalls;
}

void lez_faucet_string_free(char* value)
{
    ++g_stringFreeCalls;
    std::free(value);
}

void lez_faucet_cancel(LezFaucetClient*, uint64_t operation_token)
{
    {
        // Published under the same mutex the waiters use, so a notify can never
        // slip between a waiter's predicate check and its wait.
        std::lock_guard<std::mutex> lock(g_mutex);
        ++g_cancelCalls;
        g_lastCancelToken = operation_token;
        g_cancelledToken = operation_token;
    }
    g_condition.notify_all();
}

char* lez_faucet_current_phase(LezFaucetClient*, uint64_t operation_token)
{
    const ActiveCall active;
    ++g_phaseCalls;
    g_lastPhaseToken = operation_token;
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_phaseStarted = true;
    }
    g_condition.notify_all();
    if (g_phaseBlocks.load()) {
        std::unique_lock<std::mutex> lock(g_mutex);
        g_condition.wait_for(lock, std::chrono::seconds(5), [] { return g_phaseReleased.load(); });
    }
    return copyString(responseOr(g_phaseResponse, kDefaultPhaseResponse));
}

char* lez_faucet_get_info(LezFaucetClient*)
{
    const ActiveCall active;
    ++g_infoCalls;
    std::this_thread::sleep_for(std::chrono::milliseconds(g_infoDelayMs.load()));
    return copyString(responseOr(g_infoResponse, kDefaultInfoResponse));
}

char* lez_faucet_inspect_recipient(LezFaucetClient*, const char* address)
{
    const ActiveCall active;
    ++g_inspectCalls;
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_lastAddress = address == nullptr ? "" : address;
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(g_inspectDelayMs.load()));
    return copyString(responseOr(g_inspectResponse, kDefaultInspectResponse));
}

char* lez_faucet_request_drop(LezFaucetClient*, const char* address, const char* request_key,
                              uint64_t operation_token)
{
    const ActiveCall active;
    ++g_dropCalls;
    g_lastDropToken = operation_token;
    {
        std::lock_guard<std::mutex> lock(g_mutex);
        g_lastAddress = address == nullptr ? "" : address;
        g_lastRequestKey = request_key == nullptr ? "" : request_key;
        g_dropStarted = true;
    }
    g_condition.notify_all();

    if (g_dropWaitsForCancel.load()) {
        std::unique_lock<std::mutex> lock(g_mutex);
        g_condition.wait_for(lock, std::chrono::seconds(2),
            [operation_token] { return g_cancelledToken.load() == operation_token; });
    }
    if (g_dropWaitsForRelease.load()) {
        std::unique_lock<std::mutex> lock(g_mutex);
        g_condition.wait_for(lock, std::chrono::seconds(5), [] { return g_dropReleased.load(); });
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(g_dropDelayMs.load()));

    // Before submission a cancel is honourable, and the real client says so
    // with a structured code.
    if (g_dropHonoursCancel.load() && g_cancelledToken.load() == operation_token) {
        return copyString("{\"ok\":false,\"error\":{\"code\":\"cancelled\",\"message\":\"The "
                          "request was cancelled before anything was submitted.\","
                          "\"retryable\":false,\"phase\":\"solving\"}}");
    }
    return copyString(responseOr(g_dropResponse, kDefaultDropResponse));
}

} // extern "C"
