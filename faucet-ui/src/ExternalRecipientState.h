// SPDX-License-Identifier: MIT OR Apache-2.0
#pragma once

#include <string>
#include <unordered_map>

class ExternalRecipientState final {
public:
    void markClientOpen() { m_clientOpen = true; }
    bool canStartExternalOperation() const { return m_clientOpen; }

    void beginPreflight() { m_verifiedRecipient.clear(); }

    void recordJob(const std::string& jobId, const std::string& recipient)
    {
        m_jobRecipients.insert_or_assign(jobId, recipient);
    }

    bool completePreflight(const std::string& jobId)
    {
        const auto recipient = m_jobRecipients.find(jobId);
        if (recipient == m_jobRecipients.end() || recipient->second.empty()) {
            m_verifiedRecipient.clear();
            return false;
        }
        m_verifiedRecipient = recipient->second;
        return true;
    }

    bool consumePreflightForClaim(const std::string& recipient)
    {
        if (recipient.empty() || recipient != m_verifiedRecipient)
            return false;
        m_verifiedRecipient.clear();
        return true;
    }

    std::string pinnedRecipient(const std::string& jobId) const
    {
        const auto recipient = m_jobRecipients.find(jobId);
        return recipient == m_jobRecipients.end() ? std::string() : recipient->second;
    }

    void acknowledge(const std::string& jobId) { m_jobRecipients.erase(jobId); }

    const std::string& verifiedRecipient() const { return m_verifiedRecipient; }

private:
    bool m_clientOpen = false;
    std::string m_verifiedRecipient;
    std::unordered_map<std::string, std::string> m_jobRecipients;
};
