//! Emit the exact JSON the core produces, so the UI can be tested against it.
//!
//! The layers are developed and tested separately; this is the artifact that
//! keeps them honest about the shape they actually exchange. Both enabled
//! networks are represented, because the view renders a different prize, unit
//! and stage rail for each and must read both shapes.
use lez_faucet_ffi::{
    address::PublicAddress,
    client::{DropReceipt, Eligibility, FaucetInfo, ProgramFingerprint, RecipientInspection},
    error::{ApiError, DropPhase, ErrorCode},
    network::{self, Network},
};

fn network_row(network: &Network) -> serde_json::Value {
    serde_json::json!({
        "id": network.id,
        "name": network.display_name,
        "sequencerUrl": network.sequencer_url,
        "explorerUrl": network.explorer_url,
        "lezVersion": network.lez_version,
        "prize": network.prize().to_string(),
        "decimals": network.decimals,
        "mechanism": network.claim_mechanism(),
        "claimApiUrl": network.claim_api_url(),
    })
}

fn main() {
    let recipient = PublicAddress {
        account_id: "CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r".into(),
        address: "Public/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r".into(),
    };
    let pinata = PublicAddress {
        account_id: "EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7".into(),
        address: "Public/EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7".into(),
    };
    let legacy = &network::LEZ_LEGACY_TESTNET;
    let v03 = &network::LEZ_V03_TESTNET;

    let fixtures = serde_json::json!({
        // The network table as the UI backend mirrors it, in display order.
        "networks": network::ENABLED.iter().map(network_row).collect::<Vec<_>>(),
        "default_network": network::DEFAULT.id,
        "faucet_info": FaucetInfo {
            network: legacy.id,
            network_name: legacy.display_name,
            lez_version: legacy.lez_version,
            explorer_url: legacy.explorer_url,
            sequencer_url: "https://testnet-legacy.lez.logos.co/".into(),
            claim_mechanism: legacy.claim_mechanism(),
            decimals: legacy.decimals,
            claim_api_url: None,
            pinata_account: Some(pinata),
            prize_amount: legacy.prize().to_string(),
            pool_balance: "1476900".into(),
            claims_remaining: "9846".into(),
            difficulty_bytes: Some(3),
            effective_difficulty_bits: Some(24),
            paused: false,
            can_claim: true,
            blocked_reason: None,
            program_fingerprint: ProgramFingerprint::ImageIds {
                authenticated_transfer: "dc".repeat(32),
                pinata: "e6".repeat(32),
            },
        },
        "faucet_info_v03": FaucetInfo {
            network: v03.id,
            network_name: v03.display_name,
            lez_version: v03.lez_version,
            explorer_url: v03.explorer_url,
            sequencer_url: "https://testnet.lez.logos.co/".into(),
            claim_mechanism: v03.claim_mechanism(),
            decimals: v03.decimals,
            claim_api_url: v03.claim_api_url(),
            pinata_account: None,
            prize_amount: v03.prize().to_string(),
            pool_balance: "1234567890".into(),
            claims_remaining: "7".into(),
            difficulty_bytes: None,
            effective_difficulty_bits: None,
            paused: false,
            can_claim: true,
            blocked_reason: None,
            program_fingerprint: ProgramFingerprint::PrivacyPreservingCircuit {
                privacy_preserving_circuit: "18".repeat(32),
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
        // v0.3 has no initialization and no owning program: every public id is
        // eligible, and an unseen one has a balance of 0.
        "inspection_v03": RecipientInspection {
            recipient: recipient.clone(),
            eligibility: Eligibility::Eligible,
            balance: Some("0".into()),
            program_owner: None,
            initialization_command: None,
        },
        "receipt": DropReceipt {
            request_key: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".into(),
            recipient: recipient.clone(),
            amount: legacy.prize().to_string(),
            balance_before: "6000".into(),
            balance_after: "6150".into(),
            tx_hash: "5f".repeat(32),
            stale_challenge_retries: 0,
        },
        "receipt_v03": DropReceipt {
            request_key: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".into(),
            recipient: recipient.clone(),
            amount: v03.prize().to_string(),
            balance_before: "250000000".into(),
            balance_after: "350000000".into(),
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
        "error_rate_limited": ApiError::new(
            ErrorCode::RateLimited,
            "The faucet service is limiting claims right now. Try again in about 24 hours.")
            .at(DropPhase::Submitting)
            .with_details(serde_json::json!({ "retry_after_seconds": 86000 })),
        "error_faucet_paused": ApiError::new(
            ErrorCode::FaucetPaused,
            "The faucet service is paused by its operator. Nothing was sent.")
            .at(DropPhase::Submitting),
        "error_claim_api_unavailable": ApiError::new(
            ErrorCode::ClaimApiUnavailable,
            "The faucet service could not be reached and has no record of this request. Nothing was sent.")
            .at(DropPhase::Submitting),
    });
    println!("{}", serde_json::to_string_pretty(&fixtures).unwrap());
}
