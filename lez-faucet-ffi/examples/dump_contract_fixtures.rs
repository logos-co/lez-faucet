//! Emit the exact JSON the core produces, so the UI can be tested against it.
//!
//! The layers are developed and tested separately; this is the artifact that
//! keeps them honest about the shape they actually exchange.
use lez_faucet_ffi::{
    address::PublicAddress,
    client::{DropReceipt, Eligibility, FaucetInfo, ProgramFingerprint, RecipientInspection},
    error::{ApiError, DropPhase, ErrorCode},
};

fn main() {
    let recipient = PublicAddress {
        account_id: "CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r".into(),
        address: "Public/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r".into(),
    };
    let pinata = PublicAddress {
        account_id: "EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7".into(),
        address: "Public/EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7".into(),
    };

    let fixtures = serde_json::json!({
        "faucet_info": FaucetInfo {
            network: "lez-public-testnet",
            sequencer_url: "https://testnet.lez.logos.co/".into(),
            pinata_account: pinata,
            prize_amount: "150".into(),
            pool_balance: "1476900".into(),
            claims_remaining: "9846".into(),
            difficulty_bytes: 3,
            effective_difficulty_bits: 24,
            can_claim: true,
            blocked_reason: None,
            program_fingerprint: ProgramFingerprint {
                authenticated_transfer: "dc".repeat(32),
                pinata: "e6".repeat(32),
            },
        },
        "inspection_eligible": RecipientInspection {
            recipient: recipient.clone(),
            eligibility: Eligibility::Eligible,
            balance: Some("6000".into()),
            program_owner: Some("dc".repeat(32)),
            initialization_command: None,
        },
        "inspection_uninitialized": RecipientInspection {
            recipient: recipient.clone(),
            eligibility: Eligibility::Uninitialized,
            balance: None,
            program_owner: None,
            initialization_command: Some(format!(
                "wallet auth-transfer init --account-id {}", recipient.address)),
        },
        "receipt": DropReceipt {
            request_key: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".into(),
            recipient: recipient.clone(),
            amount: "150".into(),
            balance_before: "6000".into(),
            balance_after: "6150".into(),
            tx_hash: "5f".repeat(32),
            stale_challenge_retries: 0,
        },
        "error_uninitialized": ApiError::new(
            ErrorCode::RecipientUninitialized, "not initialized")
            .at(DropPhase::InspectingRecipient),
        "error_outcome_unknown": ApiError::new(
            ErrorCode::OutcomeUnknown, "could not be confirmed in time")
            .at(DropPhase::Reconciling)
            .with_details(serde_json::json!({
                "outcome": "unknown",
                "recipient": recipient,
                "balance_before": "6000",
                "tx_hash": "5f".repeat(32),
                "submitted_challenge_fingerprint": "0011223344556677",
            })),
    });
    println!("{}", serde_json::to_string_pretty(&fixtures).unwrap());
}
