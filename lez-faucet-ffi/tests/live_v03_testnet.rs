//! Live LEZ v0.3 testnet tests.
//!
//! These talk to the real v0.3 testnet and the real hosted claim service, so
//! they are `#[ignore]`d and are never part of a normal `cargo test` run.
//!
//! `LEZ_FAUCET_CLAIM_API_URL` overrides the claim service root (for a service
//! under test on another host); the sequencer stays the pinned one, and the
//! override must still be `https` (or `http` on loopback).
//!
//! The read-only tests are safe to run at any time. The single write test asks
//! the service to spend 0.1 LEZ from its hot balance, and is additionally gated
//! on an explicit destination through `LEZ_FAUCET_LIVE_RECIPIENT`, so that no
//! claim can be requested without someone naming the recipient on purpose. On
//! v0.3 any valid 32-byte base58 id is a valid recipient: there is no
//! initialization step.

use lez_faucet_ffi::{
    client::{Eligibility, FaucetClient, ProgramFingerprint},
    error::ErrorCode,
    network::{self, ClaimMechanism, Network, NetworkFingerprint},
};

fn network() -> &'static Network {
    match std::env::var("LEZ_FAUCET_CLAIM_API_URL") {
        Ok(base_url) => {
            let base_url: &'static str = Box::leak(base_url.into_boxed_str());
            println!("claim api override={base_url}");
            Box::leak(Box::new(Network {
                claim: ClaimMechanism::ClaimApi {
                    base_url,
                    prize: network::LEZ_V03_TESTNET.prize(),
                },
                ..network::LEZ_V03_TESTNET
            }))
        }
        Err(_) => &network::LEZ_V03_TESTNET,
    }
}

fn client() -> FaucetClient {
    let network = network();
    println!(
        "sequencer={} claim_api={}",
        network.sequencer_url,
        network.claim_api_url().unwrap_or("-")
    );
    // The override path keeps the sequencer pinned: `for_network` refuses any
    // row whose sequencer host is not an enabled one, so only the service
    // root can differ from the shipped row.
    FaucetClient::for_network(network).expect("the pinned sequencer URL must be accepted")
}

fn decimal(value: &str) -> u128 {
    value
        .parse()
        .expect("balances are canonical decimal strings")
}

#[tokio::test]
#[ignore = "talks to the live LEZ v0.3 testnet and the claim service"]
async fn faucet_info_matches_the_pinned_protocol() {
    let info = client().get_info().await.expect("faucet info");
    let network = network();

    assert_eq!(info.network, network.id);
    assert_eq!(info.lez_version, network.lez_version);
    assert_eq!(info.claim_mechanism, "claim_api");
    assert_eq!(info.decimals, 9);
    assert_eq!(info.prize_amount, "100000000");
    assert_eq!(info.pinata_account, None);
    assert_eq!(info.difficulty_bytes, None);
    assert_eq!(info.claim_api_url, network.claim_api_url());

    // The fingerprint gate passed: the deployed circuit is the one the row
    // records, which is the whole point of calling this before anything is
    // ever requested.
    let NetworkFingerprint::PpcImageId(expected) = network.fingerprint else {
        panic!("the v0.3 testnet is gated on the circuit ImageID");
    };
    assert_eq!(
        info.program_fingerprint,
        ProgramFingerprint::PrivacyPreservingCircuit {
            privacy_preserving_circuit: expected.to_owned(),
        }
    );

    let hot = decimal(&info.pool_balance);
    let remaining = decimal(&info.claims_remaining);
    assert_eq!(
        info.can_claim,
        !info.paused && remaining > 0 && hot >= 100_000_000 && info.blocked_reason.is_none()
    );
    println!(
        "hot_balance={hot} claims_remaining={remaining} paused={} blocked={:?}",
        info.paused, info.blocked_reason
    );
}

#[tokio::test]
#[ignore = "talks to the live LEZ v0.3 testnet"]
async fn any_public_account_is_eligible_and_reads_a_balance() {
    let client = client();
    // A fresh, never-funded id: eligible with a balance of 0.
    let unseen = client
        .inspect_recipient("CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r")
        .await
        .expect("inspection");
    assert_eq!(unseen.eligibility, Eligibility::Eligible);
    assert!(unseen.balance.is_some());
    assert_eq!(unseen.program_owner, None);
    assert_eq!(unseen.initialization_command, None);
}

#[tokio::test]
#[ignore = "talks to the live LEZ v0.3 testnet"]
async fn malformed_and_private_addresses_never_reach_the_network() {
    let client = client();
    let bomb = "1".repeat(133);
    for bad in [
        "Private/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r",
        "not-an-account",
        "",
        bomb.as_str(),
    ] {
        let error = client
            .inspect_recipient(bad)
            .await
            .err()
            .unwrap_or_else(|| panic!("{bad:?} must be rejected"));
        assert_eq!(error.code, ErrorCode::InvalidPublicAccountId, "{bad:?}");
    }
}

/// The one authorized write test.
///
/// Requires `LEZ_FAUCET_LIVE_RECIPIENT` to name the destination explicitly.
/// Without it the test reports that it did nothing and passes. Use a fresh
/// throwaway id: the service allows one claim per recipient per day.
#[tokio::test]
#[ignore = "asks the claim service to spend 0.1 LEZ from its hot balance"]
async fn one_authorized_claim_credits_exactly_the_prize() {
    let Ok(recipient) = std::env::var("LEZ_FAUCET_LIVE_RECIPIENT") else {
        println!(
            "skipped: set LEZ_FAUCET_LIVE_RECIPIENT to the approved destination address to run this"
        );
        return;
    };
    const PRIZE: u128 = 100_000_000;

    let client = client();
    let info_before = client.get_info().await.expect("faucet info");
    assert!(
        info_before.can_claim,
        "the service is not accepting claims: {info_before:?}"
    );
    let inspection = client
        .inspect_recipient(&recipient)
        .await
        .expect("recipient inspection");
    let balance_before = decimal(
        &inspection
            .balance
            .clone()
            .expect("v0.3 always reports a balance"),
    );
    let hot_before = decimal(&info_before.pool_balance);
    println!("before: recipient={balance_before} hot={hot_before}");

    let request_key = "3f2504e0-4f89-41d3-9a0c-0305e82c3302";
    let receipt = client
        .request_drop(&recipient, request_key, 1)
        .await
        .expect("the claim should succeed");

    // The receipt must prove the credit, not merely the service's word.
    assert_eq!(receipt.amount, PRIZE.to_string());
    assert_eq!(receipt.request_key, request_key);
    assert_eq!(
        receipt.recipient.account_id,
        inspection.recipient.account_id
    );
    assert_eq!(decimal(&receipt.balance_before), balance_before);
    assert_eq!(decimal(&receipt.balance_after), balance_before + PRIZE);
    assert_eq!(receipt.tx_hash.len(), 64);
    assert_eq!(receipt.stale_challenge_retries, 0);
    println!(
        "receipt: tx={} explorer={}",
        receipt.tx_hash,
        client.network().explorer_transaction_url(&receipt.tx_hash)
    );

    // Verify independently rather than trusting the receipt we just produced.
    let after = client
        .inspect_recipient(&recipient)
        .await
        .expect("recipient re-inspection");
    let balance_after = decimal(&after.balance.expect("balance"));
    assert_eq!(balance_after, balance_before + PRIZE);

    let info_after = client.get_info().await.expect("faucet info");
    let hot_after = decimal(&info_after.pool_balance);
    assert!(
        hot_before - hot_after >= PRIZE,
        "the hot account must have paid at least the prize (before {hot_before}, after {hot_after})"
    );
    println!("after: recipient={balance_after} hot={hot_after}");

    // Replaying the same key must not produce a second claim, locally or at
    // the service.
    let replayed = client
        .request_drop(&recipient, request_key, 2)
        .await
        .expect("a replayed key returns the original receipt");
    assert_eq!(replayed.tx_hash, receipt.tx_hash);
    let unchanged = client
        .inspect_recipient(&recipient)
        .await
        .expect("recipient re-inspection");
    assert_eq!(decimal(&unchanged.balance.expect("balance")), balance_after);
}
