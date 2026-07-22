#pragma once

#include <cstdint>

namespace MockLezFaucetFfi {

void reset();
void setBalance(uint64_t value);
uint64_t balance();
void setClaimDelayMs(int value);
void setFingerprintDelayMs(int value);
void setCancelStopsClaim(bool value);
void setClaimOutcomeUnknown(bool value);
int claimCalls();
int stringFreeCalls();
int destroyCalls();
int rustClaimUntilCalls();
int cancelCalls();

} // namespace MockLezFaucetFfi
