//! The claim-service drop against a mock sequencer and a mock claim service.
//!
//! Both hosts are one `httpmock` server on loopback: JSON-RPC at `/`, the
//! claim service under `/v1/`. The client is built through
//! `FaucetClient::for_network` with a copy of the v0.3 row whose URLs are the
//! mock server's; that hook only ever accepts loopback or an enabled host, so
//! the host pin for every real URL is untouched by it.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use httpmock::{HttpMockRequest, HttpMockResponse, Mock, MockServer};

use crate::{
    client::{FaucetClient, FaucetPolicy},
    error::{DropPhase, ErrorCode},
    network::{ClaimMechanism, Network, LEZ_V03_TESTNET},
};

const RECIPIENT: &str = "Public/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r";
const RECIPIENT_ID: &str = "CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r";
const KEY: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const PRIZE: u128 = 100_000_000;
const TX: &str = "5fb9fd56265effefabe13021d4031b62ec915d83312c24bd8dfa130afe053dc7";

/// The deployed circuit's ImageID as the sequencer reports it, matching the
/// hex in the network row.
const PPC_WORDS: [u32; 8] = [
    3943929112, 3682468618, 2822605667, 1764660516, 290278025, 2512717227, 411457898, 556249114,
];

/// A v0.3 row whose sequencer and claim service are both the mock server.
fn mock_network(server: &MockServer) -> &'static Network {
    let base_url: &'static str = Box::leak(server.base_url().into_boxed_str());
    Box::leak(Box::new(Network {
        sequencer_url: base_url,
        sequencer_host: "127.0.0.1",
        claim: ClaimMechanism::ClaimApi {
            base_url,
            prize: PRIZE,
        },
        ..LEZ_V03_TESTNET
    }))
}

fn client(server: &MockServer) -> FaucetClient {
    FaucetClient::for_network(mock_network(server))
        .expect("loopback is accepted")
        .with_policy(FaucetPolicy {
            poll_interval: Duration::from_millis(5),
            reconciliation_deadline: Duration::from_millis(400),
            ..FaucetPolicy::default()
        })
}

fn rpc_result(value: serde_json::Value) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": value }).to_string())
        .build()
}

fn json(status: u16, value: serde_json::Value) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(value.to_string())
        .build()
}

fn rpc_method(request: &HttpMockRequest) -> String {
    serde_json::from_slice::<serde_json::Value>(request.body_ref())
        .ok()
        .and_then(|body| body["method"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A sequencer whose recipient balance moves from 0 to the prize once the
/// claim service has sent the transfer, and whose `getTransaction` answers
/// for our hash once `tx_live` is set.
struct Sequencer {
    tx_live: Arc<AtomicUsize>,
    balance_reads: Arc<AtomicUsize>,
}

async fn mock_sequencer<'a>(server: &'a MockServer, credited: bool) -> (Mock<'a>, Sequencer) {
    let tx_live = Arc::new(AtomicUsize::new(0));
    let balance_reads = Arc::new(AtomicUsize::new(0));
    let state = Sequencer {
        tx_live: Arc::clone(&tx_live),
        balance_reads: Arc::clone(&balance_reads),
    };
    let mock = server
        .mock_async(move |when, then| {
            when.method("POST").path("/");
            let tx_live = Arc::clone(&tx_live);
            let balance_reads = Arc::clone(&balance_reads);
            then.respond_with(move |request: &HttpMockRequest| {
                match rpc_method(request).as_str() {
                    "getProgramIds" => rpc_result(serde_json::json!({
                        "privacy_preserving_circuit": PPC_WORDS,
                    })),
                    "getAccountBalance" => {
                        balance_reads.fetch_add(1, Ordering::SeqCst);
                        let live = tx_live.load(Ordering::SeqCst) > 0;
                        rpc_result(serde_json::json!(if live && credited { PRIZE } else { 0 }))
                    }
                    "getTransaction" => {
                        let live = tx_live.load(Ordering::SeqCst) > 0;
                        rpc_result(if live {
                            serde_json::json!({ "hash": TX })
                        } else {
                            serde_json::Value::Null
                        })
                    }
                    _ => json(404, serde_json::json!({ "error": "no such method" })),
                }
            });
        })
        .await;
    (mock, state)
}

fn claim_record(status: &str, tx_hash: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "request_id": KEY,
        "recipient": RECIPIENT,
        "status": status,
        "prize": PRIZE.to_string(),
        "tx_hash": tx_hash,
        "balance_before": null,
        "balance_after": null,
        "error": null,
        "created_at": "2026-10-22T00:00:00Z",
        "updated_at": "2026-10-22T00:00:00Z",
    })
}

#[tokio::test]
async fn info_reports_the_hot_balance_as_the_pool_and_the_service_as_the_mechanism() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    let info_mock = server
        .mock_async(|when, then| {
            when.method("GET").path("/v1/info");
            then.status(200).json_body(serde_json::json!({
                "network": "lez-v03-testnet",
                "sequencer_url": "https://testnet.lez.logos.co",
                "lez_version": "v0.3.1",
                "prize": PRIZE.to_string(),
                "decimals": 9,
                "hot_account": "Public/EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7",
                "hot_balance": "1234567890",
                "claims_remaining": 7,
                "paused": false,
                "limits": { "per_recipient_per_day": 1, "per_ip_per_day": 2, "per_hour": 3, "per_day": 10 },
            }));
        })
        .await;

    let info = client(&server).get_info().await.expect("info");
    info_mock.assert_async().await;
    assert_eq!(info.network, "lez-v03-testnet");
    assert_eq!(info.claim_mechanism, "claim_api");
    assert_eq!(info.decimals, 9);
    assert_eq!(info.prize_amount, "100000000");
    assert_eq!(info.pool_balance, "1234567890");
    assert_eq!(info.claims_remaining, "7");
    assert!(info.can_claim);
    assert!(!info.paused);
    assert_eq!(info.pinata_account, None);
    assert_eq!(info.difficulty_bytes, None);
    assert_eq!(info.claim_api_url, Some(server.base_url()).as_deref());

    // The wire shape the module and the view read.
    let wire = serde_json::to_value(&info).unwrap();
    assert_eq!(wire["claim_mechanism"], "claim_api");
    assert_eq!(
        wire["program_fingerprint"]["privacy_preserving_circuit"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert!(wire.get("pinata_account").is_none());
    assert!(wire.get("difficulty_bytes").is_none());
}

#[tokio::test]
async fn a_paused_or_mismatched_service_blocks_claims_before_anything_is_sent() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    let paused = serde_json::json!({
        "network": "lez-v03-testnet", "prize": PRIZE.to_string(), "decimals": 9,
        "hot_balance": "5000000000", "claims_remaining": 3, "paused": true,
    });
    let mock = server
        .mock_async(|when, then| {
            when.method("GET").path("/v1/info");
            then.status(200).json_body(paused);
        })
        .await;
    let info = client(&server).get_info().await.expect("info");
    assert!(!info.can_claim);
    assert!(info.paused);
    assert_eq!(info.blocked_reason, Some("faucet_paused"));
    mock.delete_async().await;

    // A service paying a different amount than this build's row would make
    // every receipt unprovable, so it is refused up front.
    let other_prize = serde_json::json!({
        "network": "lez-v03-testnet", "prize": "50000000", "decimals": 9,
        "hot_balance": "5000000000", "claims_remaining": 3, "paused": false,
    });
    server
        .mock_async(|when, then| {
            when.method("GET").path("/v1/info");
            then.status(200).json_body(other_prize);
        })
        .await;
    let info = client(&server).get_info().await.expect("info");
    assert_eq!(info.blocked_reason, Some("prize_mismatch"));
}

#[tokio::test]
async fn a_wrong_circuit_id_is_a_fingerprint_mismatch_and_nothing_else_runs() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method("POST").path("/");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "privacy_preserving_circuit": [1, 2, 3, 4, 5, 6, 7, 8] },
            }));
        })
        .await;
    let claim = server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(202).json_body(claim_record("queued", None));
        })
        .await;

    let error = client(&server)
        .request_drop(RECIPIENT, KEY, 1)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ProgramFingerprintMismatch);
    assert_eq!(error.phase, Some(DropPhase::VerifyingPrograms));
    assert_eq!(claim.calls_async().await, 0);
}

#[tokio::test]
async fn inspection_on_v03_reads_a_balance_and_needs_no_initialization() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    let inspection = client(&server)
        .inspect_recipient(RECIPIENT_ID)
        .await
        .expect("inspection");
    assert_eq!(inspection.recipient.address, RECIPIENT);
    assert_eq!(inspection.eligibility, crate::client::Eligibility::Eligible);
    assert_eq!(inspection.balance.as_deref(), Some("0"));
    assert_eq!(inspection.program_owner, None);
    assert_eq!(inspection.initialization_command, None);

    // A private address never reaches either host.
    let error = client(&server)
        .inspect_recipient(&format!("Private/{RECIPIENT_ID}"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidPublicAccountId);
}

#[tokio::test]
async fn the_happy_path_proves_the_credit_on_chain_and_replays_under_the_same_key() {
    let server = MockServer::start_async().await;
    let (_sequencer, sequencer) = mock_sequencer(&server, true).await;

    // The service accepts, then reports the hash once the worker has sent.
    let tx_live = Arc::clone(&sequencer.tx_live);
    let posts = Arc::new(AtomicUsize::new(0));
    let posted = Arc::clone(&posts);
    let claim = server
        .mock_async(move |when, then| {
            when.method("POST").path("/v1/claim").json_body_includes(
                serde_json::json!({ "recipient": RECIPIENT, "request_id": KEY }).to_string(),
            );
            then.respond_with(move |_: &HttpMockRequest| {
                posted.fetch_add(1, Ordering::SeqCst);
                json(202, claim_record("queued", None))
            });
        })
        .await;
    let status = server
        .mock_async(move |when, then| {
            when.method("GET").path(format!("/v1/claim/{KEY}"));
            then.respond_with(move |_: &HttpMockRequest| {
                // First poll: still queued. Then the transfer goes out and the
                // chain starts answering for it.
                if tx_live.fetch_add(1, Ordering::SeqCst) == 0 {
                    json(202, claim_record("queued", None))
                } else {
                    json(202, claim_record("submitted", Some(TX)))
                }
            });
        })
        .await;

    let client = client(&server);
    let receipt = client
        .request_drop(RECIPIENT, KEY, 1)
        .await
        .expect("the drop should be credited");
    assert_eq!(receipt.amount, PRIZE.to_string());
    assert_eq!(receipt.balance_before, "0");
    assert_eq!(receipt.balance_after, PRIZE.to_string());
    assert_eq!(receipt.tx_hash, TX);
    assert_eq!(receipt.stale_challenge_retries, 0);
    assert_eq!(receipt.recipient.address, RECIPIENT);
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "the service is asked exactly once"
    );
    assert!(status.calls_async().await >= 2);
    assert!(
        sequencer.balance_reads.load(Ordering::SeqCst) >= 2,
        "balance read before and after"
    );

    // The same key replays the receipt without asking the service again.
    let replayed = client
        .request_drop(RECIPIENT, KEY, 2)
        .await
        .expect("replay");
    assert_eq!(replayed, receipt);
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    claim.assert_async().await;

    // The same key for another address is a conflict, locally.
    let other = "Public/EfQhKQAkX2FJiwNii2WFQsGndjvF1Mzd7RuVe7QdPLw7";
    assert_eq!(
        client.request_drop(other, KEY, 3).await.unwrap_err().code,
        ErrorCode::IdempotencyConflict
    );
    assert_eq!(posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_services_word_is_not_proof_without_the_chain() {
    // The service says "included" but the chain never shows the transaction
    // or the credit. No receipt may come out of that.
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(200)
                .json_body(claim_record("included", Some(TX)));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path(format!("/v1/claim/{KEY}"));
            then.status(200)
                .json_body(claim_record("included", Some(TX)));
        })
        .await;

    let client = client(&server);
    let error = client.request_drop(RECIPIENT, KEY, 1).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::OutcomeUnknown);
    assert_eq!(error.phase, Some(DropPhase::Reconciling));
    let details = error.details.unwrap();
    assert_eq!(details["tx_hash"], TX);
    assert_eq!(details["balance_before"], "0");
    assert_eq!(details["recipient"]["address"], RECIPIENT);
    assert!(details.get("submitted_challenge_fingerprint").is_none());

    // And the recipient is now off limits for this session.
    let again = client
        .request_drop(RECIPIENT, "9c1b0a77-5d2e-4f61-8a0b-1c2d3e4f5a6b", 2)
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::RecipientUnreconciled);
}

#[tokio::test]
async fn a_rate_limited_refusal_is_terminal_and_carries_the_retry_delay() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    let limited = server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(429).json_body(serde_json::json!({
                "error": {
                    "code": "rate_limited",
                    "message": "one claim per recipient per day",
                    "retryable": false,
                    "details": { "retry_after_seconds": 86000 },
                }
            }));
        })
        .await;
    let status = server
        .mock_async(|when, then| {
            when.method("GET").path(format!("/v1/claim/{KEY}"));
            then.status(404).json_body(serde_json::json!({
                "error": { "code": "unknown_job", "message": "never seen", "retryable": false }
            }));
        })
        .await;

    let client = client(&server);
    let error = client.request_drop(RECIPIENT, KEY, 1).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::RateLimited);
    assert!(!error.retryable);
    assert_eq!(error.phase, Some(DropPhase::Submitting));
    assert_eq!(error.details.unwrap()["retry_after_seconds"], 86000);
    assert!(
        !error.message.contains("per recipient per day"),
        "the service's own sentence is never echoed"
    );
    // A refusal records nothing, so nothing is reconciled.
    assert_eq!(status.calls_async().await, 0);

    // Not an unproven outcome: the same recipient may be asked for again
    // under a new key.
    limited.delete_async().await;
    let paused = server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(503).json_body(serde_json::json!({
                "error": { "code": "faucet_paused", "message": "paused", "retryable": false }
            }));
        })
        .await;
    let error = client
        .request_drop(RECIPIENT, "9c1b0a77-5d2e-4f61-8a0b-1c2d3e4f5a6b", 2)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FaucetPaused);
    paused.assert_async().await;
}

#[tokio::test]
async fn a_lost_post_is_reconciled_through_the_services_ledger() {
    // The POST times out, but the service did record the claim: the drop
    // must find it by key and finish from there rather than report a loss.
    let server = MockServer::start_async().await;
    let (_sequencer, sequencer) = mock_sequencer(&server, true).await;
    server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(500).body("gateway exploded");
        })
        .await;
    let tx_live = Arc::clone(&sequencer.tx_live);
    server
        .mock_async(move |when, then| {
            when.method("GET").path(format!("/v1/claim/{KEY}"));
            then.respond_with(move |_: &HttpMockRequest| {
                tx_live.fetch_add(1, Ordering::SeqCst);
                json(200, claim_record("included", Some(TX)))
            });
        })
        .await;

    let receipt = client(&server)
        .request_drop(RECIPIENT, KEY, 1)
        .await
        .expect("recovered through the ledger");
    assert_eq!(receipt.tx_hash, TX);
    assert_eq!(receipt.balance_after, PRIZE.to_string());
}

#[tokio::test]
async fn a_lost_post_the_service_never_recorded_is_retryable_and_sent_nothing() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(502).body("bad gateway");
        })
        .await;
    let status = server
        .mock_async(|when, then| {
            when.method("GET").path_prefix("/v1/claim/");
            then.status(404).json_body(serde_json::json!({
                "error": { "code": "unknown_job", "message": "never seen", "retryable": false }
            }));
        })
        .await;

    let client = client(&server);
    let error = client.request_drop(RECIPIENT, KEY, 1).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::ClaimApiUnavailable);
    assert!(error.retryable);
    assert_eq!(error.phase, Some(DropPhase::Submitting));
    // Believed only once it has repeated.
    assert!(status.calls_async().await >= 3);
    // Nothing was sent, so the recipient is not marked unreconciled.
    let again = client
        .request_drop(RECIPIENT, "9c1b0a77-5d2e-4f61-8a0b-1c2d3e4f5a6b", 2)
        .await
        .unwrap_err();
    assert_eq!(again.code, ErrorCode::ClaimApiUnavailable);
}

#[tokio::test]
async fn a_service_failure_before_signing_is_terminal_and_names_the_cause() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    let failed = serde_json::json!({
        "request_id": KEY, "recipient": RECIPIENT, "status": "failed",
        "tx_hash": null,
        "error": { "code": "submission_rejected", "message": "nonce" },
    });
    server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(200).json_body(failed);
        })
        .await;
    let error = client(&server)
        .request_drop(RECIPIENT, KEY, 1)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::SubmissionRejected);
    assert_eq!(error.phase, Some(DropPhase::Submitting));
}

#[tokio::test]
async fn the_deadline_makes_a_pending_claim_honestly_unknown() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(202).json_body(claim_record("queued", None));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path(format!("/v1/claim/{KEY}"));
            then.status(202).json_body(claim_record("queued", None));
        })
        .await;

    let error = client(&server)
        .request_drop(RECIPIENT, KEY, 1)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::OutcomeUnknown);
    assert!(!error.retryable);
    let details = error.details.unwrap();
    assert!(
        details.get("tx_hash").is_none(),
        "no hash was ever reported"
    );
    assert_eq!(details["balance_before"], "0");
}

#[tokio::test]
async fn cancelling_before_submission_sends_nothing_and_after_it_is_ignored() {
    let server = MockServer::start_async().await;
    let (_sequencer, _) = mock_sequencer(&server, false).await;
    let claim = server
        .mock_async(|when, then| {
            when.method("POST").path("/v1/claim");
            then.status(202).json_body(claim_record("queued", None));
        })
        .await;

    let client = client(&server);
    client.cancellation().request(7);
    let error = client.request_drop(RECIPIENT, KEY, 7).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(claim.calls_async().await, 0);
    assert_eq!(
        client.current_phase(7),
        None,
        "the phase channel is cleared"
    );
}
