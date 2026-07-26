//! The stateless LEZ Faucet client.
//!
//! This client owns no wallet, no key material and no files. It reads public
//! chain state, builds one unsigned public transaction, submits it once, and
//! then proves — or refuses to claim — that the recipient gained exactly the
//! prize.
//!
//! A Piñata claim names both accounts as `PublicNoSign`, so the transaction
//! carries no signatures and no nonces. There is consequently nothing to sign
//! with and nothing to store.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use common::{transaction::LeeTransaction, HashType};
use lee::{
    program::Program,
    public_transaction::{Message, PublicTransaction, WitnessSet},
    Account, AccountId,
};
use sequencer_service_rpc::{RpcClient as _, SequencerClient, SequencerClientBuilder};
use serde::Serialize;

use crate::{
    address::{parse_public_address, PublicAddress},
    error::{ApiError, ApiResult, DropPhase, ErrorCode},
    solver::{self, Challenge},
};

/// The exact prize the deployed Piñata guest pays per successful claim.
pub const PRIZE: u128 = 150;

/// Default public testnet sequencer.
pub const DEFAULT_SEQUENCER_URL: &str = "https://testnet.lez.logos.co";

/// The only non-loopback host this client will talk to.
pub const PINNED_SEQUENCER_HOST: &str = "testnet.lez.logos.co";

/// Upper bound on retained idempotency records.
///
/// Records are **never evicted** to make room — exceeding the bound is an
/// error. Evicting a request key would let a replayed key be treated as new
/// and become a second claim, which is precisely the failure this ledger
/// exists to prevent.
const MAX_IDEMPOTENCY_RECORDS: usize = 4_096;

/// Client-side bounds. Compile-time policy; overridden only in tests.
#[derive(Debug, Clone)]
pub struct FaucetPolicy {
    pub poll_interval: Duration,
    pub reconciliation_deadline: Duration,
    pub max_stale_challenge_retries: usize,
    pub solve_deadline: Duration,
    pub max_solve_attempts: u64,
}

impl Default for FaucetPolicy {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            reconciliation_deadline: Duration::from_secs(300),
            max_stale_challenge_retries: 3,
            solve_deadline: Duration::from_secs(60),
            max_solve_attempts: 1 << 28,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProgramFingerprint {
    pub authenticated_transfer: String,
    pub pinata: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FaucetInfo {
    pub network: &'static str,
    pub sequencer_url: String,
    pub pinata_account: PublicAddress,
    pub prize_amount: String,
    pub pool_balance: String,
    pub claims_remaining: String,
    pub difficulty_bytes: u8,
    pub effective_difficulty_bits: u32,
    pub can_claim: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<&'static str>,
    pub program_fingerprint: ProgramFingerprint,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Eligibility {
    Eligible,
    Uninitialized,
    WrongOwner,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RecipientInspection {
    pub recipient: PublicAddress,
    pub eligibility: Eligibility,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialization_command: Option<String>,
}

/// Proof that one specific claim credited one specific recipient.
///
/// `tx_hash` is not optional. The client computes the transaction hash locally
/// before submitting, so it always knows which transaction is its own; a
/// receipt without one would be an assertion of success the client cannot back.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DropReceipt {
    pub request_key: String,
    pub recipient: PublicAddress,
    pub amount: String,
    pub balance_before: String,
    pub balance_after: String,
    pub tx_hash: String,
    pub stale_challenge_retries: usize,
}

#[must_use]
pub fn program_id_hex(program_id: [u32; 8]) -> String {
    let mut bytes = [0_u8; 32];
    for (index, word) in program_id.into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---- cancellation ----------------------------------------------------------

/// Cancellation scoped to a caller-supplied operation token.
///
/// Scoping matters: without it, a cancel that arrives late — after the user's
/// first request already finished — would silently abort the *next* request.
#[derive(Debug, Default)]
pub struct Cancellation {
    cancelled_token: AtomicU64,
}

impl Cancellation {
    pub fn request(&self, token: u64) {
        if token != 0 {
            self.cancelled_token.store(token, Ordering::SeqCst);
        }
    }

    #[must_use]
    pub fn is_cancelled(&self, token: u64) -> bool {
        token != 0 && self.cancelled_token.load(Ordering::SeqCst) == token
    }
}

// ---- idempotency -----------------------------------------------------------

#[derive(Debug, Clone)]
enum DropOutcome {
    Succeeded(Box<DropReceipt>),
    Failed(Box<ApiError>),
}

#[derive(Debug, Clone)]
struct IdempotencyRecord {
    account_id: String,
    outcome: Option<DropOutcome>,
}

/// Validate a request key.
///
/// Narrowed to an RFC 4122 lowercase UUID. The key crosses an FFI boundary and
/// is used as a map key; there is no reason to accept an arbitrary caller
/// string.
fn validate_request_key(key: &str) -> ApiResult<()> {
    let invalid = || {
        ApiError::new(
            ErrorCode::InvalidRequestKey,
            "The request key must be a lowercase UUID.",
        )
    };
    if key.len() != 36 {
        return Err(invalid());
    }
    for (index, byte) in key.bytes().enumerate() {
        let ok = match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        };
        if !ok {
            return Err(invalid());
        }
    }
    Ok(())
}

// ---- reconciliation --------------------------------------------------------

/// One round of observations about a submitted claim.
///
/// `None` means "could not be read this round", which is materially different
/// from a known negative and must never be treated as one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimObservation {
    /// Whether *our* transaction hash is present on chain.
    pub included: Option<bool>,
    pub balance: Option<u128>,
    /// Whether the global challenge has moved away from the one we solved.
    /// Read **before** `included`; see [`classify`].
    pub challenge_rotated: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDecision {
    /// Nothing conclusive yet.
    Continue,
    /// Our transaction is on chain and the recipient gained exactly the prize.
    Credited,
    /// Our transaction is on chain and provably credited nothing. Safe to
    /// re-solve: it has already executed and cannot execute again.
    CreditedNothing,
    /// The recipient's balance moved in a way that makes our credit unprovable.
    Unattributable,
}

/// Decide what a round of observations proves.
///
/// Every conclusion here rests on **positive** evidence about our own
/// transaction. In particular, retrying is authorised only by observing that
/// our transaction was included and did nothing — never by failing to observe
/// it.
///
/// The tempting alternative is to treat "absent from the chain, and the global
/// challenge has moved on" as proof that our transaction is inert. It is not
/// safe. Those two facts come from two separate RPC calls at two different
/// moments, so a transaction that was included between them looks identical to
/// one that was never included at all: index lag on the first read and a fresh
/// second read produce exactly that pattern with no adversary involved.
/// Retrying on it would submit a second claim for a transaction that had in
/// fact just credited the recipient — 300 LEZ from one button press.
///
/// Giving up that inference costs almost nothing, because a stale claim is
/// still a well-formed transaction: the sequencer accepts it, it is included,
/// and the guest simply returns without writing any output. The common case
/// therefore *does* produce the positive proof this function requires.
///
/// `balance` must be read after `included` within a round, so that a balance
/// equal to `balance_before` alongside a confirmed inclusion genuinely reflects
/// the state at or after that inclusion.
#[must_use]
pub fn classify(
    balance_before: u128,
    expected: u128,
    observation: ClaimObservation,
) -> ClaimDecision {
    let ClaimObservation {
        included, balance, ..
    } = observation;

    // Someone else moved this account. A balance can no longer attribute a
    // credit to our own transaction in either direction.
    if balance.is_some_and(|value| value != balance_before && value != expected) {
        return ClaimDecision::Unattributable;
    }

    if included != Some(true) {
        return ClaimDecision::Continue;
    }

    match balance {
        Some(value) if value == expected => ClaimDecision::Credited,
        // Our transaction ran and the recipient is unchanged, so it credited
        // nothing and never will: its solution was stale by the time it
        // executed. A fresh attempt cannot double-credit.
        Some(value) if value == balance_before => ClaimDecision::CreditedNothing,
        _ => ClaimDecision::Continue,
    }
}

// ---- client ----------------------------------------------------------------

pub struct FaucetClient {
    rpc: SequencerClient,
    sequencer_url: String,
    policy: FaucetPolicy,
    cancellation: Arc<Cancellation>,
    /// Set for the whole of a drop so only one mutating operation is ever live.
    ///
    /// An atomic rather than a mutex: the permit is held across `await`
    /// points, and a `MutexGuard` held across an await would make the drop
    /// future `!Send` and risk blocking a runtime worker.
    drop_active: Arc<AtomicBool>,
    idempotency: Mutex<HashMap<String, IdempotencyRecord>>,
    /// Recipients with an in-flight claim whose outcome was never proven.
    ///
    /// Request-key idempotency alone does not protect these. A user who is
    /// told "outcome unknown", re-checks a balance that has not moved yet, and
    /// clicks again produces a *fresh* key, which no tombstone covers and no
    /// live job blocks — and a second 150 LEZ is sent while the first claim is
    /// still pending. Entries are never removed for the life of the process.
    unreconciled: Mutex<std::collections::HashSet<String>>,
}

impl FaucetClient {
    /// Build a client.
    ///
    /// Performs no filesystem access and creates no key material. Only `https`
    /// is accepted, except for loopback, which tests and local development
    /// need.
    pub fn new(sequencer_url: &str) -> ApiResult<Self> {
        let parsed = url::Url::parse(sequencer_url).map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidSequencerUrl,
                "The sequencer URL is not a valid URL.",
            )
        })?;
        // Credentials in the URL would be sent to whatever host follows them,
        // and are never part of a legitimate sequencer address.
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(ApiError::new(
                ErrorCode::InvalidSequencerUrl,
                "The sequencer URL must not contain credentials.",
            ));
        }

        let host_is_loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
        if parsed.scheme() != "https" && !(parsed.scheme() == "http" && host_is_loopback) {
            return Err(ApiError::new(
                ErrorCode::InvalidSequencerUrl,
                "The sequencer URL must use https.",
            ));
        }

        // The host is pinned. A sequencer the user can redirect is a channel
        // for someone else to feed this client a fabricated pool, a fabricated
        // program fingerprint and a fabricated success, and to collect every
        // address typed into it. Loopback stays open for local development;
        // release builds reach exactly one host.
        let host = parsed.host_str().unwrap_or_default();
        if !host_is_loopback && host != PINNED_SEQUENCER_HOST {
            return Err(ApiError::new(
                ErrorCode::InvalidSequencerUrl,
                "That sequencer is not the LEZ public testnet this app is built for.",
            ));
        }

        let rpc = SequencerClientBuilder::default()
            .build(parsed.clone())
            .map_err(|_| {
                ApiError::new(
                    ErrorCode::InvalidSequencerUrl,
                    "Could not create a client for that sequencer URL.",
                )
            })?;

        Ok(Self {
            rpc,
            sequencer_url: parsed.to_string(),
            policy: FaucetPolicy::default(),
            cancellation: Arc::new(Cancellation::default()),
            drop_active: Arc::new(AtomicBool::new(false)),
            idempotency: Mutex::new(HashMap::new()),
            unreconciled: Mutex::new(std::collections::HashSet::new()),
        })
    }

    #[must_use]
    pub fn with_policy(mut self, policy: FaucetPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub fn cancellation(&self) -> Arc<Cancellation> {
        Arc::clone(&self.cancellation)
    }

    // -- reads ---------------------------------------------------------------

    async fn account(&self, id: AccountId, what: &str) -> ApiResult<Account> {
        self.rpc
            .get_account(id)
            .await
            .map_err(|error| ApiError::from_rpc(&format!("Reading the {what}"), &error))
    }

    /// Verify the deployed builtin program IDs match the ones pinned at build
    /// time. A mismatch means this app was built against a different LEZ than
    /// the one deployed, and nothing after it can be trusted.
    async fn verify_fingerprint(&self) -> ApiResult<ProgramFingerprint> {
        let remote = self
            .rpc
            .get_program_ids()
            .await
            .map_err(|error| ApiError::from_rpc("Reading the deployed program IDs", &error))?;

        for (name, local) in [
            (
                "authenticated_transfer",
                programs::authenticated_transfer().id(),
            ),
            ("pinata", programs::pinata().id()),
        ] {
            let deployed = remote.get(name).ok_or_else(|| {
                ApiError::new(
                    ErrorCode::ProgramFingerprintMismatch,
                    format!("The sequencer did not report a {name} program ID."),
                )
            })?;
            if deployed != &local {
                return Err(ApiError::new(
                    ErrorCode::ProgramFingerprintMismatch,
                    format!(
                        "This app was built for a different version of the LEZ {name} program. Update the app."
                    ),
                ));
            }
        }

        Ok(ProgramFingerprint {
            authenticated_transfer: program_id_hex(programs::authenticated_transfer().id()),
            pinata: program_id_hex(programs::pinata().id()),
        })
    }

    /// Read the Piñata account and validate that it is what we expect.
    async fn pinata_state(&self) -> ApiResult<(Account, Challenge)> {
        let account = self
            .account(
                system_accounts::pinata_account_id(),
                "Piñata faucet account",
            )
            .await?;
        if account.program_owner != programs::pinata().id() {
            return Err(ApiError::new(
                ErrorCode::PinataWrongOwner,
                "The Piñata faucet account is not owned by the expected program.",
            ));
        }
        let challenge = Challenge::parse(account.data.as_ref())?;
        Ok((account, challenge))
    }

    pub async fn get_info(&self) -> ApiResult<FaucetInfo> {
        let fingerprint = self.verify_fingerprint().await?;
        let (pinata, challenge) = self.pinata_state().await?;

        let pool_balance = pinata.balance;
        let claims_remaining = pool_balance / PRIZE;
        let unsupported = challenge.ensure_supported().is_err();
        let depleted = pool_balance < PRIZE;

        // Order matters for the message the user sees: a depleted pool is the
        // more actionable explanation, so report it first.
        let blocked_reason = if depleted {
            Some("pool_depleted")
        } else if unsupported {
            Some("unsupported_difficulty")
        } else {
            None
        };

        Ok(FaucetInfo {
            network: "lez-public-testnet",
            sequencer_url: self.sequencer_url.clone(),
            pinata_account: PublicAddress::new(system_accounts::pinata_account_id()),
            prize_amount: PRIZE.to_string(),
            pool_balance: pool_balance.to_string(),
            claims_remaining: claims_remaining.to_string(),
            difficulty_bytes: challenge.difficulty,
            effective_difficulty_bits: u32::from(challenge.difficulty) * 8,
            can_claim: blocked_reason.is_none(),
            blocked_reason,
            program_fingerprint: fingerprint,
        })
    }

    pub async fn inspect_recipient(&self, address: &str) -> ApiResult<RecipientInspection> {
        let (account_id, recipient) = parse_public_address(address)?;
        let account = self.account(account_id, "recipient account").await?;
        Ok(inspect(recipient, &account))
    }

    // -- the drop ------------------------------------------------------------

    /// Request exactly one Faucet credit for `address`.
    ///
    /// At most one on-chain claim results from one `(request_key, address)`
    /// pair for the lifetime of this process, whatever the caller does.
    pub async fn request_drop(
        &self,
        address: &str,
        request_key: &str,
        token: u64,
    ) -> ApiResult<DropReceipt> {
        validate_request_key(request_key)?;
        let (account_id, recipient) =
            parse_public_address(address).map_err(|error| error.at(DropPhase::ValidatingInput))?;

        // Replay check happens before the permit so a repeat of a finished
        // request answers immediately instead of queueing behind a live one.
        if let Some(replayed) = self.replay(request_key, &recipient)? {
            return replayed;
        }
        self.ensure_reconciled(&recipient)?;

        let _permit = DropPermit::acquire(&self.drop_active).ok_or_else(|| {
            ApiError::new(
                ErrorCode::DropInProgress,
                "A faucet request is already running. Wait for it to finish.",
            )
        })?;

        // Re-check under the permit: a duplicate that raced the check above
        // must not start a second drop.
        if let Some(replayed) = self.replay(request_key, &recipient)? {
            return replayed;
        }
        self.ensure_reconciled(&recipient)?;
        // The key is reserved *before* anything is submitted, so a crash after
        // submission still leaves the key un-replayable in this session.
        self.reserve(request_key, &recipient)?;

        let outcome = self
            .run_drop(account_id, &recipient, request_key, token)
            .await;
        self.record(request_key, &outcome);
        outcome
    }

    /// Return a stored outcome for a repeated key, or `None` if it is new.
    fn replay(
        &self,
        request_key: &str,
        recipient: &PublicAddress,
    ) -> ApiResult<Option<ApiResult<DropReceipt>>> {
        let ledger = self.idempotency.lock().map_err(|_| poisoned())?;
        let Some(record) = ledger.get(request_key) else {
            return Ok(None);
        };
        if record.account_id != recipient.account_id {
            return Err(ApiError::new(
                ErrorCode::IdempotencyConflict,
                "That request key was already used for a different address.",
            ));
        }
        Ok(match &record.outcome {
            Some(DropOutcome::Succeeded(receipt)) => Some(Ok((**receipt).clone())),
            Some(DropOutcome::Failed(error)) => Some(Err((**error).clone())),
            // Reserved but not finished: a concurrent duplicate.
            None => Some(Err(ApiError::new(
                ErrorCode::DropInProgress,
                "That request is already running.",
            ))),
        })
    }

    /// Refuse a drop to a recipient whose previous claim was never resolved.
    fn ensure_reconciled(&self, recipient: &PublicAddress) -> ApiResult<()> {
        let unreconciled = self.unreconciled.lock().map_err(|_| poisoned())?;
        if unreconciled.contains(&recipient.account_id) {
            return Err(ApiError::new(
                ErrorCode::RecipientUnreconciled,
                format!(
                    "An earlier request to {} was submitted but never confirmed. This app will not send another one to that account. Check the balance, and restart the app if you need to try again.",
                    recipient.address
                ),
            ));
        }
        Ok(())
    }

    fn mark_unreconciled(&self, account_id: &str) {
        if let Ok(mut unreconciled) = self.unreconciled.lock() {
            unreconciled.insert(account_id.to_owned());
        }
    }

    fn reserve(&self, request_key: &str, recipient: &PublicAddress) -> ApiResult<()> {
        let mut ledger = self.idempotency.lock().map_err(|_| poisoned())?;
        if ledger.len() >= MAX_IDEMPOTENCY_RECORDS {
            // Deliberately an error rather than an eviction: forgetting a key
            // would let it be replayed as a new claim.
            return Err(ApiError::new(
                ErrorCode::JobLimitReached,
                "This session has handled too many requests. Restart the app.",
            ));
        }
        ledger.insert(
            request_key.to_owned(),
            IdempotencyRecord {
                account_id: recipient.account_id.clone(),
                outcome: None,
            },
        );
        Ok(())
    }

    fn record(&self, request_key: &str, outcome: &ApiResult<DropReceipt>) {
        let Ok(mut ledger) = self.idempotency.lock() else {
            return;
        };
        if let Some(record) = ledger.get_mut(request_key) {
            record.outcome = Some(match outcome {
                Ok(receipt) => DropOutcome::Succeeded(Box::new(receipt.clone())),
                Err(error) => DropOutcome::Failed(Box::new(error.clone())),
            });
        }
    }

    async fn run_drop(
        &self,
        account_id: AccountId,
        recipient: &PublicAddress,
        request_key: &str,
        token: u64,
    ) -> ApiResult<DropReceipt> {
        let stop = || {
            if self.cancellation.is_cancelled(token) {
                Err(ApiError::new(
                    ErrorCode::Cancelled,
                    "The request was cancelled before anything was submitted.",
                ))
            } else {
                Ok(())
            }
        };

        stop()?;
        self.verify_fingerprint()
            .await
            .map_err(|error| error.at(DropPhase::VerifyingPrograms))?;

        stop()?;
        let recipient_account = self
            .account(account_id, "recipient account")
            .await
            .map_err(|error| error.at(DropPhase::InspectingRecipient))?;
        let inspection = inspect(recipient.clone(), &recipient_account);
        require_eligible(&inspection)?;
        let mut balance_before = recipient_account.balance;
        let mut expected = balance_before.checked_add(PRIZE).ok_or_else(|| {
            ApiError::new(
                ErrorCode::RecipientBalanceOverflow,
                "That account's balance cannot accept the prize.",
            )
        })?;

        for attempt in 0..=self.policy.max_stale_challenge_retries {
            stop()?;
            let (pinata, challenge) = self
                .pinata_state()
                .await
                .map_err(|error| error.at(DropPhase::FetchingChallenge))?;
            challenge
                .ensure_supported()
                .map_err(|error| error.at(DropPhase::FetchingChallenge))?;
            require_pool_can_pay(pinata.balance)?;

            stop()?;
            let cancellation = Arc::clone(&self.cancellation);
            let solution = solver::solve_with_deadline(
                challenge,
                self.policy.max_solve_attempts,
                self.policy.solve_deadline,
                Arc::new(move || cancellation.is_cancelled(token)),
                None,
            )
            .await
            .map_err(|error| error.at(DropPhase::Solving))?;

            // Re-establish every gate immediately before submitting. Up to a
            // minute of mining may have passed since they were last checked,
            // and each one is a reason not to send this transaction at all.
            stop()?;
            self.verify_fingerprint()
                .await
                .map_err(|error| error.at(DropPhase::RefreshingChallenge))?;
            let (pinata, latest) = self
                .pinata_state()
                .await
                .map_err(|error| error.at(DropPhase::RefreshingChallenge))?;
            // Do not spend a claim on work that went stale while we were mining.
            if latest != challenge {
                continue;
            }
            require_pool_can_pay(pinata.balance)?;

            // The recipient is re-read here too, so `balance_before` is the
            // value at the moment of submission. Using the value captured
            // before the solve would move the baseline that the success test
            // depends on if anything credited the account meanwhile.
            let current = self
                .account(account_id, "recipient account")
                .await
                .map_err(|error| error.at(DropPhase::RefreshingChallenge))?;
            require_eligible(&inspect(recipient.clone(), &current))?;
            if current.balance != balance_before {
                // The baseline moved. Start over rather than measure a credit
                // against a stale reference point; nothing has been sent yet.
                balance_before = current.balance;
                expected = balance_before.checked_add(PRIZE).ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::RecipientBalanceOverflow,
                        "That account's balance cannot accept the prize.",
                    )
                })?;
                continue;
            }

            // Last point at which cancelling is free. After this the chain
            // action cannot be taken back, only reconciled.
            stop()?;

            let transaction = build_claim(account_id, solution)?;
            let tx_hash = HashType(transaction.hash());
            let submission = self
                .rpc
                .send_transaction(LeeTransaction::Public(transaction))
                .await;

            // The hash was computed locally, so a lost response costs us
            // nothing: we can still ask whether our own transaction landed.
            // A *rejection*, however, is terminal and must not be reconciled
            // as though something might have been accepted.
            if let Err(error) = &submission {
                let mapped = ApiError::from_submission(error);
                if mapped.code == ErrorCode::SubmissionRejected {
                    return Err(mapped.at(DropPhase::Submitting));
                }
            }

            match self
                .reconcile(account_id, tx_hash, balance_before, expected, challenge)
                .await?
            {
                Reconciled::Credited => {
                    return Ok(DropReceipt {
                        request_key: request_key.to_owned(),
                        recipient: recipient.clone(),
                        amount: PRIZE.to_string(),
                        balance_before: balance_before.to_string(),
                        balance_after: expected.to_string(),
                        tx_hash: tx_hash.to_string(),
                        stale_challenge_retries: attempt,
                    });
                }
                // Our transaction has already executed and credited nothing, so
                // it cannot execute again. This is the one case in which a
                // second submission is provably safe.
                Reconciled::CreditedNothing => continue,
                Reconciled::Unknown(error) => {
                    self.mark_unreconciled(&recipient.account_id);
                    return Err(error);
                }
            }
        }

        Err(ApiError::new(
            ErrorCode::StaleChallengeExhausted,
            "Other claimants kept winning the challenge first. Nothing was sent. Try again.",
        ))
    }

    /// Poll until the outcome of our submitted transaction is proven, or the
    /// deadline makes it honestly unknown.
    async fn reconcile(
        &self,
        account_id: AccountId,
        tx_hash: HashType,
        balance_before: u128,
        expected: u128,
        submitted: Challenge,
    ) -> ApiResult<Reconciled> {
        let deadline = tokio::time::Instant::now() + self.policy.reconciliation_deadline;

        // Backs off from the base interval so a long reconciliation does not
        // hammer the single shared public sequencer. Many clients reconciling
        // at once is the expected case during community testing.
        let mut interval = self.policy.poll_interval;
        let max_interval = Duration::from_secs(30);

        loop {
            // Inclusion first, then balance, so that a balance equal to
            // `balance_before` alongside a confirmed inclusion reflects the
            // state at or after that inclusion. The global challenge is
            // deliberately not consulted: it rotates on any claimant's success
            // and so proves nothing about ours.
            let included = match self.rpc.get_transaction(tx_hash).await {
                Ok(found) => Some(found.is_some()),
                Err(_) => None,
            };
            let balance = self
                .rpc
                .get_account(account_id)
                .await
                .ok()
                .map(|account| account.balance);

            match classify(
                balance_before,
                expected,
                ClaimObservation {
                    included,
                    balance,
                    challenge_rotated: None,
                },
            ) {
                ClaimDecision::Credited => return Ok(Reconciled::Credited),
                ClaimDecision::CreditedNothing => return Ok(Reconciled::CreditedNothing),
                ClaimDecision::Unattributable => {
                    return Ok(Reconciled::Unknown(unknown_outcome(
                        "Another transaction changed this account's balance while the claim was in flight, so this credit cannot be attributed to this request.",
                        balance_before,
                        tx_hash,
                        &submitted,
                    )));
                }
                ClaimDecision::Continue => {}
            }

            if tokio::time::Instant::now() >= deadline {
                return Ok(Reconciled::Unknown(unknown_outcome(
                    "The claim was submitted but its outcome could not be confirmed in time.",
                    balance_before,
                    tx_hash,
                    &submitted,
                )));
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(max_interval);
        }
    }
}

/// RAII holder for the single-mutating-operation permit.
struct DropPermit(Arc<AtomicBool>);

impl DropPermit {
    fn acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self(Arc::clone(flag)))
    }
}

impl Drop for DropPermit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

enum Reconciled {
    Credited,
    CreditedNothing,
    Unknown(ApiError),
}

fn poisoned() -> ApiError {
    ApiError::internal("internal client state was left inconsistent")
}

/// Build the claim transaction.
///
/// Both accounts are unsigned public participants, so the witness set and the
/// nonce list are empty. That is not a shortcut: at the pinned revision a
/// `PublicNoSign` account contributes neither a signature nor a nonce, and the
/// state machine requires only that the two lists have equal length.
fn build_claim(winner: AccountId, solution: u128) -> ApiResult<PublicTransaction> {
    let instruction_data = Program::serialize_instruction(solution)
        .map_err(|_| ApiError::internal("could not encode the proof-of-work solution"))?;
    let message = Message::new_preserialized(
        programs::pinata().id(),
        vec![system_accounts::pinata_account_id(), winner],
        Vec::new(),
        instruction_data,
    );
    Ok(PublicTransaction::new(
        message,
        WitnessSet::from_raw_parts(Vec::new()),
    ))
}

fn inspect(recipient: PublicAddress, account: &Account) -> RecipientInspection {
    if account == &Account::default() {
        let command = format!(
            "wallet auth-transfer init --account-id {}",
            recipient.address
        );
        return RecipientInspection {
            recipient,
            eligibility: Eligibility::Uninitialized,
            balance: None,
            program_owner: None,
            initialization_command: Some(command),
        };
    }
    if account.program_owner != programs::authenticated_transfer().id() {
        return RecipientInspection {
            recipient,
            eligibility: Eligibility::WrongOwner,
            balance: None,
            program_owner: Some(program_id_hex(account.program_owner)),
            initialization_command: None,
        };
    }
    RecipientInspection {
        recipient,
        eligibility: Eligibility::Eligible,
        balance: Some(account.balance.to_string()),
        program_owner: Some(program_id_hex(account.program_owner)),
        initialization_command: None,
    }
}

fn require_eligible(inspection: &RecipientInspection) -> ApiResult<()> {
    match inspection.eligibility {
        Eligibility::Eligible => Ok(()),
        Eligibility::Uninitialized => Err(ApiError::new(
            ErrorCode::RecipientUninitialized,
            format!(
                "{} exists but has not been initialized yet. Initialize it from the wallet that owns it, then try again. The faucet never needs its recovery phrase or private key.",
                inspection.recipient.address
            ),
        )
        .at(DropPhase::InspectingRecipient)
        .with_details(serde_json::json!({
            "initialization_command": inspection.initialization_command,
        }))),
        Eligibility::WrongOwner => Err(ApiError::new(
            ErrorCode::RecipientWrongOwner,
            format!(
                "{} is managed by a different program, so the faucet cannot fund it.",
                inspection.recipient.address
            ),
        )
        .at(DropPhase::InspectingRecipient)),
    }
}

/// A depleted pool is a hard gate, not a retryable condition.
///
/// It also fails differently on chain from a lost race: an underfunded pool
/// makes the guest panic and the transaction is rejected outright, whereas a
/// stale solution is included and quietly does nothing. Conflating the two
/// would let the client mine forever against an empty pool.
fn require_pool_can_pay(pool_balance: u128) -> ApiResult<()> {
    if pool_balance < PRIZE {
        return Err(ApiError::new(
            ErrorCode::PoolDepleted,
            "The faucet pool is empty. No more testnet LEZ can be claimed until it is refilled.",
        ));
    }
    Ok(())
}

fn unknown_outcome(
    message: &str,
    balance_before: u128,
    tx_hash: HashType,
    submitted: &Challenge,
) -> ApiError {
    ApiError::new(ErrorCode::OutcomeUnknown, message)
        .at(DropPhase::Reconciling)
        .with_details(serde_json::json!({
            "outcome": "unknown",
            "balance_before": balance_before.to_string(),
            "tx_hash": tx_hash.to_string(),
            "submitted_challenge_fingerprint": submitted.fingerprint(),
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEFORE: u128 = 6_000;
    const EXPECTED: u128 = BEFORE + PRIZE;

    fn observe(
        included: Option<bool>,
        balance: Option<u128>,
        rotated: Option<bool>,
    ) -> ClaimObservation {
        ClaimObservation {
            included,
            balance,
            challenge_rotated: rotated,
        }
    }

    fn decide(observation: ClaimObservation) -> ClaimDecision {
        classify(BEFORE, EXPECTED, observation)
    }

    #[test]
    fn success_requires_both_our_transaction_and_the_exact_credit() {
        assert_eq!(
            decide(observe(Some(true), Some(EXPECTED), Some(true))),
            ClaimDecision::Credited
        );
        // The credit alone is never enough: we have not seen our own
        // transaction, so we cannot claim it was ours.
        assert_eq!(
            decide(observe(Some(false), Some(EXPECTED), Some(true))),
            ClaimDecision::Continue
        );
        assert_eq!(
            decide(observe(None, Some(EXPECTED), Some(true))),
            ClaimDecision::Continue
        );
    }

    #[test]
    fn inclusion_alone_is_never_success() {
        // Included but the recipient did not gain the prize: whatever else
        // this is, it is not a success.
        assert_ne!(
            decide(observe(Some(true), Some(BEFORE), Some(false))),
            ClaimDecision::Credited
        );
        // Included but the balance could not be read: nothing is proven yet.
        assert_eq!(
            decide(observe(Some(true), None, None)),
            ClaimDecision::Continue
        );
    }

    #[test]
    fn retry_is_authorised_only_by_positive_proof_of_a_no_op() {
        // Our transaction ran and the recipient did not move: it credited
        // nothing and cannot run again.
        assert_eq!(
            decide(observe(Some(true), Some(BEFORE), Some(true))),
            ClaimDecision::CreditedNothing
        );
        assert_eq!(
            decide(observe(Some(true), Some(BEFORE), None)),
            ClaimDecision::CreditedNothing,
            "the challenge is irrelevant to this proof"
        );
    }

    #[test]
    fn absence_from_the_chain_never_authorises_a_second_submission() {
        // This is the double-credit trap. "Not found" plus "balance unchanged"
        // plus "challenge rotated" is exactly what a *successfully included*
        // transaction looks like when the transaction index lags the state
        // read by one poll. Concluding "stale, safe to retry" here would send a
        // second claim for a transaction that already paid out.
        for rotated in [Some(true), Some(false), None] {
            assert_eq!(
                decide(observe(Some(false), Some(BEFORE), rotated)),
                ClaimDecision::Continue,
                "absence must never authorise a retry (rotated: {rotated:?})"
            );
            assert_eq!(
                decide(observe(None, Some(BEFORE), rotated)),
                ClaimDecision::Continue,
                "an unreadable inclusion must never authorise a retry"
            );
        }
    }

    #[test]
    fn challenge_rotation_carries_no_evidentiary_weight() {
        // Every claimant races one global challenge, so rotation only says
        // "somebody won" — never "we did" or "we did not". No decision may
        // turn on it.
        for included in [Some(true), Some(false), None] {
            for balance in [Some(BEFORE), Some(EXPECTED), None] {
                let rotated = decide(observe(included, balance, Some(true)));
                let unrotated = decide(observe(included, balance, Some(false)));
                let unknown = decide(observe(included, balance, None));
                assert_eq!(rotated, unrotated);
                assert_eq!(rotated, unknown);
            }
        }
    }

    #[test]
    fn a_third_party_credit_is_never_reported_as_our_success() {
        // Balance moved by more than the prize: someone else funded this
        // account too, so our own credit cannot be proven either way.
        assert_eq!(
            decide(observe(Some(true), Some(BEFORE + 2 * PRIZE), Some(true))),
            ClaimDecision::Unattributable
        );
        // An unexpected decrease is equally unattributable.
        assert_eq!(
            decide(observe(Some(true), Some(BEFORE - 1), Some(true))),
            ClaimDecision::Unattributable
        );
    }

    #[test]
    fn nothing_is_concluded_from_a_round_that_read_nothing() {
        assert_eq!(decide(observe(None, None, None)), ClaimDecision::Continue);
    }

    #[test]
    fn request_keys_must_be_lowercase_uuids() {
        assert!(validate_request_key("3f2504e0-4f89-41d3-9a0c-0305e82c3301").is_ok());
        for bad in [
            "",
            "not-a-uuid",
            "3F2504E0-4F89-41D3-9A0C-0305E82C3301", // uppercase
            "3f2504e0-4f89-41d3-9a0c-0305e82c330",  // too short
            "3f2504e0-4f89-41d3-9a0c-0305e82c33011",
            "3f2504e04f8941d39a0c0305e82c3301",
            "3f2504e0_4f89-41d3-9a0c-0305e82c3301",
            "3f2504e0-4f89-41d3-9a0c-0305e82c330g",
        ] {
            assert_eq!(
                validate_request_key(bad).unwrap_err().code,
                ErrorCode::InvalidRequestKey,
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn sequencer_url_must_be_https_outside_loopback() {
        assert!(FaucetClient::new("https://testnet.lez.logos.co").is_ok());
        assert!(FaucetClient::new("http://localhost:3040").is_ok());
        assert!(FaucetClient::new("http://127.0.0.1:3040").is_ok());
        for bad in [
            "http://testnet.lez.logos.co",
            "ftp://testnet.lez.logos.co",
            "file:///etc/passwd",
            "not a url",
            "",
            // A redirectable sequencer would let someone else fabricate the
            // pool, the fingerprint and the success, and harvest addresses.
            "https://evil.example.com",
            "https://testnet.lez.logos.co.evil.example",
            "https://user:pw@testnet.lez.logos.co",
        ] {
            let error = FaucetClient::new(bad)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} must be rejected"));
            assert_eq!(error.code, ErrorCode::InvalidSequencerUrl, "{bad:?}");
        }
    }

    #[test]
    fn inspection_distinguishes_the_three_recipient_states() {
        let address = PublicAddress::new(system_accounts::pinata_account_id());

        let uninitialized = inspect(address.clone(), &Account::default());
        assert_eq!(uninitialized.eligibility, Eligibility::Uninitialized);
        assert!(uninitialized.balance.is_none());
        assert!(uninitialized
            .initialization_command
            .as_deref()
            .is_some_and(|command| command.contains("auth-transfer init")));

        let eligible_account = Account {
            program_owner: programs::authenticated_transfer().id(),
            balance: 6_000,
            ..Account::default()
        };
        let eligible = inspect(address.clone(), &eligible_account);
        assert_eq!(eligible.eligibility, Eligibility::Eligible);
        assert_eq!(eligible.balance.as_deref(), Some("6000"));
        assert!(eligible.initialization_command.is_none());

        let foreign = Account {
            program_owner: programs::pinata().id(),
            balance: 1,
            ..Account::default()
        };
        let wrong = inspect(address, &foreign);
        assert_eq!(wrong.eligibility, Eligibility::WrongOwner);
        assert!(wrong.program_owner.is_some());
    }

    #[test]
    fn only_an_eligible_recipient_may_start_a_drop() {
        let address = PublicAddress::new(system_accounts::pinata_account_id());
        let eligible_account = Account {
            program_owner: programs::authenticated_transfer().id(),
            ..Account::default()
        };

        assert!(require_eligible(&inspect(address.clone(), &eligible_account)).is_ok());
        assert_eq!(
            require_eligible(&inspect(address.clone(), &Account::default()))
                .unwrap_err()
                .code,
            ErrorCode::RecipientUninitialized
        );

        let foreign = Account {
            program_owner: programs::pinata().id(),
            balance: 1,
            ..Account::default()
        };
        assert_eq!(
            require_eligible(&inspect(address, &foreign))
                .unwrap_err()
                .code,
            ErrorCode::RecipientWrongOwner
        );
    }

    #[test]
    fn the_pool_gate_matches_the_guests_arithmetic() {
        assert!(require_pool_can_pay(PRIZE).is_ok());
        assert_eq!(
            require_pool_can_pay(PRIZE - 1).unwrap_err().code,
            ErrorCode::PoolDepleted
        );
        assert_eq!(
            require_pool_can_pay(0).unwrap_err().code,
            ErrorCode::PoolDepleted
        );
    }

    #[test]
    fn cancellation_is_scoped_to_its_own_operation() {
        let cancellation = Cancellation::default();
        cancellation.request(7);
        assert!(cancellation.is_cancelled(7));
        // A cancel aimed at an earlier operation must not stop a later one.
        assert!(!cancellation.is_cancelled(8));
        // Token 0 is the "no operation" sentinel and is never cancellable.
        assert!(!cancellation.is_cancelled(0));
    }

    #[test]
    fn the_claim_transaction_carries_no_signature_and_no_nonce() {
        let winner = system_accounts::pinata_account_id();
        let transaction = build_claim(winner, 25_385_721).unwrap();
        assert!(
            transaction
                .witness_set()
                .signatures_and_public_keys()
                .is_empty(),
            "a faucet claim must never be signed: there is no key to sign it with"
        );
        assert!(transaction.message().nonces.is_empty());
        assert_eq!(transaction.message().account_ids.len(), 2);
        assert_eq!(transaction.message().program_id, programs::pinata().id());
    }

    #[test]
    fn the_transaction_hash_is_deterministic_and_known_before_submission() {
        let winner = system_accounts::pinata_account_id();
        let first = build_claim(winner, 42).unwrap().hash();
        let second = build_claim(winner, 42).unwrap().hash();
        assert_eq!(
            first, second,
            "the hash must be a pure function of the transaction"
        );
        assert_ne!(first, build_claim(winner, 43).unwrap().hash());
    }

    #[test]
    fn balances_are_exact_decimal_strings_beyond_the_javascript_safe_range() {
        let mut account = Account {
            program_owner: programs::authenticated_transfer().id(),
            balance: u128::MAX,
            ..Account::default()
        };
        let inspection = inspect(
            PublicAddress::new(system_accounts::pinata_account_id()),
            &account,
        );
        assert_eq!(
            inspection.balance.as_deref(),
            Some("340282366920938463463374607431768211455")
        );

        account.balance = (1_u128 << 53) + 1;
        let inspection = inspect(
            PublicAddress::new(system_accounts::pinata_account_id()),
            &account,
        );
        assert_eq!(inspection.balance.as_deref(), Some("9007199254740993"));
    }

    #[test]
    fn unknown_outcome_details_carry_evidence_but_not_the_challenge() {
        let challenge = Challenge {
            difficulty: 3,
            seed: [0xAB; 32],
        };
        let error = unknown_outcome("lost", 6_000, HashType([7; 32]), &challenge);
        assert_eq!(error.code, ErrorCode::OutcomeUnknown);
        assert!(
            !error.retryable,
            "an unknown outcome must never invite a retry"
        );
        let details = error.details.unwrap();
        assert_eq!(details["outcome"], "unknown");
        assert_eq!(details["balance_before"], "6000");
        assert_eq!(
            details["submitted_challenge_fingerprint"],
            challenge.fingerprint()
        );
        let rendered = details.to_string();
        assert!(
            !rendered.contains("abababab"),
            "the raw seed must not appear in error details"
        );
    }
}
