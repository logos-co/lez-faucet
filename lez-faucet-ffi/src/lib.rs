//! Headless LEZ public-testnet faucet client and its small C ABI.
//!
//! Every LEZ crate is pinned by the workspace manifest to the exact revision
//! deployed on the public testnet. A runtime fingerprint check still runs
//! before state-changing operations so version skew fails loudly.

use std::{
    collections::BTreeMap,
    ffi::{c_char, c_void, CStr, CString},
    future::Future,
    path::{Path, PathBuf},
    ptr,
    str::FromStr as _,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use common::HashType;
use lee::{Account, AccountId};
use sequencer_service_rpc::RpcClient as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::runtime::Runtime;
use wallet::{
    config::WalletConfigOverrides,
    program_facades::{native_token_transfer::NativeTokenTransfer, pinata::Pinata},
    AccountIdentity, WalletCore,
};
use zeroize::{Zeroize as _, Zeroizing};

/// The exact public-testnet reward encoded by the v0.2.0 Piñata guest.
pub const PINATA_PRIZE: u128 = 150;

/// This warning must remain visible anywhere a recovery phrase is shown.
pub const STORAGE_SECURITY_WARNING: &str = "Public-testnet wallet only. LEZ v0.2.0 ignores the wallet password and stores key material as plaintext JSON. Keep the wallet directory private and never reuse this mnemonic for real funds.";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_TX_DEADLINE: Duration = Duration::from_secs(300);
const DEFAULT_STALE_RETRIES: usize = 3;
const DEFAULT_SOLVE_DEADLINE: Duration = Duration::from_secs(60);
const DEFAULT_MAX_SOLVE_ATTEMPTS: u64 = 1 << 28;
const MAX_SAFE_PINATA_DIFFICULTY: u8 = 3;
static CREATE_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct FaucetPolicy {
    pub poll_interval: Duration,
    pub transaction_deadline: Duration,
    pub max_stale_challenge_retries: usize,
    pub solve_deadline: Duration,
    pub max_solve_attempts: u64,
}

impl Default for FaucetPolicy {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            transaction_deadline: DEFAULT_TX_DEADLINE,
            max_stale_challenge_retries: DEFAULT_STALE_RETRIES,
            solve_deadline: DEFAULT_SOLVE_DEADLINE,
            max_solve_attempts: DEFAULT_MAX_SOLVE_ATTEMPTS,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProgramFingerprint {
    pub authenticated_transfer: String,
    pub pinata: String,
    pub token: String,
    pub amm: String,
    pub privacy_preserving_circuit: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InitializedAccount {
    pub account_id: String,
    pub init_tx_hash: Option<String>,
    pub balance: u128,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClaimReceipt {
    pub tx_hash: Option<String>,
    pub balance_before: u128,
    pub balance_after: u128,
    pub stale_challenge_retries: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClaimProgress {
    pub completed_claims: usize,
    pub required_claims: usize,
    pub target: u128,
    pub balance: u128,
    pub receipt: ClaimReceipt,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ClaimUntilResult {
    pub target: u128,
    pub initial_balance: u128,
    pub final_balance: u128,
    pub claims: Vec<ClaimReceipt>,
}

pub struct CreatedWallet {
    pub client: FaucetClient,
    /// Display this exactly once, never log it, then discard it.
    pub mnemonic: Zeroizing<String>,
    pub security_warning: &'static str,
}

pub struct FaucetClient {
    wallet: WalletCore,
    config_path: PathBuf,
    storage_path: PathBuf,
    state_path: PathBuf,
    state: FaucetState,
    policy: FaucetPolicy,
    cancel_generation: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct FaucetState {
    active_public_account: Option<ActivePublicAccount>,
    pending_public_account: Option<PendingPublicAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ActivePublicAccount {
    account_id: String,
    init_tx_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PendingPublicAccount {
    account_id: String,
    init_tx_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateStep {
    PersistWallet,
    PersistState,
    StoreConfigChanges,
    RestrictWalletFiles,
}

impl FaucetClient {
    pub async fn create(
        config_path: impl Into<PathBuf>,
        storage_path: impl Into<PathBuf>,
        sequencer_url: &str,
        password: &str,
    ) -> Result<CreatedWallet> {
        Self::create_with_hook(
            config_path.into(),
            storage_path.into(),
            sequencer_url,
            password,
            |_| Ok(()),
        )
        .await
    }

    async fn create_with_hook(
        config_path: PathBuf,
        storage_path: PathBuf,
        sequencer_url: &str,
        password: &str,
        after_step: impl Fn(CreateStep) -> Result<()>,
    ) -> Result<CreatedWallet> {
        let state_path = state_path_for(&storage_path);
        ensure_create_paths_absent(&config_path, &storage_path, &state_path)?;
        prepare_private_path(&config_path)?;
        prepare_private_path(&storage_path)?;

        let mut creation = WalletCreation::new(&config_path, &storage_path, &state_path)?;
        let staged_config_path = creation.staged_path(CreateArtifact::Config).to_owned();
        let staged_storage_path = creation.staged_path(CreateArtifact::Storage).to_owned();
        let staged_state_path = creation.staged_path(CreateArtifact::State).to_owned();

        let overrides = wallet_overrides(sequencer_url)?;
        let (wallet, mnemonic) = WalletCore::new_init_storage(
            staged_config_path.clone(),
            staged_storage_path.clone(),
            Some(overrides),
            password,
        )
        .context("failed to create LEZ wallet")?;

        let staged_client = Self {
            wallet,
            config_path: staged_config_path,
            storage_path: staged_storage_path,
            state_path: staged_state_path,
            state: FaucetState::default(),
            policy: FaucetPolicy::default(),
            cancel_generation: Arc::new(AtomicU64::new(0)),
        };
        staged_client
            .wallet
            .store_persistent_data()
            .context("failed to persist wallet storage")?;
        after_step(CreateStep::PersistWallet)?;
        staged_client.persist_state()?;
        after_step(CreateStep::PersistState)?;
        staged_client
            .wallet
            .store_config_changes()
            .await
            .context("failed to persist wallet config")?;
        after_step(CreateStep::StoreConfigChanges)?;
        staged_client.restrict_wallet_files()?;
        after_step(CreateStep::RestrictWalletFiles)?;
        drop(staged_client);

        creation.publish()?;
        let wallet = WalletCore::new_update_chain(
            config_path.clone(),
            storage_path.clone(),
            Some(wallet_overrides(sequencer_url)?),
        )
        .context("failed to reopen newly created LEZ wallet")?;
        let client = Self {
            wallet,
            config_path,
            storage_path,
            state_path,
            state: FaucetState::default(),
            policy: FaucetPolicy::default(),
            cancel_generation: Arc::new(AtomicU64::new(0)),
        };
        client.restrict_wallet_files()?;
        creation.commit()?;

        Ok(CreatedWallet {
            client,
            mnemonic: Zeroizing::new(mnemonic.to_string()),
            security_warning: STORAGE_SECURITY_WARNING,
        })
    }

    pub fn open(
        config_path: impl Into<PathBuf>,
        storage_path: impl Into<PathBuf>,
        sequencer_url: &str,
    ) -> Result<Self> {
        let config_path = config_path.into();
        let storage_path = storage_path.into();
        let state_path = state_path_for(&storage_path);
        prepare_private_path(&config_path)?;
        prepare_private_path(&storage_path)?;
        let wallet = WalletCore::new_update_chain(
            config_path.clone(),
            storage_path.clone(),
            Some(wallet_overrides(sequencer_url)?),
        )
        .context("failed to open LEZ wallet")?;
        let client = Self {
            wallet,
            config_path,
            storage_path,
            state: load_state(&state_path)?,
            state_path,
            policy: FaucetPolicy::default(),
            cancel_generation: Arc::new(AtomicU64::new(0)),
        };
        client.restrict_wallet_files()?;
        Ok(client)
    }

    #[must_use]
    pub fn with_policy(mut self, policy: FaucetPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub async fn verify_program_fingerprint(&self) -> Result<ProgramFingerprint> {
        let remote = self
            .wallet
            .sequencer_client
            .get_program_ids()
            .await
            .context("failed to fetch sequencer program IDs")?;
        verify_program_ids(&remote)
    }

    pub async fn create_and_initialize_public_account(&mut self) -> Result<InitializedAccount> {
        self.verify_program_fingerprint().await?;

        if let Some(active) = self.state.active_public_account.clone() {
            let account_id = parse_account_id(&active.account_id)
                .context("persisted active account ID is invalid")?;
            let current = self
                .wallet
                .get_account_public(account_id)
                .await
                .with_context(|| format!("failed to inspect active account {account_id}"))?;
            return initialized_active_account(&active, &current);
        }

        let pending = if let Some(pending) = self.state.pending_public_account.clone() {
            pending
        } else {
            let (account_id, _chain_index) = self.wallet.create_new_account_public(None);
            // Persist the signing key before exposing or using its public account ID.
            self.persist_wallet()?;
            let pending = PendingPublicAccount {
                account_id: account_id.to_string(),
                init_tx_hash: None,
            };
            let mut next_state = self.state.clone();
            next_state.pending_public_account = Some(pending.clone());
            self.replace_state(next_state)?;
            pending
        };
        let account_id = parse_account_id(&pending.account_id)
            .context("persisted pending account ID is invalid")?;

        let current = self
            .wallet
            .get_account_public(account_id)
            .await
            .with_context(|| format!("failed to inspect pending account {account_id}"))?;
        if current.program_owner == programs::authenticated_transfer().id() {
            let initialized = InitializedAccount {
                account_id: account_id.to_string(),
                init_tx_hash: pending.init_tx_hash.clone(),
                balance: current.balance,
            };
            self.activate_pending_account(&pending)?;
            return Ok(initialized);
        }
        ensure!(
            current == Account::default(),
            "pending account {account_id} is not default and is owned by an unexpected program"
        );

        if let Some(previous_hash) = pending.init_tx_hash.as_deref() {
            let previous_hash = HashType::from_str(previous_hash)
                .context("persisted initialization transaction hash is invalid")?;
            if self
                .wallet
                .sequencer_client
                .get_transaction(previous_hash)
                .await
                .with_context(|| {
                    format!("failed to inspect initialization transaction {previous_hash}")
                })?
                .is_some()
            {
                let refreshed = self.wallet.get_account_public(account_id).await?;
                ensure!(
                    refreshed.program_owner == programs::authenticated_transfer().id(),
                    "initialization transaction {previous_hash} was included but pending account {account_id} has the wrong owner"
                );
                let initialized = InitializedAccount {
                    account_id: account_id.to_string(),
                    init_tx_hash: Some(previous_hash.to_string()),
                    balance: refreshed.balance,
                };
                self.activate_pending_account(&pending)?;
                return Ok(initialized);
            }
        }

        let init_hash = NativeTokenTransfer(&self.wallet)
            .register_account(AccountIdentity::Public(account_id))
            .await
            .context("failed to submit authenticated-transfer initialization")?;
        let mut next_state = self.state.clone();
        next_state
            .pending_public_account
            .as_mut()
            .context("pending account state disappeared")?
            .init_tx_hash = Some(init_hash.to_string());
        self.replace_state(next_state)?;
        self.wallet
            .poll_native_token_transfer(init_hash)
            .await
            .with_context(|| {
                format!(
                    "initialization transaction {init_hash} for pending account {account_id} was not included; retry resumes this account"
                )
            })?;

        let account = self
            .wallet
            .get_account_public(account_id)
            .await
            .context("failed to verify initialized account")?;
        ensure!(
            account.program_owner == programs::authenticated_transfer().id(),
            "initialized account has unexpected program owner"
        );
        ensure!(
            account.balance == 0,
            "newly initialized account unexpectedly has balance {}",
            account.balance
        );
        let persisted_pending = self
            .state
            .pending_public_account
            .clone()
            .context("pending account state disappeared after initialization")?;
        self.activate_pending_account(&persisted_pending)?;

        Ok(InitializedAccount {
            account_id: account_id.to_string(),
            init_tx_hash: Some(init_hash.to_string()),
            balance: account.balance,
        })
    }

    pub async fn balance(&self, account_id: AccountId) -> Result<u128> {
        self.wallet
            .get_account_balance(account_id)
            .await
            .context("failed to fetch account balance")
    }

    pub async fn claim_once(&self, winner_id: AccountId) -> Result<ClaimReceipt> {
        let operation_generation = self.cancel_generation.load(Ordering::SeqCst);
        self.claim_once_at_generation(winner_id, operation_generation)
            .await
    }

    async fn claim_once_at_generation(
        &self,
        winner_id: AccountId,
        operation_generation: u64,
    ) -> Result<ClaimReceipt> {
        ensure_not_cancelled(&self.cancel_generation, operation_generation)?;
        self.verify_program_fingerprint().await?;
        ensure_not_cancelled(&self.cancel_generation, operation_generation)?;
        let winner = self
            .wallet
            .get_account_public(winner_id)
            .await
            .context("failed to fetch claim recipient")?;
        ensure!(
            winner != Account::default(),
            "claim recipient is uninitialized"
        );
        ensure!(
            winner.program_owner == programs::authenticated_transfer().id(),
            "claim recipient is not owned by authenticated-transfer"
        );

        let balance_before = winner.balance;
        let pinata_id = system_accounts::pinata_account_id();

        for stale_retries in 0..=self.policy.max_stale_challenge_retries {
            ensure_not_cancelled(&self.cancel_generation, operation_generation)?;
            let pinata = self
                .wallet
                .get_account_public(pinata_id)
                .await
                .context("failed to fetch Piñata challenge")?;
            ensure!(
                pinata.program_owner == programs::pinata().id(),
                "Piñata system account has unexpected program owner"
            );
            ensure!(
                pinata.balance >= PINATA_PRIZE,
                "Piñata pool has insufficient balance"
            );
            let challenge = challenge_bytes(&pinata)?;
            let solution = self
                .solve_challenge(challenge, operation_generation)
                .await?;

            // Avoid submitting work that became stale while the CPU solver was running.
            ensure_not_cancelled(&self.cancel_generation, operation_generation)?;
            let latest = self
                .wallet
                .get_account_public(pinata_id)
                .await
                .context("failed to refresh Piñata challenge")?;
            if challenge_bytes(&latest)? != challenge {
                continue;
            }
            ensure_not_cancelled(&self.cancel_generation, operation_generation)?;

            // Once this call begins, a transport error may mean the sequencer
            // accepted the transaction. Cancellation must not interrupt the
            // bounded reconciliation below or permit a racing resubmission.
            let (tx_hash, submission_error) = match Pinata(&self.wallet)
                .claim(pinata_id, winner_id, solution)
                .await
            {
                Ok(tx_hash) => (Some(tx_hash), None),
                Err(error) => (
                    None,
                    Some(format!("failed to submit Piñata claim: {error:#}")),
                ),
            };

            match self
                .await_claim(
                    tx_hash,
                    winner_id,
                    balance_before,
                    challenge,
                    submission_error,
                )
                .await?
            {
                ClaimOutcome::Credited {
                    balance_after,
                    tx_hash,
                } => {
                    return Ok(ClaimReceipt {
                        tx_hash: tx_hash.map(|hash| hash.to_string()),
                        balance_before,
                        balance_after,
                        stale_challenge_retries: stale_retries,
                    });
                }
                ClaimOutcome::Stale => continue,
            }
        }

        bail!(
            "Piñata challenge changed too often; exhausted {} stale retries",
            self.policy.max_stale_challenge_retries
        )
    }

    pub async fn claim_until_target<F>(
        &self,
        winner_id: AccountId,
        target: u128,
        max_claims: usize,
        on_progress: F,
    ) -> Result<ClaimUntilResult>
    where
        F: FnMut(&ClaimProgress),
    {
        let operation_generation = self.cancel_generation.load(Ordering::SeqCst);
        let initial_balance = self.balance(winner_id).await?;
        run_claim_loop(
            initial_balance,
            target,
            max_claims,
            || self.claim_once_at_generation(winner_id, operation_generation),
            on_progress,
        )
        .await
    }

    async fn await_claim(
        &self,
        tx_hash: Option<HashType>,
        winner_id: AccountId,
        balance_before: u128,
        submitted_challenge: [u8; 33],
        submission_error: Option<String>,
    ) -> Result<ClaimOutcome> {
        let expected = balance_before
            .checked_add(PINATA_PRIZE)
            .context("winner balance overflow")?;
        let deadline = tokio::time::Instant::now() + self.policy.transaction_deadline;
        let pinata_id = system_accounts::pinata_account_id();
        let mut last_poll_error = submission_error;

        loop {
            let included = if let Some(hash) = tx_hash {
                match self.wallet.sequencer_client.get_transaction(hash).await {
                    Ok(transaction) => Some(transaction.is_some()),
                    Err(error) => {
                        last_poll_error = Some(format!(
                            "failed to poll claim transaction {hash}: {error:#}"
                        ));
                        None
                    }
                }
            } else {
                None
            };
            let balance = match self.balance(winner_id).await {
                Ok(balance) => Some(balance),
                Err(error) => {
                    last_poll_error = Some(format!("failed to poll winner balance: {error:#}"));
                    None
                }
            };
            let current_challenge = match self.wallet.get_account_public(pinata_id).await {
                Ok(account) => match challenge_bytes(&account) {
                    Ok(challenge) => Some(challenge),
                    Err(error) => return Err(error),
                },
                Err(error) => {
                    last_poll_error = Some(format!("failed to poll Piñata challenge: {error:#}"));
                    None
                }
            };
            let observation = ClaimObservation {
                included,
                balance,
                challenge: current_challenge,
            };
            match classify_claim_observation(
                tx_hash.is_some(),
                balance_before,
                expected,
                submitted_challenge,
                observation,
            )? {
                ClaimDecision::Continue => {}
                ClaimDecision::Credited(balance_after) => {
                    return Ok(ClaimOutcome::Credited {
                        balance_after,
                        tx_hash,
                    });
                }
                ClaimDecision::Stale => return Ok(ClaimOutcome::Stale),
                ClaimDecision::IncludedWithoutCredit => {
                    let hash = tx_hash.context("included claim had no transaction hash")?;
                    bail!(
                        "claim transaction {hash} was included without the expected +{PINATA_PRIZE} credit"
                    );
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(reconciliation_timeout_error(
                    tx_hash.map(|hash| hash.to_string()),
                    last_poll_error.as_deref(),
                ));
            }
            tokio::time::sleep(self.policy.poll_interval).await;
        }
    }

    async fn solve_challenge(
        &self,
        challenge: [u8; 33],
        operation_generation: u64,
    ) -> Result<u128> {
        solve_challenge_with_limits(
            challenge,
            self.policy.max_solve_attempts,
            self.policy.solve_deadline,
            Arc::clone(&self.cancel_generation),
            operation_generation,
            None,
        )
        .await
    }

    fn persist_wallet(&self) -> Result<()> {
        self.wallet
            .store_persistent_data()
            .context("failed to persist wallet storage")?;
        self.restrict_wallet_files()
    }

    fn persist_state(&self) -> Result<()> {
        persist_state_to(&self.state_path, &self.state)
    }

    fn replace_state(&mut self, next_state: FaucetState) -> Result<()> {
        replace_persisted_state(&self.state_path, &mut self.state, next_state)
    }

    fn activate_pending_account(&mut self, pending: &PendingPublicAccount) -> Result<()> {
        let mut next_state = self.state.clone();
        next_state.active_public_account = Some(ActivePublicAccount {
            account_id: pending.account_id.clone(),
            init_tx_hash: pending.init_tx_hash.clone(),
        });
        next_state.pending_public_account = None;
        self.replace_state(next_state)
    }

    #[must_use]
    fn cancellation_token(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.cancel_generation)
    }

    pub fn cancel_pending_work(&self) {
        self.cancel_generation.fetch_add(1, Ordering::SeqCst);
    }

    fn restrict_wallet_files(&self) -> Result<()> {
        restrict_file(&self.config_path)?;
        restrict_file(&self.storage_path)?;
        restrict_file(&self.state_path)
    }
}

fn initialized_active_account(
    active: &ActivePublicAccount,
    current: &Account,
) -> Result<InitializedAccount> {
    ensure!(
        current.program_owner == programs::authenticated_transfer().id(),
        "active account {} is owned by an unexpected program",
        active.account_id
    );
    Ok(InitializedAccount {
        account_id: active.account_id.clone(),
        init_tx_hash: active.init_tx_hash.clone(),
        balance: current.balance,
    })
}

fn persist_state_to(state_path: &Path, state: &FaucetState) -> Result<()> {
    let serialized = serde_json::to_vec_pretty(state)?;
    let temporary = state_path.with_extension("json.tmp");
    std::fs::write(&temporary, serialized)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    restrict_file(&temporary)?;
    std::fs::rename(&temporary, state_path)
        .with_context(|| format!("failed to replace {}", state_path.display()))?;
    restrict_file(state_path)
}

fn replace_persisted_state(
    state_path: &Path,
    current_state: &mut FaucetState,
    next_state: FaucetState,
) -> Result<()> {
    persist_state_to(state_path, &next_state)?;
    *current_state = next_state;
    Ok(())
}

enum ClaimOutcome {
    Credited {
        balance_after: u128,
        tx_hash: Option<HashType>,
    },
    Stale,
}

#[derive(Debug)]
struct OutcomeUnknown {
    tx_hash: Option<String>,
    detail: String,
}

impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "claim outcome is unknown: {}", self.detail)
    }
}

impl std::error::Error for OutcomeUnknown {}

fn reconciliation_timeout_error(
    tx_hash: Option<String>,
    last_error: Option<&str>,
) -> anyhow::Error {
    OutcomeUnknown {
        tx_hash,
        detail: format!(
            "timed out reconciling claim outcome{}",
            last_error
                .map(|error| format!("; last observation error: {error}"))
                .unwrap_or_default()
        ),
    }
    .into()
}

#[derive(Debug, Clone, Copy)]
struct ClaimObservation {
    included: Option<bool>,
    balance: Option<u128>,
    challenge: Option<[u8; 33]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimDecision {
    Continue,
    Credited(u128),
    Stale,
    IncludedWithoutCredit,
}

fn classify_claim_observation(
    has_known_tx_hash: bool,
    balance_before: u128,
    expected_balance: u128,
    submitted_challenge: [u8; 33],
    observation: ClaimObservation,
) -> Result<ClaimDecision> {
    if let Some(balance) = observation.balance {
        ensure!(
            balance <= expected_balance,
            "winner balance changed by more than one Piñata prize (before {balance_before}, now {balance})"
        );
    }

    let challenge_changed = observation
        .challenge
        .is_some_and(|challenge| challenge != submitted_challenge);
    if observation.balance == Some(expected_balance)
        && (observation.included == Some(true) || (!has_known_tx_hash && challenge_changed))
    {
        return Ok(ClaimDecision::Credited(expected_balance));
    }
    if has_known_tx_hash
        && observation.included == Some(true)
        && observation.balance == Some(balance_before)
        && challenge_changed
    {
        return Ok(ClaimDecision::IncludedWithoutCredit);
    }
    if has_known_tx_hash
        && observation.included == Some(false)
        && observation.balance == Some(balance_before)
        && challenge_changed
    {
        return Ok(ClaimDecision::Stale);
    }
    Ok(ClaimDecision::Continue)
}

fn ensure_not_cancelled(
    cancellation_generation: &AtomicU64,
    expected_generation: u64,
) -> Result<()> {
    ensure!(
        cancellation_generation.load(Ordering::SeqCst) == expected_generation,
        "claim cancelled before submission"
    );
    Ok(())
}

fn wallet_overrides(sequencer_url: &str) -> Result<WalletConfigOverrides> {
    let sequencer_addr = url::Url::parse(sequencer_url).context("invalid sequencer URL")?;
    Ok(WalletConfigOverrides {
        sequencer_addr: Some(sequencer_addr),
        seq_poll_timeout: Some(DEFAULT_POLL_INTERVAL),
        seq_tx_poll_max_blocks: Some(151),
        seq_poll_max_retries: Some(5),
        seq_block_poll_max_amount: Some(100),
        basic_auth: None,
    })
}

fn challenge_bytes(account: &Account) -> Result<[u8; 33]> {
    account
        .data
        .as_ref()
        .try_into()
        .map_err(|_| anyhow!("Piñata account data must be exactly 33 bytes"))
}

pub fn solve_challenge(challenge: [u8; 33]) -> Result<u128> {
    solve_challenge_bounded(challenge, DEFAULT_MAX_SOLVE_ATTEMPTS, || false)
}

fn solve_challenge_bounded(
    challenge: [u8; 33],
    max_attempts: u64,
    cancelled: impl Fn() -> bool,
) -> Result<u128> {
    let difficulty = usize::from(challenge[0]);
    ensure!(difficulty <= 32, "Piñata difficulty exceeds SHA-256 size");
    ensure!(
        difficulty <= usize::from(MAX_SAFE_PINATA_DIFFICULTY),
        "Piñata difficulty {difficulty} exceeds supported safe maximum {MAX_SAFE_PINATA_DIFFICULTY}"
    );
    ensure!(
        max_attempts > 0,
        "Piñata solve attempt budget must be positive"
    );
    let seed = &challenge[1..];
    for attempt in 0..max_attempts {
        if attempt % 4_096 == 0 && cancelled() {
            bail!("Piñata solve cancelled");
        }
        let solution = u128::from(attempt);
        if valid_solution(difficulty, seed, solution) {
            return Ok(solution);
        }
    }
    bail!("Piñata solve attempt budget exhausted after {max_attempts} attempts")
}

async fn solve_challenge_with_limits(
    challenge: [u8; 33],
    max_attempts: u64,
    deadline: Duration,
    cancellation_generation: Arc<AtomicU64>,
    expected_generation: u64,
    worker_active: Option<Arc<AtomicBool>>,
) -> Result<u128> {
    // Reject impossible or unsupported work before occupying the blocking pool.
    validate_challenge_difficulty(challenge[0])?;
    ensure!(
        max_attempts > 0,
        "Piñata solve attempt budget must be positive"
    );

    let timeout_cancelled = Arc::new(AtomicBool::new(false));
    let worker_timeout_cancelled = Arc::clone(&timeout_cancelled);
    let worker_cancellation_generation = Arc::clone(&cancellation_generation);
    let worker_activity = worker_active.clone();
    let mut task = tokio::task::spawn_blocking(move || {
        struct ActivityGuard(Option<Arc<AtomicBool>>);
        impl Drop for ActivityGuard {
            fn drop(&mut self) {
                if let Some(active) = &self.0 {
                    active.store(false, Ordering::SeqCst);
                }
            }
        }

        if let Some(active) = &worker_activity {
            active.store(true, Ordering::SeqCst);
        }
        let _activity_guard = ActivityGuard(worker_activity);
        solve_challenge_bounded(challenge, max_attempts, || {
            worker_timeout_cancelled.load(Ordering::Relaxed)
                || worker_cancellation_generation.load(Ordering::SeqCst) != expected_generation
        })
    });

    tokio::select! {
        result = &mut task => result.context("Piñata solver task failed")?,
        () = tokio::time::sleep(deadline) => {
            timeout_cancelled.store(true, Ordering::SeqCst);
            // A blocking task cannot be aborted after it starts. Cooperatively cancel it
            // and await its exit so a timed-out solver never continues in the background.
            let _ = task.await.context("Piñata solver task failed while stopping")?;
            bail!("Piñata solve timed out after {} ms", deadline.as_millis())
        }
    }
}

fn validate_challenge_difficulty(difficulty: u8) -> Result<()> {
    ensure!(difficulty <= 32, "Piñata difficulty exceeds SHA-256 size");
    ensure!(
        difficulty <= MAX_SAFE_PINATA_DIFFICULTY,
        "Piñata difficulty {difficulty} exceeds supported safe maximum {MAX_SAFE_PINATA_DIFFICULTY}"
    );
    Ok(())
}

fn valid_solution(difficulty: usize, seed: &[u8], solution: u128) -> bool {
    let mut input = [0_u8; 48];
    input[..32].copy_from_slice(seed);
    input[32..].copy_from_slice(&solution.to_le_bytes());
    let digest: [u8; 32] = Sha256::digest(input).into();
    digest[..difficulty].iter().all(|byte| *byte == 0)
}

fn verify_program_ids(remote: &BTreeMap<String, [u32; 8]>) -> Result<ProgramFingerprint> {
    let expected = [
        (
            "authenticated_transfer",
            programs::authenticated_transfer().id(),
        ),
        ("pinata", programs::pinata().id()),
        ("token", programs::token().id()),
        ("amm", programs::amm().id()),
        (
            "privacy_preserving_circuit",
            lee::PRIVACY_PRESERVING_CIRCUIT_ID,
        ),
    ];
    for (name, local) in expected {
        let deployed = remote
            .get(name)
            .with_context(|| format!("sequencer fingerprint is missing {name}"))?;
        ensure!(
            deployed == &local,
            "LEZ version skew for {name}: local {}, deployed {}",
            program_id_hex(local),
            program_id_hex(*deployed)
        );
    }

    Ok(ProgramFingerprint {
        authenticated_transfer: program_id_hex(programs::authenticated_transfer().id()),
        pinata: program_id_hex(programs::pinata().id()),
        token: program_id_hex(programs::token().id()),
        amm: program_id_hex(programs::amm().id()),
        privacy_preserving_circuit: program_id_hex(lee::PRIVACY_PRESERVING_CIRCUIT_ID),
    })
}

pub fn program_id_hex(program_id: [u32; 8]) -> String {
    let mut bytes = [0_u8; 32];
    for (index, word) in program_id.into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn claims_required(balance: u128, target: u128) -> Result<usize> {
    if balance >= target {
        return Ok(0);
    }
    let missing = target - balance;
    let claims = missing.div_ceil(PINATA_PRIZE);
    claims
        .try_into()
        .context("claim count does not fit on this platform")
}

async fn run_claim_loop<ClaimFn, ClaimFuture, ProgressFn>(
    initial_balance: u128,
    target: u128,
    max_claims: usize,
    mut claim: ClaimFn,
    mut on_progress: ProgressFn,
) -> Result<ClaimUntilResult>
where
    ClaimFn: FnMut() -> ClaimFuture,
    ClaimFuture: Future<Output = Result<ClaimReceipt>>,
    ProgressFn: FnMut(&ClaimProgress),
{
    let required_claims = claims_required(initial_balance, target)?;
    ensure!(
        required_claims <= max_claims,
        "target requires {required_claims} claims, exceeding explicit limit {max_claims}"
    );

    let mut balance = initial_balance;
    let mut claims = Vec::with_capacity(required_claims);
    while balance < target {
        let receipt = claim().await?;
        ensure!(
            receipt.balance_before == balance,
            "claim progress balance changed unexpectedly"
        );
        ensure!(
            receipt.balance_after == balance + PINATA_PRIZE,
            "claim did not add exactly {PINATA_PRIZE}"
        );
        balance = receipt.balance_after;
        let progress = ClaimProgress {
            completed_claims: claims.len() + 1,
            required_claims,
            target,
            balance,
            receipt: receipt.clone(),
        };
        on_progress(&progress);
        claims.push(receipt);
    }

    Ok(ClaimUntilResult {
        target,
        initial_balance,
        final_balance: balance,
        claims,
    })
}

#[derive(Debug, Clone, Copy)]
enum CreateArtifact {
    Config = 0,
    Storage = 1,
    State = 2,
}

#[derive(Debug)]
struct StagedArtifact {
    staged: PathBuf,
    final_path: PathBuf,
    published: bool,
    #[cfg(unix)]
    published_identity: Option<FileIdentity>,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

/// Owns only uniquely named staging files and final hard links published by
/// this creation attempt. A failed attempt can therefore clean up without
/// deleting a path another process created after the initial absence check.
struct WalletCreation {
    artifacts: [StagedArtifact; 3],
    committed: bool,
}

impl WalletCreation {
    fn new(config_path: &Path, storage_path: &Path, state_path: &Path) -> Result<Self> {
        let sequence = CREATE_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let process_id = std::process::id();
        let staged = |final_path: &Path, kind: &str| -> Result<PathBuf> {
            let parent = final_path
                .parent()
                .with_context(|| format!("{} has no parent directory", final_path.display()))?;
            Ok(parent.join(format!(".lez-faucet-create-{process_id}-{sequence}-{kind}")))
        };
        let artifacts = [
            StagedArtifact {
                staged: staged(config_path, "config")?,
                final_path: config_path.to_owned(),
                published: false,
                #[cfg(unix)]
                published_identity: None,
            },
            StagedArtifact {
                staged: staged(storage_path, "storage")?,
                final_path: storage_path.to_owned(),
                published: false,
                #[cfg(unix)]
                published_identity: None,
            },
            StagedArtifact {
                staged: staged(state_path, "state")?,
                final_path: state_path.to_owned(),
                published: false,
                #[cfg(unix)]
                published_identity: None,
            },
        ];
        for artifact in &artifacts {
            ensure_path_absent(&artifact.staged, "wallet creation staging file")?;
        }
        ensure_path_absent(
            &artifacts[CreateArtifact::State as usize]
                .staged
                .with_extension("json.tmp"),
            "wallet creation temporary state file",
        )?;
        Ok(Self {
            artifacts,
            committed: false,
        })
    }

    fn staged_path(&self, artifact: CreateArtifact) -> &Path {
        &self.artifacts[artifact as usize].staged
    }

    fn publish(&mut self) -> Result<()> {
        for artifact in &mut self.artifacts {
            std::fs::hard_link(&artifact.staged, &artifact.final_path).with_context(|| {
                format!(
                    "failed to publish newly created wallet artifact {}",
                    artifact.final_path.display()
                )
            })?;
            artifact.published = true;
            #[cfg(unix)]
            {
                artifact.published_identity = Some(file_identity(&artifact.staged)?);
            }
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        for artifact in &self.artifacts {
            std::fs::remove_file(&artifact.staged).with_context(|| {
                format!(
                    "failed to remove wallet creation staging file {}",
                    artifact.staged.display()
                )
            })?;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for WalletCreation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for artifact in self.artifacts.iter().rev() {
            if artifact.published && published_path_is_owned(artifact) {
                let _ = std::fs::remove_file(&artifact.final_path);
            }
            let _ = std::fs::remove_file(&artifact.staged);
        }
        let state_temporary = self.artifacts[CreateArtifact::State as usize]
            .staged
            .with_extension("json.tmp");
        let _ = std::fs::remove_file(state_temporary);
    }
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = std::fs::metadata(path)
        .with_context(|| format!("failed to inspect created artifact {}", path.display()))?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn published_path_is_owned(artifact: &StagedArtifact) -> bool {
    artifact.published_identity.is_some_and(|identity| {
        file_identity(&artifact.final_path)
            .is_ok_and(|current_identity| current_identity == identity)
    })
}

#[cfg(not(unix))]
fn published_path_is_owned(artifact: &StagedArtifact) -> bool {
    artifact.published
}

fn ensure_create_paths_absent(
    config_path: &Path,
    storage_path: &Path,
    state_path: &Path,
) -> Result<()> {
    for (kind, path) in [
        ("wallet config", config_path),
        ("wallet storage", storage_path),
        ("faucet state", state_path),
    ] {
        ensure_path_absent(path, kind)?;
    }
    Ok(())
}

fn ensure_path_absent(path: &Path, kind: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => bail!(
            "refusing to create a wallet because {kind} already exists at {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn prepare_private_path(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    restrict_directory(parent)
}

fn state_path_for(storage_path: &Path) -> PathBuf {
    storage_path.with_file_name("faucet_state.json")
}

fn load_state(path: &Path) -> Result<FaucetState> {
    if !path.exists() {
        return Ok(FaucetState::default());
    }
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read faucet state from {}", path.display()))?;
    let state: FaucetState = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse faucet state from {}", path.display()))?;
    ensure!(
        state.active_public_account.is_none() || state.pending_public_account.is_none(),
        "faucet state cannot contain both active and pending public accounts"
    );
    Ok(state)
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set private permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if !path.exists() {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set private permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn parse_account_id(raw: &str) -> Result<AccountId> {
    let raw = raw.strip_prefix("Public/").unwrap_or(raw);
    AccountId::from_str(raw).context("invalid public account ID")
}

// ---- C ABI -----------------------------------------------------------------

#[repr(C)]
pub struct FfiCreateOutput {
    pub handle: *mut FaucetHandle,
    /// JSON containing the one-time mnemonic or an error. Free with
    /// `lez_faucet_string_free`.
    pub result_json: *mut c_char,
}

pub struct FaucetHandle {
    client: Mutex<FaucetClient>,
    cancel_generation: Arc<AtomicU64>,
}

/// Called synchronously after each successful claim. The JSON pointer is valid
/// only for the duration of the callback; copy it before returning.
pub type FfiProgressCallback = Option<unsafe extern "C" fn(*const c_char, *mut c_void)>;

#[derive(Serialize)]
struct CreateSuccess<'a> {
    ok: bool,
    mnemonic: &'a str,
    security_warning: &'a str,
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to create Tokio runtime"))
}

fn c_string(value: &str) -> *mut c_char {
    let safe = value.replace('\0', "�");
    CString::new(safe).map_or(ptr::null_mut(), CString::into_raw)
}

fn json_result<T: Serialize>(result: Result<T>) -> *mut c_char {
    let value = match result {
        Ok(value) => serde_json::json!({ "ok": true, "result": value }),
        Err(error) => {
            if let Some(unknown) = error.downcast_ref::<OutcomeUnknown>() {
                serde_json::json!({
                    "ok": false,
                    "outcome": "unknown",
                    "tx_hash": unknown.tx_hash,
                    "error": format!("{error:#}"),
                })
            } else {
                serde_json::json!({ "ok": false, "error": format!("{error:#}") })
            }
        }
    };
    c_string(&value.to_string())
}

unsafe fn required_str<'a>(value: *const c_char, name: &str) -> Result<&'a str> {
    ensure!(!value.is_null(), "{name} is null");
    // SAFETY: The caller contract requires a valid NUL-terminated string.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .with_context(|| format!("{name} is not UTF-8"))
}

fn with_client<T: Serialize>(
    handle: *mut FaucetHandle,
    operation: impl FnOnce(&mut FaucetClient) -> Result<T>,
) -> *mut c_char {
    let result = (|| {
        ensure!(!handle.is_null(), "wallet handle is null");
        // SAFETY: The caller must pass a live handle returned by this library.
        let handle = unsafe { &*handle };
        let mut client = handle
            .client
            .lock()
            .map_err(|_| anyhow!("wallet lock poisoned"))?;
        operation(&mut client)
    })();
    json_result(result)
}

/// Create a new wallet and return its opaque handle plus one-time recovery JSON.
///
/// # Safety
/// All string pointers must be non-null, valid, NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_create(
    config_path: *const c_char,
    storage_path: *const c_char,
    sequencer_url: *const c_char,
    password: *const c_char,
) -> FfiCreateOutput {
    let result = (|| {
        let config_path = unsafe { required_str(config_path, "config_path") }?;
        let storage_path = unsafe { required_str(storage_path, "storage_path") }?;
        let sequencer_url = unsafe { required_str(sequencer_url, "sequencer_url") }?;
        let password = unsafe { required_str(password, "password") }?;
        runtime().block_on(FaucetClient::create(
            config_path,
            storage_path,
            sequencer_url,
            password,
        ))
    })();

    match result {
        Ok(created) => {
            let serialized = Zeroizing::new(
                serde_json::to_string(&CreateSuccess {
                    ok: true,
                    mnemonic: created.mnemonic.as_str(),
                    security_warning: created.security_warning,
                })
                .expect("serializing static wallet creation fields cannot fail"),
            );
            let result_json = c_string(&serialized);
            let cancel_generation = created.client.cancellation_token();
            let handle = Box::into_raw(Box::new(FaucetHandle {
                client: Mutex::new(created.client),
                cancel_generation,
            }));
            FfiCreateOutput {
                handle,
                result_json,
            }
        }
        Err(error) => FfiCreateOutput {
            handle: ptr::null_mut(),
            result_json: json_result::<()>(Err(error)),
        },
    }
}

/// Open an existing wallet and return its handle plus status JSON.
///
/// # Safety
/// All string pointers must be non-null, valid, NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_open(
    config_path: *const c_char,
    storage_path: *const c_char,
    sequencer_url: *const c_char,
) -> FfiCreateOutput {
    let result = (|| {
        let config_path = unsafe { required_str(config_path, "config_path") }?;
        let storage_path = unsafe { required_str(storage_path, "storage_path") }?;
        let sequencer_url = unsafe { required_str(sequencer_url, "sequencer_url") }?;
        FaucetClient::open(config_path, storage_path, sequencer_url)
    })();
    match result {
        Ok(client) => {
            let cancel_generation = client.cancellation_token();
            FfiCreateOutput {
                handle: Box::into_raw(Box::new(FaucetHandle {
                    client: Mutex::new(client),
                    cancel_generation,
                })),
                result_json: c_string(
                    &serde_json::json!({
                        "ok": true,
                        "security_warning": STORAGE_SECURITY_WARNING,
                    })
                    .to_string(),
                ),
            }
        }
        Err(error) => FfiCreateOutput {
            handle: ptr::null_mut(),
            result_json: json_result::<()>(Err(error)),
        },
    }
}

/// Destroy a wallet handle.
///
/// # Safety
/// `handle` must be null or a live handle returned by this library, and must
/// not be used or destroyed again after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_destroy(handle: *mut FaucetHandle) {
    if !handle.is_null() {
        // SAFETY: The caller must destroy a handle exactly once.
        unsafe { drop(Box::from_raw(handle)) };
    }
}

/// Free a JSON string returned by this library.
///
/// # Safety
/// `value` must be null or a pointer returned by this library, and must not be
/// used or freed again after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: The pointer must have been allocated by `c_string`.
        let mut bytes = unsafe { CString::from_raw(value) }.into_bytes_with_nul();
        bytes.zeroize();
    }
}

/// Request cooperative cancellation before the next transaction submission.
///
/// An active solver stops promptly. If submission may already have occurred,
/// the blocked call first completes bounded reconciliation and returns a
/// receipt or explicit unknown outcome. This function does not acquire the
/// wallet lock and is safe to call from a different thread.
///
/// # Safety
/// `handle` must be a live handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_cancel(handle: *mut FaucetHandle) {
    if !handle.is_null() {
        // SAFETY: The caller contract requires a live handle.
        let handle = unsafe { &*handle };
        handle.cancel_generation.fetch_add(1, Ordering::SeqCst);
    }
}

/// Verify that the sequencer's builtin program IDs match the pinned client.
///
/// # Safety
/// `handle` must be a live handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_verify_fingerprint(handle: *mut FaucetHandle) -> *mut c_char {
    with_client(handle, |client| {
        runtime().block_on(client.verify_program_fingerprint())
    })
}

/// Create or resume and initialize one public account.
///
/// # Safety
/// `handle` must be a live handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_create_and_initialize_account(
    handle: *mut FaucetHandle,
) -> *mut c_char {
    with_client(handle, |client| {
        runtime().block_on(client.create_and_initialize_public_account())
    })
}

/// Read a public account's balance.
///
/// # Safety
/// `handle` must be live and `account_id` must be a valid NUL-terminated UTF-8
/// string for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_get_balance(
    handle: *mut FaucetHandle,
    account_id: *const c_char,
) -> *mut c_char {
    with_client(handle, |client| {
        let account_id = parse_account_id(unsafe { required_str(account_id, "account_id") }?)?;
        runtime().block_on(client.balance(account_id))
    })
}

/// Solve and submit one Piñata claim.
///
/// # Safety
/// `handle` must be live and `account_id` must be a valid NUL-terminated UTF-8
/// string for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_claim_once(
    handle: *mut FaucetHandle,
    account_id: *const c_char,
) -> *mut c_char {
    with_client(handle, |client| {
        let account_id = parse_account_id(unsafe { required_str(account_id, "account_id") }?)?;
        runtime().block_on(client.claim_once(account_id))
    })
}

/// Claim repeatedly until the decimal target is reached or an error occurs.
///
/// # Safety
/// `handle` must be live; string pointers must be valid NUL-terminated UTF-8;
/// and any callback/context pair must remain valid until this call returns.
/// The callback must not re-enter this API while the wallet operation is live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_claim_until_target(
    handle: *mut FaucetHandle,
    account_id: *const c_char,
    target: *const c_char,
    max_claims: usize,
    progress_callback: FfiProgressCallback,
    progress_context: *mut c_void,
) -> *mut c_char {
    with_client(handle, |client| {
        let account_id = parse_account_id(unsafe { required_str(account_id, "account_id") }?)?;
        let target = unsafe { required_str(target, "target") }?
            .parse::<u128>()
            .context("target must be an unsigned decimal integer")?;
        runtime().block_on(
            client.claim_until_target(account_id, target, max_claims, |progress| {
                let Some(callback) = progress_callback else {
                    return;
                };
                let Ok(json) = serde_json::to_string(progress) else {
                    return;
                };
                let Ok(json) = CString::new(json) else {
                    return;
                };
                // SAFETY: The caller owns the callback and context. This call is
                // synchronous, and `json` remains alive until it returns.
                unsafe { callback(json.as_ptr(), progress_context) };
            }),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solver_matches_wire_format() {
        let mut challenge = [0_u8; 33];
        challenge[0] = 1;
        challenge[1..].copy_from_slice(&[0x42; 32]);
        let solution = solve_challenge(challenge).unwrap();
        assert!(valid_solution(1, &challenge[1..], solution));
        if solution > 0 {
            assert!(!valid_solution(1, &challenge[1..], solution - 1));
        }
    }

    #[test]
    fn solver_enforces_protocol_safety_budget_and_cancellation() {
        let zero_difficulty = [0_u8; 33];
        assert_eq!(solve_challenge(zero_difficulty).unwrap(), 0);

        let mut unsupported = [0_u8; 33];
        unsupported[0] = MAX_SAFE_PINATA_DIFFICULTY + 1;
        assert!(solve_challenge(unsupported)
            .unwrap_err()
            .to_string()
            .contains("safe maximum"));

        let mut invalid_protocol = [0_u8; 33];
        invalid_protocol[0] = 33;
        assert!(solve_challenge(invalid_protocol)
            .unwrap_err()
            .to_string()
            .contains("SHA-256 size"));

        let mut challenge = [0_u8; 33];
        challenge[0] = 1;
        challenge[1..].fill(0x42);
        assert!(!valid_solution(1, &challenge[1..], 0));
        assert!(solve_challenge_bounded(challenge, 1, || false)
            .unwrap_err()
            .to_string()
            .contains("attempt budget exhausted"));
        assert!(solve_challenge_bounded(challenge, 10_000, || true)
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
    }

    #[tokio::test]
    async fn cancelled_solver_worker_has_stopped_before_return() {
        let mut challenge = [0_u8; 33];
        challenge[0] = MAX_SAFE_PINATA_DIFFICULTY;
        challenge[1..].fill(0x42);
        let generation = Arc::new(AtomicU64::new(1));
        let worker_active = Arc::new(AtomicBool::new(false));
        let error = solve_challenge_with_limits(
            challenge,
            u64::MAX,
            Duration::from_secs(1),
            generation,
            0,
            Some(Arc::clone(&worker_active)),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("cancelled"));
        assert!(!worker_active.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn timed_out_solver_worker_has_stopped_before_return() {
        let mut challenge = [0_u8; 33];
        challenge[0] = MAX_SAFE_PINATA_DIFFICULTY;
        challenge[1..].fill(0x42);
        let generation = Arc::new(AtomicU64::new(0));
        let worker_active = Arc::new(AtomicBool::new(false));
        let error = solve_challenge_with_limits(
            challenge,
            u64::MAX,
            Duration::ZERO,
            generation,
            0,
            Some(Arc::clone(&worker_active)),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("timed out"));
        assert!(!worker_active.load(Ordering::SeqCst));
    }

    #[test]
    fn claim_reconciliation_distinguishes_definite_and_ambiguous_outcomes() {
        let submitted = [7_u8; 33];
        let changed = [8_u8; 33];
        let observation = |included, balance, challenge| ClaimObservation {
            included,
            balance,
            challenge,
        };

        assert_eq!(
            classify_claim_observation(
                true,
                0,
                PINATA_PRIZE,
                submitted,
                observation(Some(false), Some(0), Some(changed)),
            )
            .unwrap(),
            ClaimDecision::Stale
        );
        assert_eq!(
            classify_claim_observation(
                true,
                0,
                PINATA_PRIZE,
                submitted,
                observation(Some(true), Some(PINATA_PRIZE), Some(changed)),
            )
            .unwrap(),
            ClaimDecision::Credited(PINATA_PRIZE)
        );
        assert_eq!(
            classify_claim_observation(
                true,
                0,
                PINATA_PRIZE,
                submitted,
                observation(Some(true), Some(0), Some(changed)),
            )
            .unwrap(),
            ClaimDecision::IncludedWithoutCredit
        );
        assert_eq!(
            classify_claim_observation(
                true,
                0,
                PINATA_PRIZE,
                submitted,
                observation(None, None, None),
            )
            .unwrap(),
            ClaimDecision::Continue
        );
        assert_eq!(
            classify_claim_observation(
                true,
                0,
                PINATA_PRIZE,
                submitted,
                observation(Some(true), Some(PINATA_PRIZE), Some(changed)),
            )
            .unwrap(),
            ClaimDecision::Credited(PINATA_PRIZE)
        );

        // A transport error after submission has no hash and is never treated as
        // stale/resubmittable. Challenge movement plus exact credit is sufficient
        // evidence of success; no credit remains ambiguous until timeout.
        assert_eq!(
            classify_claim_observation(
                false,
                0,
                PINATA_PRIZE,
                submitted,
                observation(None, Some(PINATA_PRIZE), Some(changed)),
            )
            .unwrap(),
            ClaimDecision::Credited(PINATA_PRIZE)
        );
        assert_eq!(
            classify_claim_observation(
                false,
                0,
                PINATA_PRIZE,
                submitted,
                observation(None, Some(0), Some(changed)),
            )
            .unwrap(),
            ClaimDecision::Continue
        );

        let timeout = reconciliation_timeout_error(Some("known-hash".to_owned()), Some("rpc"));
        let unknown = timeout.downcast_ref::<OutcomeUnknown>().unwrap();
        assert_eq!(unknown.tx_hash.as_deref(), Some("known-hash"));
        assert!(unknown.detail.contains("last observation error: rpc"));
    }

    #[test]
    fn active_account_reuse_preserves_identity_and_rejects_wrong_owner() {
        let active = ActivePublicAccount {
            account_id: "original-account".to_owned(),
            init_tx_hash: Some("original-init".to_owned()),
        };
        let account = Account {
            program_owner: programs::authenticated_transfer().id(),
            balance: 300,
            ..Account::default()
        };
        let first = initialized_active_account(&active, &account).unwrap();
        let repeated = initialized_active_account(&active, &account).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(repeated.account_id, "original-account");
        assert_eq!(repeated.init_tx_hash.as_deref(), Some("original-init"));

        let original_state = FaucetState {
            active_public_account: Some(active.clone()),
            pending_public_account: None,
        };
        let wrong_owner = Account::default();
        assert!(initialized_active_account(&active, &wrong_owner)
            .unwrap_err()
            .to_string()
            .contains("unexpected program"));
        assert_eq!(
            original_state.active_public_account.as_ref(),
            Some(&active),
            "validating a wrong owner must not clear or replace durable state"
        );
    }

    #[test]
    fn failed_state_write_does_not_mutate_live_state() {
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("missing-parent/faucet_state.json");
        let active = ActivePublicAccount {
            account_id: "original-account".to_owned(),
            init_tx_hash: Some("original-init".to_owned()),
        };
        let mut current = FaucetState {
            active_public_account: Some(active),
            pending_public_account: None,
        };
        let original = current.clone();
        let next = FaucetState::default();
        assert!(replace_persisted_state(&state_path, &mut current, next).is_err());
        assert_eq!(current, original);
    }

    #[test]
    fn fingerprint_includes_pinata_and_rejects_skew() {
        let mut ids = BTreeMap::from([
            (
                "authenticated_transfer".to_owned(),
                programs::authenticated_transfer().id(),
            ),
            ("pinata".to_owned(), programs::pinata().id()),
            ("token".to_owned(), programs::token().id()),
            ("amm".to_owned(), programs::amm().id()),
            (
                "privacy_preserving_circuit".to_owned(),
                lee::PRIVACY_PRESERVING_CIRCUIT_ID,
            ),
        ]);
        let fingerprint = verify_program_ids(&ids).unwrap();
        assert_eq!(
            fingerprint.pinata,
            "66f6a58d92c159c3c13ea54d1e37a68a814f0fd3b8fd44b7d35c0617ac4456f8"
        );

        ids.get_mut("pinata").unwrap()[0] ^= 1;
        let error = verify_program_ids(&ids).unwrap_err().to_string();
        assert!(error.contains("version skew for pinata"));
    }

    #[test]
    fn claim_count_rounds_up_to_prize_boundary() {
        assert_eq!(claims_required(0, 0).unwrap(), 0);
        assert_eq!(claims_required(0, 1).unwrap(), 1);
        assert_eq!(claims_required(0, 150).unwrap(), 1);
        assert_eq!(claims_required(0, 1_000).unwrap(), 7);
        assert_eq!(claims_required(900, 1_000).unwrap(), 1);
    }

    #[tokio::test]
    async fn claim_until_reports_each_exact_credit() {
        let next_balance = std::cell::Cell::new(0_u128);
        let progress = std::cell::RefCell::new(Vec::new());
        let result = run_claim_loop(
            0,
            1_000,
            7,
            || {
                let before = next_balance.get();
                let after = before + PINATA_PRIZE;
                next_balance.set(after);
                std::future::ready(Ok(ClaimReceipt {
                    tx_hash: Some(format!("tx-{after}")),
                    balance_before: before,
                    balance_after: after,
                    stale_challenge_retries: 0,
                }))
            },
            |event| progress.borrow_mut().push(event.clone()),
        )
        .await
        .unwrap();

        assert_eq!(result.final_balance, 1_050);
        assert_eq!(result.claims.len(), 7);
        assert_eq!(progress.borrow().len(), 7);
        assert_eq!(progress.borrow().last().unwrap().balance, 1_050);
    }

    #[tokio::test]
    async fn claim_until_requires_explicit_sufficient_limit() {
        let error = run_claim_loop(
            0,
            1_000,
            6,
            || std::future::ready(Err(anyhow!("must not run"))),
            |_| {},
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("requires 7 claims"));
    }

    #[tokio::test]
    async fn cancel_after_submit_reconciles_current_claim_then_blocks_the_next() {
        let generation = Arc::new(AtomicU64::new(0));
        let expected_generation = 0;
        let attempted_submissions = std::cell::Cell::new(0_usize);
        let settled_progress = std::cell::RefCell::new(Vec::new());

        let error = run_claim_loop(
            0,
            PINATA_PRIZE * 2,
            2,
            || {
                let result = ensure_not_cancelled(&generation, expected_generation).map(|()| {
                    let before = (attempted_submissions.get() as u128) * PINATA_PRIZE;
                    attempted_submissions.set(attempted_submissions.get() + 1);
                    // Model cancellation racing the first submit. Post-submit
                    // reconciliation still returns its settled receipt.
                    generation.fetch_add(1, Ordering::SeqCst);
                    ClaimReceipt {
                        tx_hash: Some("settled-first-claim".to_owned()),
                        balance_before: before,
                        balance_after: before + PINATA_PRIZE,
                        stale_challenge_retries: 0,
                    }
                });
                std::future::ready(result)
            },
            |progress| settled_progress.borrow_mut().push(progress.clone()),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("cancelled before submission"));
        assert_eq!(attempted_submissions.get(), 1);
        assert_eq!(settled_progress.borrow().len(), 1);
        assert_eq!(
            settled_progress.borrow()[0].receipt.tx_hash.as_deref(),
            Some("settled-first-claim")
        );
    }

    #[cfg(unix)]
    #[test]
    fn wallet_paths_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let wallet_dir = temp.path().join("wallet");
        let storage = wallet_dir.join("storage.json");
        prepare_private_path(&storage).unwrap();
        std::fs::write(&storage, b"secret").unwrap();
        restrict_file(&storage).unwrap();
        assert_eq!(
            std::fs::metadata(&wallet_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&storage).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn pending_account_state_roundtrips_privately() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("wallet/faucet_state.json");
        prepare_private_path(&state_path).unwrap();
        let state = FaucetState {
            active_public_account: None,
            pending_public_account: Some(PendingPublicAccount {
                account_id: "public-account".to_owned(),
                init_tx_hash: Some("init-hash".to_owned()),
            }),
        };
        let serialized = serde_json::to_vec_pretty(&state).unwrap();
        std::fs::write(&state_path, serialized).unwrap();
        restrict_file(&state_path).unwrap();
        assert_eq!(load_state(&state_path).unwrap(), state);
        assert_eq!(
            std::fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_wallet_persists_all_files_privately() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let wallet_dir = temp.path().join("wallet");
        let config = wallet_dir.join("config.json");
        let storage = wallet_dir.join("storage.json");
        let state = wallet_dir.join("faucet_state.json");
        let created = FaucetClient::create(
            &config,
            &storage,
            "http://127.0.0.1:3040",
            "ignored-by-upstream-v0.2.0",
        )
        .await
        .unwrap();
        assert_eq!(created.security_warning, STORAGE_SECURITY_WARNING);
        drop(created.mnemonic);

        for path in [&config, &storage, &state] {
            assert!(path.exists());
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(&wallet_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(load_state(&state).unwrap(), FaucetState::default());
    }

    #[tokio::test]
    async fn create_rolls_back_every_fallible_persistence_step() {
        for failure_point in [
            CreateStep::PersistWallet,
            CreateStep::PersistState,
            CreateStep::StoreConfigChanges,
            CreateStep::RestrictWalletFiles,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let wallet_dir = temp.path().join("wallet");
            let config = wallet_dir.join("config.json");
            let storage = wallet_dir.join("storage.json");
            let state = wallet_dir.join("faucet_state.json");
            let error = match FaucetClient::create_with_hook(
                config.clone(),
                storage.clone(),
                "http://127.0.0.1:3040",
                "ignored-by-upstream-v0.2.0",
                |completed_step| {
                    ensure!(
                        completed_step != failure_point,
                        "injected failure after {failure_point:?}"
                    );
                    Ok(())
                },
            )
            .await
            {
                Ok(_) => panic!("injected wallet creation failure should propagate"),
                Err(error) => error,
            };
            assert!(error
                .to_string()
                .contains(&format!("injected failure after {failure_point:?}")));
            for path in [&config, &storage, &state] {
                assert!(
                    !path.exists(),
                    "failed creation left artifact {}",
                    path.display()
                );
            }
            assert_eq!(
                std::fs::read_dir(&wallet_dir).unwrap().count(),
                0,
                "failed creation left a staging artifact after {failure_point:?}"
            );

            let retry = FaucetClient::create(
                &config,
                &storage,
                "http://127.0.0.1:3040",
                "ignored-by-upstream-v0.2.0",
            )
            .await
            .unwrap_or_else(|error| {
                panic!("retry after {failure_point:?} rollback failed: {error:#}")
            });
            drop(retry);
        }
    }

    #[test]
    fn creation_publish_never_removes_a_competing_final_path() {
        let temp = tempfile::tempdir().unwrap();
        let wallet_dir = temp.path().join("wallet");
        std::fs::create_dir(&wallet_dir).unwrap();
        let config = wallet_dir.join("config.json");
        let storage = wallet_dir.join("storage.json");
        let state = wallet_dir.join("faucet_state.json");
        let mut creation = WalletCreation::new(&config, &storage, &state).unwrap();
        for artifact in [
            CreateArtifact::Config,
            CreateArtifact::Storage,
            CreateArtifact::State,
        ] {
            std::fs::write(creation.staged_path(artifact), b"created-by-wallet").unwrap();
        }

        // Simulate another creator winning the race after the absence check,
        // and after this transaction has published its first two hard links.
        std::fs::write(&state, b"created-by-someone-else").unwrap();
        let error = creation.publish().unwrap_err();
        assert!(error.to_string().contains("failed to publish"));
        drop(creation);

        assert!(!config.exists());
        assert!(!storage.exists());
        assert_eq!(std::fs::read(&state).unwrap(), b"created-by-someone-else");
        assert_eq!(std::fs::read_dir(&wallet_dir).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn create_refuses_each_existing_artifact_without_modifying_any_path() {
        for existing_index in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let wallet_dir = temp.path().join("wallet");
            std::fs::create_dir(&wallet_dir).unwrap();
            let config = wallet_dir.join("config.json");
            let storage = wallet_dir.join("storage.json");
            let state = wallet_dir.join("faucet_state.json");
            let paths = [&config, &storage, &state];
            std::fs::write(paths[existing_index], b"do-not-touch").unwrap();

            let error = match FaucetClient::create(
                &config,
                &storage,
                "http://127.0.0.1:3040",
                "ignored-by-upstream-v0.2.0",
            )
            .await
            {
                Ok(_) => panic!("creation unexpectedly replaced an existing artifact"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains("already exists"));
            assert_eq!(
                std::fs::read(paths[existing_index]).unwrap(),
                b"do-not-touch"
            );
            for (index, path) in paths.iter().enumerate() {
                assert_eq!(
                    path.exists(),
                    index == existing_index,
                    "creation changed {} after refusing existing artifact {}",
                    path.display(),
                    paths[existing_index].display()
                );
            }
        }
    }
}
