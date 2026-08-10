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
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
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

// ---- live phase ------------------------------------------------------------

/// Wire codes for [`DropPhase`], for the atomic in [`PhaseChannel`].
///
/// 0 is reserved for "no phase". The match is exhaustive, so adding a phase
/// without a code is a compile error rather than a phase the poller cannot see.
const fn phase_code(phase: DropPhase) -> u8 {
    match phase {
        DropPhase::ValidatingInput => 1,
        DropPhase::VerifyingPrograms => 2,
        DropPhase::InspectingRecipient => 3,
        DropPhase::FetchingChallenge => 4,
        DropPhase::Solving => 5,
        DropPhase::RefreshingChallenge => 6,
        DropPhase::Submitting => 7,
        DropPhase::Reconciling => 8,
    }
}

const fn phase_from_code(code: u8) -> Option<DropPhase> {
    Some(match code {
        1 => DropPhase::ValidatingInput,
        2 => DropPhase::VerifyingPrograms,
        3 => DropPhase::InspectingRecipient,
        4 => DropPhase::FetchingChallenge,
        5 => DropPhase::Solving,
        6 => DropPhase::RefreshingChallenge,
        7 => DropPhase::Submitting,
        8 => DropPhase::Reconciling,
        _ => return None,
    })
}

/// Lock-free live-phase channel for the single in-flight drop.
///
/// `lez_faucet_current_phase` polls this from another thread while
/// `lez_faucet_request_drop` is blocked, so a read must never take the drop
/// permit or any lock the drop holds. Two atomics suffice because the permit
/// already guarantees at most one drop is live: `token` names the operation
/// being reported and `phase` is where it has got to. The token is checked on
/// every read, so a reader racing an update can see "no phase" or a phase one
/// step stale, but never a phase attributed to a different operation — by the
/// time a new drop can publish, its predecessor has already cleared.
#[derive(Debug, Default)]
pub struct PhaseChannel {
    token: AtomicU64,
    phase: AtomicU8,
}

impl PhaseChannel {
    /// Publish `phase` for the operation named by `token`.
    ///
    /// Token 0 is the "no operation" sentinel and is never published.
    fn publish(&self, token: u64, phase: DropPhase) {
        if token == 0 {
            return;
        }
        // Phase first, then token: a reader that has just seen this token can
        // only read a phase this operation wrote.
        self.phase.store(phase_code(phase), Ordering::SeqCst);
        self.token.store(token, Ordering::SeqCst);
    }

    /// Clear the channel when the operation named by `token` ends.
    fn clear(&self, token: u64) {
        // Compare first, so a delayed clear cannot erase a later operation.
        let _ = self
            .token
            .compare_exchange(token, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// The live phase of `token`, or `None` for an unknown or finished one.
    #[must_use]
    pub fn current(&self, token: u64) -> Option<DropPhase> {
        if token == 0 || self.token.load(Ordering::SeqCst) != token {
            return None;
        }
        phase_from_code(self.phase.load(Ordering::SeqCst))
    }
}

/// Clears the phase channel however its drop ends, success or error.
struct PhaseGuard<'a> {
    channel: &'a PhaseChannel,
    token: u64,
}

impl Drop for PhaseGuard<'_> {
    fn drop(&mut self) {
        self.channel.clear(self.token);
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
    ///
    /// Meaningful only alongside an absent transaction and an unchanged
    /// balance; on its own it says nothing about our claim, because every
    /// claimant races the same challenge. See [`classify`].
    pub challenge_rotated: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimDecision {
    /// Nothing conclusive yet.
    Continue,
    /// Our transaction is on chain and the recipient gained exactly the prize.
    Credited,
    /// Our transaction lost the race and was discarded before reaching a block.
    /// It credited nothing and never can, so re-solving is safe.
    LostRace,
    /// The recipient's balance moved in a way that makes our credit unprovable.
    Unattributable,
}

/// Decide what a round of observations proves.
///
/// Two facts about the pinned sequencer drive every rule here, and both were
/// checked in its source rather than assumed.
///
/// **A losing claim is never included.** When the proof-of-work is stale the
/// guest returns without calling `ProgramOutput::write`, so it commits an empty
/// journal (`lee/state_machine/core/src/program/mod.rs:470` is the only journal
/// write). Decoding that empty journal fails, surfacing as
/// `ProgramExecutionFailed`, and `build_block_from_mempool` logs and `continue`s
/// past such a transaction rather than sealing it into a block
/// (`lez/sequencer/core/src/lib.rs`). So our transaction appearing on chain
/// *is* proof that it paid out, and a losing claim simply never appears.
///
/// **State is applied before the transaction becomes queryable.**
/// `apply_state_diff` runs while the block is still being assembled, and the
/// block — the thing `get_transaction` searches — is stored afterwards. A
/// credit is therefore visible in the balance no later than the transaction is
/// visible by hash, never after it.
///
/// That second fact is what makes the `LostRace` rule safe. Reading "absent"
/// and "challenge rotated" would otherwise be the classic double-credit trap:
/// two reads at two moments, where a transaction included between them looks
/// exactly like one never included. Requiring the balance to still equal
/// `balance_before` closes it — had our transaction credited, the balance would
/// already show it, because state leads the index.
///
/// **Both facts describe one sequencer's own block production, and v0.2.2
/// weakened them.** That release introduced a two-tier chain state with peer
/// block adoption and orphaning (`lez/chain_state/`). Reads answer from the
/// reorg-able head rather than a finalized tier, and the hash index behind
/// `get_transaction` is extended on every block but never pruned when one is
/// orphaned. So a transaction can read as included after the block carrying it
/// has been reverted, with the credit no longer in the balance.
///
/// This does not make the rules below unsafe, because they were already written
/// to refuse rather than guess: that combination returns `Unattributable`, which
/// reports an unknown outcome and submits nothing further. What it does mean is
/// that inclusion is evidence of payout rather than proof of it, and that a
/// `Credited` receipt describes the head at the moment it was read. Treating
/// `Credited` as final would need reads against a finalized tier, which the
/// current RPC surface does not expose.
#[must_use]
pub fn classify(
    balance_before: u128,
    expected: u128,
    observation: ClaimObservation,
) -> ClaimDecision {
    let ClaimObservation {
        included,
        balance,
        challenge_rotated,
    } = observation;

    // Someone else moved this account. A balance can no longer attribute a
    // credit to our own transaction in either direction.
    if balance.is_some_and(|value| value != balance_before && value != expected) {
        return ClaimDecision::Unattributable;
    }

    if included == Some(true) {
        return match balance {
            Some(value) if value == expected => ClaimDecision::Credited,
            // Included but no credit visible. Since v0.2.2 this is reachable:
            // the block carrying our transaction can be orphaned while the
            // hash index still answers "included". Either way the response is
            // the same — refuse to attribute, and never treat it as permission
            // to send a second claim.
            Some(_) => ClaimDecision::Unattributable,
            None => ClaimDecision::Continue,
        };
    }

    // Absent, the balance has not moved, and another claimant has taken the
    // challenge we solved for. Our transaction can only fail validation from
    // here, so it will be discarded and can never credit.
    if included == Some(false) && balance == Some(balance_before) && challenge_rotated == Some(true)
    {
        return ClaimDecision::LostRace;
    }

    ClaimDecision::Continue
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
    /// Where the in-flight drop currently is, for a poller on another thread.
    ///
    /// Deliberately a polled channel and not a callback: the previous release
    /// had a progress callback that re-entered C++ while the module held its
    /// operation mutex, and this ABI's freedom from function pointers is an
    /// asserted property.
    phase_channel: PhaseChannel,
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
            phase_channel: PhaseChannel::default(),
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

    /// The live phase of the drop named by `token`, if it is still running.
    ///
    /// Reads two atomics and returns. No permit, no lock, no `await`: safe to
    /// call from any thread while a drop is blocked inside [`Self::request_drop`].
    #[must_use]
    pub fn current_phase(&self, token: u64) -> Option<DropPhase> {
        self.phase_channel.current(token)
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
        // Each step publishes its phase before starting, so a poller on
        // another thread sees where the drop is rather than where it was. The
        // guard clears the channel on every exit path, so a finished token
        // reads as "no phase" rather than as wherever it happened to stop.
        let _phase = PhaseGuard {
            channel: &self.phase_channel,
            token,
        };
        let at = |phase: DropPhase| self.phase_channel.publish(token, phase);
        at(DropPhase::ValidatingInput);

        // Once anything has been submitted, no path may report a cancellation
        // or claim that nothing was sent: the chain action cannot be recalled,
        // only reconciled, and saying otherwise would hide a live transaction
        // from the user.
        let mut submitted = false;
        let stop = |submitted: bool| {
            if !submitted && self.cancellation.is_cancelled(token) {
                Err(ApiError::new(
                    ErrorCode::Cancelled,
                    "The request was cancelled before anything was submitted.",
                ))
            } else {
                Ok(())
            }
        };

        stop(submitted)?;
        at(DropPhase::VerifyingPrograms);
        self.verify_fingerprint()
            .await
            .map_err(|error| error.at(DropPhase::VerifyingPrograms))?;

        stop(submitted)?;
        at(DropPhase::InspectingRecipient);
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
            stop(submitted)?;
            at(DropPhase::FetchingChallenge);
            let (pinata, challenge) = self
                .pinata_state()
                .await
                .map_err(|error| error.at(DropPhase::FetchingChallenge))?;
            challenge
                .ensure_supported()
                .map_err(|error| error.at(DropPhase::FetchingChallenge))?;
            require_pool_can_pay(pinata.balance)?;

            stop(submitted)?;
            at(DropPhase::Solving);
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
            stop(submitted)?;
            at(DropPhase::RefreshingChallenge);
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
            stop(submitted)?;

            // Latched before the network call, not after: a submission whose
            // response is lost has still been submitted.
            at(DropPhase::Submitting);
            submitted = true;
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

            at(DropPhase::Reconciling);
            match self
                .reconcile(
                    account_id,
                    recipient,
                    tx_hash,
                    balance_before,
                    expected,
                    challenge,
                )
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
                // Our transaction was discarded before it could reach a block,
                // so it credited nothing and never can. This is the one case in
                // which a second submission is provably safe.
                Reconciled::LostRace => continue,
                Reconciled::Unknown(error) => {
                    self.mark_unreconciled(&recipient.account_id);
                    return Err(error);
                }
            }
        }

        Err(ApiError::new(
            ErrorCode::StaleChallengeExhausted,
            "Other claimants kept winning the proof-of-work race. Nothing was credited. You can try again.",
        ))
    }

    /// Poll until the outcome of our submitted transaction is proven, or the
    /// deadline makes it honestly unknown.
    async fn reconcile(
        &self,
        account_id: AccountId,
        recipient: &PublicAddress,
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
            // Only meaningful together with an unchanged balance; on its own
            // rotation says nothing about our claim, since every claimant
            // races the same challenge.
            let challenge_rotated = match self.pinata_state().await {
                Ok((_, current)) => Some(current != submitted),
                Err(_) => None,
            };

            match classify(
                balance_before,
                expected,
                ClaimObservation {
                    included,
                    balance,
                    challenge_rotated,
                },
            ) {
                ClaimDecision::Credited => return Ok(Reconciled::Credited),
                ClaimDecision::LostRace => return Ok(Reconciled::LostRace),
                ClaimDecision::Unattributable => {
                    return Ok(Reconciled::Unknown(unknown_outcome(
                        "Another transaction changed this account's balance while the claim was in flight, so this credit cannot be attributed to this request.",
                        recipient,
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
                    recipient,
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
    LostRace,
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
    recipient: &PublicAddress,
    balance_before: u128,
    tx_hash: HashType,
    submitted: &Challenge,
) -> ApiError {
    // The view has to be able to tell the user exactly which account is in
    // doubt and what its balance was beforehand, so that they can check it
    // themselves. Carrying the canonical recipient here means the view never
    // has to fall back on echoing whatever the user typed.
    ApiError::new(ErrorCode::OutcomeUnknown, message)
        .at(DropPhase::Reconciling)
        .with_details(serde_json::json!({
            "outcome": "unknown",
            "recipient": recipient,
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
    fn retry_is_authorised_only_when_the_claim_provably_lost() {
        // Absent, balance untouched, and someone else has taken the challenge.
        // Our transaction can now only fail validation, so it is discarded and
        // can never credit.
        assert_eq!(
            decide(observe(Some(false), Some(BEFORE), Some(true))),
            ClaimDecision::LostRace
        );
    }

    #[test]
    fn every_weaker_observation_refuses_to_authorise_a_second_claim() {
        for weaker in [
            // The challenge is unchanged: our claim may still win.
            observe(Some(false), Some(BEFORE), Some(false)),
            // An unreadable challenge is not proof of rotation.
            observe(Some(false), Some(BEFORE), None),
            // An unreadable inclusion is not proof of absence.
            observe(None, Some(BEFORE), Some(true)),
            // An unreadable balance cannot rule out a credit.
            observe(Some(false), None, Some(true)),
            // Already credited: retrying would double it.
            observe(Some(false), Some(EXPECTED), Some(true)),
        ] {
            assert_ne!(
                decide(weaker),
                ClaimDecision::LostRace,
                "{weaker:?} must not authorise a second claim"
            );
        }
    }

    #[test]
    fn inclusion_without_a_visible_credit_is_never_a_retry() {
        // Unreachable if the sequencer behaves as its source says: a losing
        // claim is discarded rather than included, and state is applied before
        // the transaction is indexed. Seeing this means an assumption broke,
        // and guessing here is exactly how one press becomes two credits.
        assert_eq!(
            decide(observe(Some(true), Some(BEFORE), Some(true))),
            ClaimDecision::Unattributable
        );
        assert_eq!(
            decide(observe(Some(true), Some(BEFORE), None)),
            ClaimDecision::Unattributable
        );
    }

    #[test]
    fn challenge_rotation_alone_never_decides_anything() {
        // Rotation only says "somebody won", never "we did" or "we did not".
        // It may narrow the losing case, but it can never on its own turn a
        // non-decision into a decision — least of all a success.
        for included in [Some(true), None] {
            for balance in [Some(BEFORE), Some(EXPECTED), None] {
                let rotated = decide(observe(included, balance, Some(true)));
                assert_eq!(rotated, decide(observe(included, balance, Some(false))));
                assert_eq!(rotated, decide(observe(included, balance, None)));
            }
        }
        // And it never manufactures a success on its own.
        assert_ne!(
            decide(observe(Some(false), Some(BEFORE), Some(true))),
            ClaimDecision::Credited
        );
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
    fn the_phase_channel_reports_only_the_operation_that_is_live() {
        let channel = PhaseChannel::default();
        assert_eq!(channel.current(7), None, "nothing published yet");

        channel.publish(7, DropPhase::Solving);
        assert_eq!(channel.current(7), Some(DropPhase::Solving));
        // A different token — earlier, later, or the sentinel — reads nothing.
        assert_eq!(channel.current(6), None);
        assert_eq!(channel.current(8), None);
        assert_eq!(channel.current(0), None);

        channel.publish(7, DropPhase::Reconciling);
        assert_eq!(channel.current(7), Some(DropPhase::Reconciling));

        channel.clear(7);
        assert_eq!(
            channel.current(7),
            None,
            "a finished token reads as no phase"
        );
    }

    #[test]
    fn the_sentinel_token_is_never_published_and_a_stale_clear_is_ignored() {
        let channel = PhaseChannel::default();
        // Token 0 is the Rust "no operation" sentinel; publishing it would make
        // an idle poller see a phantom drop.
        channel.publish(0, DropPhase::Solving);
        assert_eq!(channel.current(0), None);

        // A clear aimed at an earlier operation must not erase a later one.
        channel.publish(7, DropPhase::Solving);
        channel.clear(3);
        assert_eq!(channel.current(7), Some(DropPhase::Solving));
    }

    #[test]
    fn every_phase_survives_its_wire_code() {
        for phase in [
            DropPhase::ValidatingInput,
            DropPhase::VerifyingPrograms,
            DropPhase::InspectingRecipient,
            DropPhase::FetchingChallenge,
            DropPhase::Solving,
            DropPhase::RefreshingChallenge,
            DropPhase::Submitting,
            DropPhase::Reconciling,
        ] {
            assert_eq!(phase_from_code(phase_code(phase)), Some(phase));
            assert_ne!(phase_code(phase), 0, "0 is reserved for no phase");
        }
        assert_eq!(phase_from_code(0), None);
        assert_eq!(phase_from_code(9), None);
    }

    #[test]
    fn a_fresh_client_reports_no_live_phase_for_any_token() {
        let client = FaucetClient::new("https://testnet.lez.logos.co").unwrap();
        for token in [0, 1, u64::MAX] {
            assert_eq!(client.current_phase(token), None);
        }
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
        let recipient = PublicAddress::new(system_accounts::pinata_account_id());
        let error = unknown_outcome("lost", &recipient, 6_000, HashType([7; 32]), &challenge);
        assert_eq!(error.code, ErrorCode::OutcomeUnknown);
        assert!(
            !error.retryable,
            "an unknown outcome must never invite a retry"
        );
        let details = error.details.unwrap();
        assert_eq!(details["outcome"], "unknown");
        assert_eq!(details["balance_before"], "6000");
        // The view must be able to name the account in doubt without ever
        // falling back on the raw string the user typed.
        assert_eq!(details["recipient"]["account_id"], recipient.account_id);
        assert_eq!(details["recipient"]["address"], recipient.address);
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
