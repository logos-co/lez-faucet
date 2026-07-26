//! Live public-testnet tests.
//!
//! These talk to the real LEZ public testnet, so they are `#[ignore]`d and are
//! never part of a normal `cargo test` run.
//!
//! The read-only tests are safe to run at any time. The single write test
//! spends 150 LEZ from a finite community pool and is additionally gated on an
//! explicit destination address supplied through the environment, so that no
//! transaction can be submitted without someone naming the recipient on
//! purpose.

use lez_faucet_ffi::{
    client::{Eligibility, FaucetClient},
    error::ErrorCode,
};

const SEQUENCER: &str = "https://testnet.lez.logos.co";

/// Pinned at the revision this client is built against.
const PINATA_ACCOUNT: &str = "EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7";
const PRIZE: u128 = 150;

fn client() -> FaucetClient {
    FaucetClient::new(SEQUENCER).expect("the pinned sequencer URL must be accepted")
}

fn decimal(value: &str) -> u128 {
    value.parse().expect("balances are canonical decimal strings")
}

#[tokio::test]
#[ignore = "talks to the live public testnet"]
async fn faucet_info_matches_the_pinned_protocol() {
    let info = client().get_info().await.expect("faucet info");

    assert_eq!(info.network, "lez-public-testnet");
    assert_eq!(info.prize_amount, "150");
    assert_eq!(info.pinata_account.account_id, PINATA_ACCOUNT);
    assert_eq!(
        info.pinata_account.address,
        format!("Public/{PINATA_ACCOUNT}")
    );

    // The fingerprint gate passed, which is the whole point of calling this
    // before anything is ever submitted.
    assert_eq!(info.program_fingerprint.pinata.len(), 64);
    assert_eq!(info.program_fingerprint.authenticated_transfer.len(), 64);

    // Genesis difficulty is 3. A different value is not necessarily wrong, but
    // it is a protocol change we want to notice rather than mine through.
    assert_eq!(
        info.difficulty_bytes, 3,
        "difficulty changed from the pinned genesis value"
    );
    assert_eq!(info.effective_difficulty_bits, 24);

    // The pool only ever moves in multiples of the prize.
    let pool = decimal(&info.pool_balance);
    assert_eq!(pool % PRIZE, 0, "pool balance is not a multiple of the prize");
    assert_eq!(info.claims_remaining, (pool / PRIZE).to_string());
    assert_eq!(info.can_claim, pool >= PRIZE && info.blocked_reason.is_none());

    println!(
        "pool={pool} claims_remaining={} difficulty={}",
        info.claims_remaining, info.difficulty_bytes
    );
}

#[tokio::test]
#[ignore = "talks to the live public testnet"]
async fn recipient_states_are_distinguished_against_live_accounts() {
    let client = client();

    // The Piñata account itself exists but is owned by the Piñata program, so
    // it is a real, live example of a wrong-owner recipient.
    let pinata = client
        .inspect_recipient(PINATA_ACCOUNT)
        .await
        .expect("inspection should succeed even for an ineligible account");
    assert_eq!(pinata.eligibility, Eligibility::WrongOwner);
    assert!(pinata.balance.is_none());
}

#[tokio::test]
#[ignore = "talks to the live public testnet"]
async fn malformed_and_private_addresses_never_reach_the_network() {
    let client = client();
    let bomb = "1".repeat(133);
    for bad in [
        "Private/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r",
        "not-an-account",
        "",
        // The input that panics the pinned base58 decoder.
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
/// Without it the test reports that it did nothing and passes, so it can never
/// spend a claim by accident.
#[tokio::test]
#[ignore = "submits a real transaction and spends 150 LEZ from a finite pool"]
async fn one_authorized_claim_credits_exactly_the_prize() {
    let Ok(recipient) = std::env::var("LEZ_FAUCET_LIVE_RECIPIENT") else {
        println!(
            "skipped: set LEZ_FAUCET_LIVE_RECIPIENT to the approved destination address to run this"
        );
        return;
    };

    let client = client();

    // Independent pre-state, read before anything is built.
    let info_before = client.get_info().await.expect("faucet info");
    let inspection = client
        .inspect_recipient(&recipient)
        .await
        .expect("recipient inspection");
    assert_eq!(
        inspection.eligibility,
        Eligibility::Eligible,
        "the destination must already be an initialized authenticated-transfer account; \
         the faucet must never initialize it on the owner's behalf"
    );
    let balance_before = decimal(
        &inspection
            .balance
            .clone()
            .expect("eligible accounts report a balance"),
    );
    let pool_before = decimal(&info_before.pool_balance);

    println!("before: recipient={balance_before} pool={pool_before}");

    let request_key = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    let receipt = client
        .request_drop(&recipient, request_key, 1)
        .await
        .expect("the claim should succeed");

    // The receipt must prove the credit, not merely the submission.
    assert_eq!(receipt.amount, "150");
    assert_eq!(receipt.request_key, request_key);
    assert_eq!(receipt.recipient.account_id, inspection.recipient.account_id);
    assert_eq!(decimal(&receipt.balance_before), balance_before);
    assert_eq!(decimal(&receipt.balance_after), balance_before + PRIZE);
    assert_eq!(
        receipt.tx_hash.len(),
        64,
        "a receipt always carries its transaction hash"
    );
    println!(
        "receipt: tx={} retries={}",
        receipt.tx_hash, receipt.stale_challenge_retries
    );

    // Verify independently rather than trusting the receipt we just produced.
    let after = client
        .inspect_recipient(&recipient)
        .await
        .expect("recipient re-inspection");
    let balance_after = decimal(&after.balance.expect("eligible accounts report a balance"));
    assert_eq!(
        balance_after,
        balance_before + PRIZE,
        "the recipient must have gained exactly the prize"
    );

    let info_after = client.get_info().await.expect("faucet info");
    let pool_after = decimal(&info_after.pool_balance);
    assert!(
        pool_before - pool_after >= PRIZE,
        "the pool must have paid at least the prize (before {pool_before}, after {pool_after})"
    );

    println!("after: recipient={balance_after} pool={pool_after}");

    // Replaying the same key must not produce a second claim.
    let replayed = client
        .request_drop(&recipient, request_key, 2)
        .await
        .expect("a replayed key returns the original receipt");
    assert_eq!(replayed.tx_hash, receipt.tx_hash);

    let unchanged = client
        .inspect_recipient(&recipient)
        .await
        .expect("recipient re-inspection");
    assert_eq!(
        decimal(&unchanged.balance.expect("balance")),
        balance_after,
        "replaying a request key must not move the balance again"
    );
}
