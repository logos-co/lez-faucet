#pragma once

#include <atomic>
#include <cstdint>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>

extern "C" {
#include <lez_faucet_ffi.h>
}

// Qt-free Basecamp core module. Every wallet/network operation starts a worker
// job and returns immediately. Call jobStatus(job_id) until the status is one
// of completed, failed, or cancelled.
//
// Every terminal result is replayed by jobStatus until jobResultAck(job_id)
// explicitly acknowledges it, at which point the retained job is reaped. A
// create result's stored mnemonic is wiped as part of that acknowledgement.
//
// claimUntilTarget cancellation is cooperative: it is checked before the
// first claim and between confirmed claims. An in-flight claim is never
// abandoned because its Rust ABI call owns submission, polling, and balance
// verification as one atomic operation.
//
// Keep public declarations on one line: logos-cpp-generator parses this header
// line-by-line when generating Basecamp glue.
class LezFaucetModule {
public:
    LezFaucetModule();
    ~LezFaucetModule();

    LezFaucetModule(const LezFaucetModule&) = delete;
    LezFaucetModule& operator=(const LezFaucetModule&) = delete;

    std::string name() const;
    std::string version() const;

    std::string create(const std::string& configPath, const std::string& storagePath, const std::string& sequencerUrl, const std::string& password);
    std::string open(const std::string& configPath, const std::string& storagePath, const std::string& sequencerUrl);
    std::string destroy();
    std::string verifyFingerprint();
    std::string createAndInitializeAccount();
    std::string balance(const std::string& accountId);
    std::string claimOnce(const std::string& accountId);
    std::string claimUntilTarget(const std::string& accountId, const std::string& target, int64_t maxClaims);
    std::string cancel(const std::string& jobId);
    std::string jobStatus(const std::string& jobId);
    std::string jobResultAck(const std::string& jobId);

private:
    struct Job;
    using JobPtr = std::shared_ptr<Job>;

    std::string startJob(const std::string& operation, std::function<std::string(const JobPtr&)> work);
    void reapFinishedWorkers();
    std::string runCreate(const std::string& configPath, const std::string& storagePath, const std::string& sequencerUrl, const std::string& password);
    std::string runOpen(const std::string& configPath, const std::string& storagePath, const std::string& sequencerUrl);
    std::string runHandleCall(const std::string& operation, const std::function<char*(FaucetHandle*)>& call);
    std::string runClaimUntil(const JobPtr& job, const std::string& accountId, const std::string& target, int64_t maxClaims);
    static std::string takeFfiString(char* value);
    static std::string structuredError(const std::string& code, const std::string& message);
    static std::string jobJson(const JobPtr& job);
    static std::string ffiResultPayload(const std::string& json);
    static std::string ffiErrorPayload(const std::string& json);
    static bool isTerminal(const std::string& status);
    static bool ffiSucceeded(const std::string& json);

    std::atomic<uint64_t> m_nextJobId{1};
    std::atomic<bool> m_stopping{false};
    mutable std::mutex m_jobsMutex;
    std::unordered_map<std::string, JobPtr> m_jobs;
    std::mutex m_operationMutex;
    std::atomic<FaucetHandle*> m_handle{nullptr};
};
