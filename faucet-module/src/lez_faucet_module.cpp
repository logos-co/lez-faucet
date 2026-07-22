#include "lez_faucet_module.h"

#include <algorithm>
#include <cctype>
#include <chrono>
#include <limits>
#include <sstream>
#include <utility>
#include <vector>

namespace {

using U128 = unsigned __int128;
constexpr U128 kPrize = 150;
constexpr int64_t kMaxClaimsPerJob = 100;
constexpr std::size_t kMaxRetainedJobs = 128;

std::string jsonEscape(const std::string& value)
{
    std::string out;
    out.reserve(value.size() + 8);
    for (const unsigned char character : value) {
        switch (character) {
        case '\\': out += "\\\\"; break;
        case '"': out += "\\\""; break;
        case '\n': out += "\\n"; break;
        case '\r': out += "\\r"; break;
        case '\t': out += "\\t"; break;
        default:
            if (character < 0x20) {
                static constexpr char digits[] = "0123456789abcdef";
                out += "\\u00";
                out.push_back(digits[(character >> 4) & 0xf]);
                out.push_back(digits[character & 0xf]);
            } else {
                out.push_back(static_cast<char>(character));
            }
        }
    }
    return out;
}

std::string jsonString(const std::string& value)
{
    return "\"" + jsonEscape(value) + "\"";
}

std::string jsonValue(const std::string& document, const std::string& key)
{
    const std::string needle = "\"" + key + "\"";
    std::size_t position = 0;
    while ((position = document.find(needle, position)) != std::string::npos) {
        std::size_t cursor = position + needle.size();
        while (cursor < document.size() && std::isspace(static_cast<unsigned char>(document[cursor]))) {
            ++cursor;
        }
        if (cursor >= document.size() || document[cursor] != ':') {
            position += needle.size();
            continue;
        }
        ++cursor;
        while (cursor < document.size() && std::isspace(static_cast<unsigned char>(document[cursor]))) {
            ++cursor;
        }
        if (cursor >= document.size()) {
            return {};
        }

        const std::size_t start = cursor;
        const char first = document[cursor];
        if (first == '"') {
            bool escaped = false;
            for (++cursor; cursor < document.size(); ++cursor) {
                const char current = document[cursor];
                if (escaped) {
                    escaped = false;
                } else if (current == '\\') {
                    escaped = true;
                } else if (current == '"') {
                    return document.substr(start, cursor - start + 1);
                }
            }
            return {};
        }

        if (first == '{' || first == '[') {
            const char open = first;
            const char close = first == '{' ? '}' : ']';
            std::size_t depth = 0;
            bool inString = false;
            bool escaped = false;
            for (; cursor < document.size(); ++cursor) {
                const char current = document[cursor];
                if (inString) {
                    if (escaped) {
                        escaped = false;
                    } else if (current == '\\') {
                        escaped = true;
                    } else if (current == '"') {
                        inString = false;
                    }
                } else if (current == '"') {
                    inString = true;
                } else if (current == open) {
                    ++depth;
                } else if (current == close && --depth == 0) {
                    return document.substr(start, cursor - start + 1);
                }
            }
            return {};
        }

        while (cursor < document.size() && document[cursor] != ',' && document[cursor] != '}'
               && document[cursor] != ']' && !std::isspace(static_cast<unsigned char>(document[cursor]))) {
            ++cursor;
        }
        return document.substr(start, cursor - start);
    }
    return {};
}

std::string unquoteJsonString(const std::string& value)
{
    if (value.size() < 2 || value.front() != '"' || value.back() != '"') {
        return value;
    }
    std::string out;
    out.reserve(value.size() - 2);
    bool escaped = false;
    for (std::size_t index = 1; index + 1 < value.size(); ++index) {
        const char current = value[index];
        if (!escaped && current == '\\') {
            escaped = true;
            continue;
        }
        if (escaped) {
            switch (current) {
            case 'n': out.push_back('\n'); break;
            case 'r': out.push_back('\r'); break;
            case 't': out.push_back('\t'); break;
            default: out.push_back(current); break;
            }
            escaped = false;
        } else {
            out.push_back(current);
        }
    }
    return out;
}

bool parseU128(const std::string& value, U128& output)
{
    const std::string raw = unquoteJsonString(value);
    if (raw.empty()) {
        return false;
    }
    U128 parsed = 0;
    constexpr U128 max = ~static_cast<U128>(0);
    for (const char character : raw) {
        if (character < '0' || character > '9') {
            return false;
        }
        const unsigned digit = static_cast<unsigned>(character - '0');
        if (parsed > (max - digit) / 10) {
            return false;
        }
        parsed = parsed * 10 + digit;
    }
    output = parsed;
    return true;
}

std::string u128String(U128 value)
{
    if (value == 0) {
        return "0";
    }
    std::string out;
    while (value != 0) {
        out.push_back(static_cast<char>('0' + value % 10));
        value /= 10;
    }
    std::reverse(out.begin(), out.end());
    return out;
}

std::string errorObject(const std::string& code, const std::string& message)
{
    return "{\"code\":" + jsonString(code) + ",\"message\":" + jsonString(message) + "}";
}

std::string normalizationError(const std::string& code, const std::string& message)
{
    return "{\"ok\":false,\"error\":" + errorObject(code, message) + "}";
}

std::string normalizeBalanceResponse(const std::string& response)
{
    if (jsonValue(response, "ok") != "true") {
        return response;
    }
    U128 balance = 0;
    if (!parseU128(jsonValue(response, "result"), balance)) {
        return normalizationError("invalid_balance", "balance result is not an unsigned decimal integer");
    }
    return "{\"ok\":true,\"result\":" + jsonString(u128String(balance)) + "}";
}

std::string normalizeInitializedAccountResponse(const std::string& response)
{
    if (jsonValue(response, "ok") != "true") {
        return response;
    }
    const std::string result = jsonValue(response, "result");
    const std::string accountId = jsonValue(result, "account_id");
    const std::string initTxHash = jsonValue(result, "init_tx_hash");
    U128 balance = 0;
    if (accountId.size() < 2 || accountId.front() != '"'
        || !parseU128(jsonValue(result, "balance"), balance)) {
        return normalizationError("invalid_initialized_account",
            "initialized account result is missing an account ID or decimal balance");
    }
    return "{\"ok\":true,\"result\":{\"account_id\":" + accountId
        + ",\"init_tx_hash\":" + (initTxHash.empty() ? "null" : initTxHash)
        + ",\"balance\":" + jsonString(u128String(balance)) + "}}";
}

std::string normalizedClaimReceipt(const std::string& response)
{
    if (jsonValue(response, "ok") != "true") {
        return {};
    }
    const std::string result = jsonValue(response, "result");
    const std::string txHash = jsonValue(result, "tx_hash");
    const std::string staleRetries = jsonValue(result, "stale_challenge_retries");
    U128 before = 0;
    U128 after = 0;
    if (!parseU128(jsonValue(result, "balance_before"), before)
        || !parseU128(jsonValue(result, "balance_after"), after)) {
        return {};
    }
    return "{\"tx_hash\":" + (txHash.empty() ? "null" : txHash)
        + ",\"balance_before\":" + jsonString(u128String(before))
        + ",\"balance_after\":" + jsonString(u128String(after))
        + ",\"stale_challenge_retries\":" + (staleRetries.empty() ? "0" : staleRetries) + "}";
}

std::string normalizeClaimResponse(const std::string& response)
{
    if (jsonValue(response, "ok") != "true") {
        return response;
    }
    const std::string receipt = normalizedClaimReceipt(response);
    if (receipt.empty()) {
        return normalizationError("invalid_claim_receipt",
            "claim receipt is missing unsigned decimal balances");
    }
    return "{\"ok\":true,\"result\":" + receipt + "}";
}

void secureClear(std::string& value)
{
    volatile char* bytes = value.empty() ? nullptr : value.data();
    for (std::size_t index = 0; index < value.size(); ++index) {
        bytes[index] = '\0';
    }
    value.clear();
    value.shrink_to_fit();
}

} // namespace

struct LezFaucetModule::Job {
    mutable std::mutex mutex;
    std::string id;
    std::string operation;
    std::string status = "queued";
    std::string progressJson;
    std::string resultJson;
    std::string errorJson;
    bool cancelRequested = false;
    bool sensitiveResult = false;
    bool workerFinished = false;
    std::thread worker;
};

LezFaucetModule::LezFaucetModule() = default;

LezFaucetModule::~LezFaucetModule()
{
    m_stopping.store(true);
    {
        std::lock_guard<std::mutex> jobsLock(m_jobsMutex);
        for (const auto& entry : m_jobs) {
            std::lock_guard<std::mutex> jobLock(entry.second->mutex);
            entry.second->cancelRequested = true;
        }
    }
    std::vector<JobPtr> jobs;
    {
        std::lock_guard<std::mutex> jobsLock(m_jobsMutex);
        jobs.reserve(m_jobs.size());
        for (const auto& entry : m_jobs) {
            jobs.push_back(entry.second);
        }
    }
    std::vector<std::thread> workers;
    workers.reserve(jobs.size());
    for (const JobPtr& job : jobs) {
        std::lock_guard<std::mutex> jobLock(job->mutex);
        if (job->worker.joinable()) {
            workers.emplace_back(std::move(job->worker));
        }
    }
    for (std::thread& worker : workers) {
        if (worker.joinable()) {
            worker.join();
        }
    }

    if (FaucetHandle* handle = m_handle.exchange(nullptr); handle != nullptr) {
        lez_faucet_destroy(handle);
    }
}

std::string LezFaucetModule::name() const
{
    return "lez_faucet";
}

std::string LezFaucetModule::version() const
{
    return "0.1.0";
}

std::string LezFaucetModule::create(const std::string& configPath, const std::string& storagePath,
                                    const std::string& sequencerUrl, const std::string& password)
{
    return startJob("create", [this, configPath, storagePath, sequencerUrl,
                                passwordCopy = std::string(password)](const JobPtr& job) mutable {
        const std::string result = runCreate(configPath, storagePath, sequencerUrl, passwordCopy);
        secureClear(passwordCopy);
        if (ffiSucceeded(result)) {
            std::lock_guard<std::mutex> lock(job->mutex);
            job->sensitiveResult = true;
        }
        return result;
    });
}

std::string LezFaucetModule::open(const std::string& configPath, const std::string& storagePath,
                                  const std::string& sequencerUrl)
{
    return startJob("open", [this, configPath, storagePath, sequencerUrl](const JobPtr&) {
        return runOpen(configPath, storagePath, sequencerUrl);
    });
}

std::string LezFaucetModule::destroy()
{
    return startJob("destroy", [this](const JobPtr&) {
        FaucetHandle* handle = m_handle.exchange(nullptr);
        const bool existed = handle != nullptr;
        if (handle != nullptr) {
            lez_faucet_destroy(handle);
        }
        return std::string{"{\"ok\":true,\"result\":{\"destroyed\":"}
            + (existed ? "true" : "false") + "}}";
    });
}

std::string LezFaucetModule::verifyFingerprint()
{
    return startJob("verify_fingerprint", [this](const JobPtr&) {
        return runHandleCall("verify_fingerprint", [](FaucetHandle* handle) {
            return lez_faucet_verify_fingerprint(handle);
        });
    });
}

std::string LezFaucetModule::createAndInitializeAccount()
{
    return startJob("create_and_initialize_account", [this](const JobPtr&) {
        return normalizeInitializedAccountResponse(runHandleCall(
            "create_and_initialize_account", [](FaucetHandle* handle) {
            return lez_faucet_create_and_initialize_account(handle);
        }));
    });
}

std::string LezFaucetModule::balance(const std::string& accountId)
{
    return startJob("balance", [this, accountId](const JobPtr&) {
        return normalizeBalanceResponse(runHandleCall(
            "balance", [&accountId](FaucetHandle* handle) {
            return lez_faucet_get_balance(handle, accountId.c_str());
        }));
    });
}

std::string LezFaucetModule::claimOnce(const std::string& accountId)
{
    return startJob("claim_once", [this, accountId](const JobPtr&) {
        return normalizeClaimResponse(runHandleCall(
            "claim_once", [&accountId](FaucetHandle* handle) {
            return lez_faucet_claim_once(handle, accountId.c_str());
        }));
    });
}

std::string LezFaucetModule::claimUntilTarget(const std::string& accountId, const std::string& target,
                                              int64_t maxClaims)
{
    return startJob("claim_until_target", [this, accountId, target, maxClaims](const JobPtr& job) {
        return runClaimUntil(job, accountId, target, maxClaims);
    });
}

std::string LezFaucetModule::cancel(const std::string& jobId)
{
    reapFinishedWorkers();
    JobPtr job;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        const auto found = m_jobs.find(jobId);
        if (found == m_jobs.end()) {
            return structuredError("unknown_job", "unknown job_id");
        }
        job = found->second;
    }
    {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (!isTerminal(job->status)) {
            const bool wasRunning = job->status == "running";
            job->cancelRequested = true;
            job->status = "cancelling";
            const bool canInterrupt = job->operation == "claim_once"
                || job->operation == "claim_until_target";
            if (wasRunning && canInterrupt) {
                if (FaucetHandle* handle = m_handle.load(); handle != nullptr) {
                    lez_faucet_cancel(handle);
                }
            }
        }
    }
    return jobJson(job);
}

std::string LezFaucetModule::jobStatus(const std::string& jobId)
{
    reapFinishedWorkers();
    JobPtr job;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        const auto found = m_jobs.find(jobId);
        if (found == m_jobs.end()) {
            return structuredError("unknown_job", "unknown job_id");
        }
        job = found->second;
    }
    return jobJson(job);
}

std::string LezFaucetModule::jobResultAck(const std::string& jobId)
{
    reapFinishedWorkers();
    JobPtr job;
    std::thread worker;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        const auto found = m_jobs.find(jobId);
        if (found == m_jobs.end()) {
            return structuredError("unknown_job", "unknown job_id");
        }
        job = found->second;
        std::lock_guard<std::mutex> jobLock(job->mutex);
        if (!isTerminal(job->status)) {
            return structuredError("job_not_terminal", "job result cannot be acknowledged before completion");
        }
        secureClear(job->resultJson);
        job->sensitiveResult = false;
        if (job->worker.joinable()) {
            worker = std::move(job->worker);
        }
        m_jobs.erase(found);
    }
    if (worker.joinable()) {
        worker.join();
    }
    return "{\"ok\":true,\"job_id\":" + jsonString(jobId)
        + ",\"acknowledged\":true,\"reaped\":true}";
}

std::string LezFaucetModule::startJob(const std::string& operation,
                                      std::function<std::string(const JobPtr&)> work)
{
    reapFinishedWorkers();
    if (m_stopping.load()) {
        return structuredError("module_stopping", "module is shutting down");
    }

    auto job = std::make_shared<Job>();
    job->id = operation + "-" + std::to_string(m_nextJobId.fetch_add(1));
    job->operation = operation;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        if (m_jobs.size() >= kMaxRetainedJobs) {
            return structuredError("job_limit_reached",
                "too many unacknowledged jobs; acknowledge terminal results before starting more work");
        }
        m_jobs.emplace(job->id, job);
    }

    std::thread worker([this, job, work = std::move(work)]() {
        std::unique_lock<std::mutex> operationLock(m_operationMutex);
        {
            std::lock_guard<std::mutex> lock(job->mutex);
            if (job->cancelRequested || m_stopping.load()) {
                job->status = "cancelled";
                job->errorJson = errorObject("cancelled", "job cancelled before it started");
                job->workerFinished = true;
                return;
            }
            job->status = "running";
        }

        const std::string result = work(job);
        std::lock_guard<std::mutex> lock(job->mutex);
        if (job->status != "cancelled") {
            if (ffiSucceeded(result)) {
                job->status = "completed";
                job->resultJson = ffiResultPayload(result);
                job->errorJson.clear();
            } else {
                job->status = "failed";
                const std::string error = jsonValue(result, "error");
                const std::string errorMessage = unquoteJsonString(error);
                if (job->cancelRequested && errorMessage.find("cancel") != std::string::npos) {
                    job->status = "cancelled";
                    job->errorJson = errorObject("cancelled", errorMessage);
                } else {
                    job->errorJson = ffiErrorPayload(result);
                }
            }
        }
        job->workerFinished = true;
    });

    {
        std::lock_guard<std::mutex> lock(job->mutex);
        job->worker = std::move(worker);
    }
    return jobJson(job);
}

void LezFaucetModule::reapFinishedWorkers()
{
    std::vector<JobPtr> jobs;
    {
        std::lock_guard<std::mutex> lock(m_jobsMutex);
        jobs.reserve(m_jobs.size());
        for (const auto& entry : m_jobs) {
            jobs.push_back(entry.second);
        }
    }

    std::vector<std::thread> finished;
    for (const JobPtr& job : jobs) {
        std::lock_guard<std::mutex> lock(job->mutex);
        if (job->workerFinished && job->worker.joinable()) {
            finished.emplace_back(std::move(job->worker));
        }
    }
    for (std::thread& worker : finished) {
        worker.join();
    }
}

std::string LezFaucetModule::runCreate(const std::string& configPath, const std::string& storagePath,
                                       const std::string& sequencerUrl, const std::string& password)
{
    if (m_handle.load() != nullptr) {
        return structuredError("wallet_already_open", "destroy the current wallet before creating another");
    }
    FfiCreateOutput output = lez_faucet_create(
        configPath.c_str(), storagePath.c_str(), sequencerUrl.c_str(), password.c_str());
    const std::string result = takeFfiString(output.result_json);
    if (output.handle == nullptr) {
        return result.empty()
            ? structuredError("create_failed", "wallet creation failed without an FFI result")
            : result;
    }
    if (!ffiSucceeded(result)) {
        lez_faucet_destroy(output.handle);
        return result.empty()
            ? structuredError("create_failed", "wallet creation returned an invalid result")
            : result;
    }
    m_handle.store(output.handle);
    return result;
}

std::string LezFaucetModule::runOpen(const std::string& configPath, const std::string& storagePath,
                                     const std::string& sequencerUrl)
{
    if (m_handle.load() != nullptr) {
        return structuredError("wallet_already_open", "destroy the current wallet before opening another");
    }
    FfiCreateOutput output = lez_faucet_open(
        configPath.c_str(), storagePath.c_str(), sequencerUrl.c_str());
    const std::string result = takeFfiString(output.result_json);
    if (output.handle == nullptr) {
        return result.empty()
            ? structuredError("open_failed", "wallet open failed without an FFI result")
            : result;
    }
    if (!ffiSucceeded(result)) {
        lez_faucet_destroy(output.handle);
        return result.empty()
            ? structuredError("open_failed", "wallet open returned an invalid result")
            : result;
    }
    m_handle.store(output.handle);
    return result;
}

std::string LezFaucetModule::runHandleCall(const std::string& operation,
                                           const std::function<char*(FaucetHandle*)>& call)
{
    FaucetHandle* handle = m_handle.load();
    if (handle == nullptr) {
        return structuredError("wallet_not_open", operation + " requires an open wallet");
    }
    const std::string result = takeFfiString(call(handle));
    return result.empty()
        ? structuredError("invalid_ffi_result", operation + " returned a null or empty result")
        : result;
}

std::string LezFaucetModule::runClaimUntil(const JobPtr& job, const std::string& accountId,
                                           const std::string& target, int64_t maxClaims)
{
    FaucetHandle* handle = m_handle.load();
    if (handle == nullptr) {
        return structuredError("wallet_not_open", "claim_until_target requires an open wallet");
    }
    if (maxClaims < 0 || maxClaims > kMaxClaimsPerJob) {
        return structuredError("invalid_max_claims", "maxClaims must be between 0 and 100");
    }

    U128 targetValue = 0;
    if (!parseU128(target, targetValue)) {
        return structuredError("invalid_target", "target must be an unsigned decimal integer");
    }

    const std::string balanceResponse = takeFfiString(lez_faucet_get_balance(handle, accountId.c_str()));
    if (!ffiSucceeded(balanceResponse)) {
        return balanceResponse.empty()
            ? structuredError("invalid_ffi_result", "get_balance returned a null or empty result")
            : balanceResponse;
    }
    U128 initialBalance = 0;
    if (!parseU128(jsonValue(balanceResponse, "result"), initialBalance)) {
        return structuredError("invalid_balance", "get_balance returned a non-decimal balance");
    }

    const U128 missing = initialBalance >= targetValue ? 0 : targetValue - initialBalance;
    const U128 requiredU128 = missing / kPrize + (missing % kPrize == 0 ? 0 : 1);
    if (requiredU128 > static_cast<U128>(std::numeric_limits<int64_t>::max())
        || requiredU128 > static_cast<U128>(maxClaims)) {
        return structuredError("claim_limit_exceeded",
            "target requires more claims than the explicit maxClaims limit");
    }
    const int64_t requiredClaims = static_cast<int64_t>(requiredU128);

    U128 balance = initialBalance;
    int64_t completedClaims = 0;
    {
        std::ostringstream progress;
        progress << "{\"completed_claims\":0"
                 << ",\"required_claims\":" << requiredClaims
                 << ",\"target\":" << jsonString(u128String(targetValue))
                 << ",\"balance\":" << jsonString(u128String(balance))
                 << ",\"receipt\":null}";
        std::lock_guard<std::mutex> lock(job->mutex);
        job->progressJson = progress.str();
    }
    while (balance < targetValue) {
        {
            std::lock_guard<std::mutex> lock(job->mutex);
            if (job->cancelRequested || m_stopping.load()) {
                job->status = "cancelled";
                job->errorJson = errorObject("cancelled",
                    "claim loop cancelled between confirmed claims; no in-flight claim was abandoned");
                return structuredError("cancelled", "claim loop cancelled between confirmed claims");
            }
        }

        const std::string claimResponse = takeFfiString(lez_faucet_claim_once(handle, accountId.c_str()));
        if (!ffiSucceeded(claimResponse)) {
            return claimResponse.empty()
                ? structuredError("invalid_ffi_result", "claim_once returned a null or empty result")
                : claimResponse;
        }

        U128 before = 0;
        U128 after = 0;
        if (!parseU128(jsonValue(claimResponse, "balance_before"), before)
            || !parseU128(jsonValue(claimResponse, "balance_after"), after)) {
            return structuredError("invalid_claim_receipt", "claim receipt is missing decimal balances");
        }
        constexpr U128 maxBalance = ~static_cast<U128>(0);
        if (before != balance || before > maxBalance - kPrize || after != before + kPrize) {
            return structuredError("invalid_claim_delta", "claim receipt did not prove an exact +150 credit");
        }

        balance = after;
        ++completedClaims;
        const std::string receipt = normalizedClaimReceipt(claimResponse);
        if (receipt.empty()) {
            return structuredError("invalid_claim_receipt",
                "claim receipt could not be normalized to exact decimal strings");
        }
        std::ostringstream progress;
        progress << "{\"completed_claims\":" << completedClaims
                 << ",\"required_claims\":" << requiredClaims
                 << ",\"target\":" << jsonString(u128String(targetValue))
                 << ",\"balance\":" << jsonString(u128String(balance))
                 << ",\"receipt\":" << (receipt.empty() ? "null" : receipt) << "}";
        {
            std::lock_guard<std::mutex> lock(job->mutex);
            job->progressJson = progress.str();
        }
    }

    std::ostringstream result;
    result << "{\"ok\":true,\"result\":{\"target\":" << jsonString(u128String(targetValue))
           << ",\"initial_balance\":" << jsonString(u128String(initialBalance))
           << ",\"final_balance\":" << jsonString(u128String(balance))
           << ",\"completed_claims\":" << completedClaims << "}}";
    return result.str();
}

std::string LezFaucetModule::takeFfiString(char* value)
{
    if (value == nullptr) {
        return {};
    }
    std::string result(value);
    lez_faucet_string_free(value);
    return result;
}

std::string LezFaucetModule::structuredError(const std::string& code, const std::string& message)
{
    return "{\"ok\":false,\"error\":" + errorObject(code, message) + "}";
}

std::string LezFaucetModule::jobJson(const JobPtr& job)
{
    std::lock_guard<std::mutex> lock(job->mutex);
    const std::string result = job->resultJson.empty() ? "null" : job->resultJson;
    std::ostringstream out;
    out << "{\"ok\":true"
        << ",\"job_id\":" << jsonString(job->id)
        << ",\"operation\":" << jsonString(job->operation)
        << ",\"status\":" << jsonString(job->status)
        << ",\"cancel_requested\":" << (job->cancelRequested ? "true" : "false")
        << ",\"progress\":" << (job->progressJson.empty() ? "null" : job->progressJson)
        << ",\"result\":" << result
        << ",\"error\":" << (job->errorJson.empty() ? "null" : job->errorJson)
        << "}";

    return out.str();
}

bool LezFaucetModule::isTerminal(const std::string& status)
{
    return status == "completed" || status == "failed" || status == "cancelled";
}

std::string LezFaucetModule::ffiResultPayload(const std::string& json)
{
    const std::string result = jsonValue(json, "result");
    if (!result.empty()) {
        return result;
    }

    // Wallet creation intentionally returns the one-time recovery fields at
    // the top level instead of under `result`.
    const std::string mnemonic = jsonValue(json, "mnemonic");
    const std::string warning = jsonValue(json, "security_warning");
    if (!mnemonic.empty() && !warning.empty()) {
        return "{\"mnemonic\":" + mnemonic + ",\"security_warning\":" + warning + "}";
    }
    return "null";
}

std::string LezFaucetModule::ffiErrorPayload(const std::string& json)
{
    const std::string error = jsonValue(json, "error");
    if (error.empty()) {
        return errorObject("operation_failed", "operation returned an invalid error");
    }
    if (error.front() != '"') {
        return error;
    }

    const std::string outcome = jsonValue(json, "outcome");
    const std::string txHash = jsonValue(json, "tx_hash");
    const bool outcomeUnknown = outcome == "\"unknown\"";
    std::string structured = "{\"code\":"
        + jsonString(outcomeUnknown ? "outcome_unknown" : "ffi_error")
        + ",\"message\":" + jsonString(unquoteJsonString(error));
    if (!outcome.empty()) {
        structured += ",\"outcome\":" + outcome;
    }
    if (!txHash.empty()) {
        structured += ",\"tx_hash\":" + txHash;
    }
    structured += "}";
    return structured;
}

bool LezFaucetModule::ffiSucceeded(const std::string& json)
{
    return jsonValue(json, "ok") == "true";
}
