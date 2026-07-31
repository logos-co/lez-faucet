#pragma once

#include <cstdint>
#include <string>

// Test double for the stateless faucet C ABI.
//
// Every knob here exists to reproduce a real failure mode of the live client:
// a slow network, a cancel that arrives before submission, a cancel that
// arrives after it, and an error payload whose *message* is shaped like a
// success envelope.
namespace MockLezFaucetFfi {

void reset();

// -- injection ---------------------------------------------------------------
void setClientNewFails(bool value);
void setInfoDelayMs(int value);
void setInspectDelayMs(int value);
void setDropDelayMs(int value);
void setInfoResponse(const std::string& json);
void setInspectResponse(const std::string& json);
void setDropResponse(const std::string& json);
/// The drop blocks until its own operation token is cancelled, then returns
/// the configured response. Models work in progress that a cancel can reach.
void setDropWaitsForCancel(bool value);
/// A cancel before submission is honoured with a structured `cancelled` error.
void setDropHonoursCancel(bool value);
/// The drop blocks until releaseDrop(), whatever any cancel says. Models the
/// post-submission reconciliation that cannot be recalled.
void setDropWaitsForRelease(bool value);
void releaseDrop();
/// What lez_faucet_current_phase returns. Defaults to a null phase, the "no
/// drop is live" answer of the real client.
void setPhaseResponse(const std::string& json);
/// The phase poll blocks until releasePhase(). Models nothing the real client
/// does — its poll is two atomic reads — but proves the module never lets a
/// slow poll hold job->mutex or block a concurrent caller.
void setPhaseBlocks(bool value);
void releasePhase();

// -- observation -------------------------------------------------------------
bool waitForDropStart(int timeoutMs);
bool waitForCancelCall(int timeoutMs);
bool waitForPhaseStart(int timeoutMs);
int clientNewCalls();
int destroyCalls();
int stringAllocations();
int stringFreeCalls();
int infoCalls();
int inspectCalls();
int dropCalls();
int cancelCalls();
int phaseCalls();
uint64_t lastDropToken();
uint64_t lastCancelToken();
uint64_t lastPhaseToken();
std::string lastAddress();
std::string lastRequestKey();
std::string lastSequencerUrl();
/// True if the client handle was destroyed while an FFI call was in flight,
/// which is the use-after-free the destructor ordering exists to prevent.
bool destroyWhileCallActive();

} // namespace MockLezFaucetFfi
