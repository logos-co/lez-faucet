#pragma once

#include <cstdint>
#include <string>

namespace MockLezFaucetFfi {

void reset();
void setBalance(uint64_t value);
void setBalanceRecipientError(bool value);
uint64_t balance();
void setClaimDelayMs(int value);
void setFingerprintDelayMs(int value);
void setCancelStopsClaim(bool value);
void setClaimWaitsForCancel(bool value);
void setClaimWaitsForReconciliationRelease(bool value);
void setClaimOutcomeUnknown(bool value);
bool waitForClaimStart(int timeoutMs);
bool waitForCancelCall(int timeoutMs);
void releaseClaimReconciliation();
int claimCalls();
int stringFreeCalls();
int destroyCalls();
int rustClaimUntilCalls();
int cancelCalls();
bool destroyWhileClaimActive();
std::string lastBalanceAccountId();
std::string lastClaimAccountId();

} // namespace MockLezFaucetFfi
