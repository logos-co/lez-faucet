// SPDX-License-Identifier: MIT OR Apache-2.0
#include "ExternalRecipientState.h"

#include <cstdlib>
#include <iostream>

namespace {

#define CHECK(condition)                                                                       \
    do {                                                                                       \
        if (!(condition)) {                                                                    \
            std::cerr << "CHECK failed at " << __FILE__ << ':' << __LINE__ << ": " #condition \
                      << '\n';                                                                 \
            std::exit(1);                                                                      \
        }                                                                                      \
    } while (false)

constexpr auto RECIPIENT_A = "RecipientA";
constexpr auto RECIPIENT_B = "RecipientB";

void openWalletGatesExternalOperations()
{
    ExternalRecipientState state;
    CHECK(!state.canStartExternalOperation());
    state.markClientOpen();
    CHECK(state.canStartExternalOperation());
}

void onlyTheExactPreflightTargetIsAuthorized()
{
    ExternalRecipientState state;
    state.markClientOpen();
    state.beginPreflight();
    state.recordJob("balance-a", RECIPIENT_A);
    CHECK(state.completePreflight("balance-a"));
    CHECK(state.verifiedRecipient() == RECIPIENT_A);
    CHECK(!state.consumePreflightForClaim(RECIPIENT_B));
    CHECK(state.verifiedRecipient() == RECIPIENT_A);
}

void failedPreflightClearsPriorAuthorization()
{
    ExternalRecipientState state;
    state.beginPreflight();
    state.recordJob("balance-a", RECIPIENT_A);
    CHECK(state.completePreflight("balance-a"));
    state.beginPreflight();
    CHECK(!state.completePreflight("missing-job"));
    CHECK(state.verifiedRecipient().empty());
    CHECK(!state.consumePreflightForClaim(RECIPIENT_A));
}

void claimStartConsumesAuthorization()
{
    ExternalRecipientState state;
    state.beginPreflight();
    state.recordJob("balance-a", RECIPIENT_A);
    CHECK(state.completePreflight("balance-a"));
    CHECK(state.consumePreflightForClaim(RECIPIENT_A));
    CHECK(state.verifiedRecipient().empty());
    CHECK(!state.consumePreflightForClaim(RECIPIENT_A));
}

void jobPinSurvivesClaimConsumptionUntilAcknowledgement()
{
    ExternalRecipientState state;
    state.beginPreflight();
    state.recordJob("balance-a", RECIPIENT_A);
    CHECK(state.completePreflight("balance-a"));
    CHECK(state.consumePreflightForClaim(RECIPIENT_A));
    state.recordJob("claim-a", RECIPIENT_A);
    CHECK(state.pinnedRecipient("claim-a") == RECIPIENT_A);
    state.acknowledge("claim-a");
    CHECK(state.pinnedRecipient("claim-a").empty());
}

} // namespace

int main()
{
    openWalletGatesExternalOperations();
    onlyTheExactPreflightTargetIsAuthorized();
    failedPreflightClearsPriorAuthorization();
    claimStartConsumesAuthorization();
    jobPinSurvivesClaimConsumptionUntilAcknowledgement();
    std::cout << "faucet backend native tests passed\n";
    return 0;
}
