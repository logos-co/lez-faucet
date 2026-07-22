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
    sync::{Mutex, OnceLock},
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

/// The exact public-testnet reward encoded by the v0.2.0 Piñata guest.
pub const PINATA_PRIZE: u128 = 150;

/// This warning must remain visible anywhere a recovery phrase is shown.
pub const STORAGE_SECURITY_WARNING: &str = "Public-testnet wallet only. LEZ v0.2.0 ignores the wallet password and stores key material as plaintext JSON. Keep the wallet directory private and never reuse this mnemonic for real funds.";

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_TX_DEADLINE: Duration = Duration::from_secs(300);
const DEFAULT_STALE_RETRIES: usize = 3;

#[derive(Debug, Clone)]
pub struct FaucetPolicy {
    pub poll_interval: Duration,
    pub transaction_deadline: Duration,
    pub max_stale_challenge_retries: usize,
}

impl Default for FaucetPolicy {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_POLL_INTERVAL,
            transaction_deadline: DEFAULT_TX_DEADLINE,
            max_stale_challenge_retries: DEFAULT_STALE_RETRIES,
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
    pub tx_hash: String,
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
    pub mnemonic: String,
    pub security_warning: &'static str,
}

pub struct FaucetClient {
    wallet: WalletCore,
    config_path: PathBuf,
    storage_path: PathBuf,
    state_path: PathBuf,
    state: FaucetState,
    policy: FaucetPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct FaucetState {
    pending_public_account: Option<PendingPublicAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PendingPublicAccount {
    account_id: String,
    init_tx_hash: Option<String>,
}

impl FaucetClient {
    pub async fn create(
        config_path: impl Into<PathBuf>,
        storage_path: impl Into<PathBuf>,
        sequencer_url: &str,
        password: &str,
    ) -> Result<CreatedWallet> {
        let config_path = config_path.into();
        let storage_path = storage_path.into();
        let state_path = state_path_for(&storage_path);
        prepare_private_path(&config_path)?;
        prepare_private_path(&storage_path)?;

        let overrides = wallet_overrides(sequencer_url)?;
        let (wallet, mnemonic) = WalletCore::new_init_storage(
            config_path.clone(),
            storage_path.clone(),
            Some(overrides),
            password,
        )
        .context("failed to create LEZ wallet")?;

        let client = Self {
            wallet,
            config_path,
            storage_path,
            state_path,
            state: FaucetState::default(),
            policy: FaucetPolicy::default(),
        };
        client.persist_wallet()?;
        client.persist_state()?;
        client
            .wallet
            .store_config_changes()
            .await
            .context("failed to persist wallet config")?;
        client.restrict_wallet_files()?;

        Ok(CreatedWallet {
            client,
            mnemonic: mnemonic.to_string(),
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
            self.state.pending_public_account = Some(pending.clone());
            self.persist_state()?;
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
            self.finish_pending_account()?;
            return Ok(InitializedAccount {
                account_id: account_id.to_string(),
                init_tx_hash: pending.init_tx_hash,
                balance: current.balance,
            });
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
                self.finish_pending_account()?;
                return Ok(InitializedAccount {
                    account_id: account_id.to_string(),
                    init_tx_hash: Some(previous_hash.to_string()),
                    balance: refreshed.balance,
                });
            }
        }

        let init_hash = NativeTokenTransfer(&self.wallet)
            .register_account(AccountIdentity::Public(account_id))
            .await
            .context("failed to submit authenticated-transfer initialization")?;
        self.state
            .pending_public_account
            .as_mut()
            .context("pending account state disappeared")?
            .init_tx_hash = Some(init_hash.to_string());
        self.persist_state()?;
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
        self.finish_pending_account()?;

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
        self.verify_program_fingerprint().await?;
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
            let solution = tokio::task::spawn_blocking(move || solve_challenge(challenge))
                .await
                .context("Piñata solver task failed")??;

            // Avoid submitting work that became stale while the CPU solver was running.
            let latest = self
                .wallet
                .get_account_public(pinata_id)
                .await
                .context("failed to refresh Piñata challenge")?;
            if challenge_bytes(&latest)? != challenge {
                continue;
            }

            let tx_hash = Pinata(&self.wallet)
                .claim(pinata_id, winner_id, solution)
                .await
                .context("failed to submit Piñata claim")?;

            match self
                .await_claim(tx_hash, winner_id, balance_before, challenge)
                .await?
            {
                ClaimOutcome::Credited(balance_after) => {
                    return Ok(ClaimReceipt {
                        tx_hash: tx_hash.to_string(),
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
        let initial_balance = self.balance(winner_id).await?;
        run_claim_loop(
            initial_balance,
            target,
            max_claims,
            || self.claim_once(winner_id),
            on_progress,
        )
        .await
    }

    async fn await_claim(
        &self,
        tx_hash: HashType,
        winner_id: AccountId,
        balance_before: u128,
        submitted_challenge: [u8; 33],
    ) -> Result<ClaimOutcome> {
        let expected = balance_before
            .checked_add(PINATA_PRIZE)
            .context("winner balance overflow")?;
        let deadline = tokio::time::Instant::now() + self.policy.transaction_deadline;
        let pinata_id = system_accounts::pinata_account_id();

        loop {
            let included = self
                .wallet
                .sequencer_client
                .get_transaction(tx_hash)
                .await
                .with_context(|| format!("failed to poll claim transaction {tx_hash}"))?
                .is_some();
            let balance = self.balance(winner_id).await?;

            ensure!(
                balance <= expected,
                "winner balance changed by more than one Piñata prize (before {balance_before}, now {balance})"
            );
            if included && balance == expected {
                return Ok(ClaimOutcome::Credited(balance));
            }
            if included && balance == balance_before {
                bail!("claim transaction {tx_hash} was included without the expected +150 credit");
            }

            let current_pinata = self
                .wallet
                .get_account_public(pinata_id)
                .await
                .context("failed to monitor Piñata challenge")?;
            if challenge_bytes(&current_pinata)? != submitted_challenge {
                // Re-read both values after observing the challenge change to avoid a
                // false stale result from two RPC calls straddling the same block.
                let confirmed_balance = self.balance(winner_id).await?;
                let confirmed_included = self
                    .wallet
                    .sequencer_client
                    .get_transaction(tx_hash)
                    .await
                    .with_context(|| format!("failed to confirm claim transaction {tx_hash}"))?
                    .is_some();
                if confirmed_included && confirmed_balance == expected {
                    return Ok(ClaimOutcome::Credited(confirmed_balance));
                }
                if !confirmed_included && confirmed_balance == balance_before {
                    return Ok(ClaimOutcome::Stale);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out waiting for claim transaction {tx_hash} and +150 balance evidence"
                );
            }
            tokio::time::sleep(self.policy.poll_interval).await;
        }
    }

    fn persist_wallet(&self) -> Result<()> {
        self.wallet
            .store_persistent_data()
            .context("failed to persist wallet storage")?;
        self.restrict_wallet_files()
    }

    fn persist_state(&self) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(&self.state)?;
        let temporary = self.state_path.with_extension("json.tmp");
        std::fs::write(&temporary, serialized)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        restrict_file(&temporary)?;
        std::fs::rename(&temporary, &self.state_path)
            .with_context(|| format!("failed to replace {}", self.state_path.display()))?;
        restrict_file(&self.state_path)
    }

    fn finish_pending_account(&mut self) -> Result<()> {
        self.state.pending_public_account = None;
        self.persist_state()
    }

    fn restrict_wallet_files(&self) -> Result<()> {
        restrict_file(&self.config_path)?;
        restrict_file(&self.storage_path)?;
        restrict_file(&self.state_path)
    }
}

enum ClaimOutcome {
    Credited(u128),
    Stale,
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
    let difficulty = usize::from(challenge[0]);
    ensure!(difficulty <= 32, "Piñata difficulty exceeds SHA-256 size");
    let seed = &challenge[1..];
    for solution in 0..=u128::MAX {
        if valid_solution(difficulty, seed, solution) {
            return Ok(solution);
        }
    }
    bail!("Piñata solution space exhausted")
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
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse faucet state from {}", path.display()))
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
}

/// Called synchronously after each successful claim. The JSON pointer is valid
/// only for the duration of the callback; copy it before returning.
pub type FfiProgressCallback = Option<unsafe extern "C" fn(*const c_char, *mut c_void)>;

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
        Err(error) => serde_json::json!({ "ok": false, "error": format!("{error:#}") }),
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
            let result_json = c_string(
                &serde_json::json!({
                    "ok": true,
                    "mnemonic": created.mnemonic,
                    "security_warning": created.security_warning,
                })
                .to_string(),
            );
            let handle = Box::into_raw(Box::new(FaucetHandle {
                client: Mutex::new(created.client),
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
        Ok(client) => FfiCreateOutput {
            handle: Box::into_raw(Box::new(FaucetHandle {
                client: Mutex::new(client),
            })),
            result_json: c_string(
                &serde_json::json!({
                    "ok": true,
                    "security_warning": STORAGE_SECURITY_WARNING,
                })
                .to_string(),
            ),
        },
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
        unsafe { drop(CString::from_raw(value)) };
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
                    tx_hash: format!("tx-{after}"),
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
}
