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

// Qt-free Basecamp core module for the stateless LEZ Faucet.
//
// This module owns no wallet, no key material and no files. It accepts one
// public account address and, per explicit user action, requests exactly one
// fixed 150 LEZ credit. There is no password, mnemonic, recovery phrase or
// account-creation surface anywhere in this class, and there must never be:
// the underlying claim transaction names both accounts as unsigned public
// participants, so there is nothing to sign with.
//
// getFaucetInfo/inspectRecipient/requestDrop start a worker job and return
// immediately. Call jobStatus(job_id) until the status is one of succeeded,
// failed, cancelled or outcome_unknown. Every terminal result is replayed by
// jobStatus until jobResultAck(job_id) acknowledges it, at which point the
// retained job record is reaped.
//
// A request-key tombstone is *not* a job record. Job records are bounded and
// reapable; tombstones live for the whole process and are never evicted,
// because forgetting a key would let a bridge retry become a second claim.
//
// Cancellation is cooperative and scoped by an operation token allocated here.
// Before submission a cancel yields `cancelled`; once the Rust client has
// submitted, the chain action is irrevocable and the job terminates as
// succeeded/failed/outcome_unknown, never as cancelled.
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

    std::string configure(const std::string& sequencerUrl);
    std::string getFaucetInfo();
    std::string inspectRecipient(const std::string& address);
    std::string requestDrop(const std::string& address, const std::string& requestKey);
    std::string cancel(const std::string& jobId);
    std::string jobStatus(const std::string& jobId);
    std::string jobResultAck(const std::string& jobId);
    std::string shutdown();

private:
    struct Job;
    using JobPtr = std::shared_ptr<Job>;

    std::string startJob(const std::string& operation, const std::string& requestKey, bool mutating, std::function<std::string(const JobPtr&)> work);
    void settleJob(const JobPtr& job, bool ran, const std::string& raw);
    void reapFinishedWorkers();
    void joinAllWorkers();
    std::string runFfiCall(const std::function<char*(LezFaucetClient*)>& call);
    std::string runDrop(const JobPtr& job, const std::string& address, const std::string& requestKey);
    JobPtr findJob(const std::string& jobId) const;
    std::string tombstonedJob(const std::string& requestKey) const;
    void rememberRequestKey(const std::string& requestKey, const std::string& address, const std::string& jobId);
    static std::string takeFfiString(char* value);
    static std::string structuredError(const std::string& code, const std::string& message);
    static std::string jobJson(const JobPtr& job);
    static std::string startedJson(const JobPtr& job);
    static bool isTerminal(const std::string& status);

    /// One retained request key. Never evicted; see the class comment.
    struct Tombstone {
        std::string accountId;
        std::string jobId;
        std::string status;
    };

    bool reserveDropSlot();
    void releaseDropSlot();
    void recordTombstoneStatus(const std::string& requestKey, const std::string& status);

    std::atomic<uint64_t> m_nextJobId{1};
    std::atomic<uint64_t> m_nextOperationToken{1};
    std::atomic<bool> m_stopping{false};
    mutable std::mutex m_jobsMutex;
    std::unordered_map<std::string, JobPtr> m_jobs;
    mutable std::mutex m_tombstonesMutex;
    std::unordered_map<std::string, Tombstone> m_tombstones;
    // Guards the single-mutating-job permit only. Reads never take it, so a
    // 300 s reconciliation can never make getFaucetInfo/inspectRecipient block
    // (API_LOCK §A1.7.2).
    mutable std::mutex m_dropMutex;
    bool m_dropActive = false;
    std::atomic<LezFaucetClient*> m_client{nullptr};
};
