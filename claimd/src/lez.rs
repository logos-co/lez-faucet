//! The real backend: LEZ v0.3.1 `wallet` crate. Signing happens on a dedicated OS thread
//! that owns the `WalletCore` (so nothing here depends on it being `Send`); reads go through
//! a clone of its JSON-RPC client. Mirrors what the CLI does for `auth-transfer send` between
//! two public accounts (`lez/wallet/src/cli/programs/native_token_transfer.rs::handle_public`):
//! `NativeTokenTransfer::send_public_transfer` with the hot account as signer and fee payer.

use std::{path::PathBuf, str::FromStr as _};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use common::HashType;
use lee::AccountId;
use sequencer_service_rpc::{ClientError, RpcClient as _, SequencerClient};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};
use wallet::{
    account::{AccountIdWithPrivacy, Label},
    program_facades::native_token_transfer::NativeTokenTransfer,
    AccountIdentity, ExecutionFailureKind, WalletCore,
};

use crate::chain::{Chain, ChainError, FeeQuote, TxStatus};

struct SendCmd {
    to: AccountId,
    amount: u128,
    reply: oneshot::Sender<Result<String, ChainError>>,
}

pub struct LezChain {
    client: SequencerClient,
    hot: AccountId,
    hot_b58: String,
    gas_limit: u64,
    sender: mpsc::Sender<SendCmd>,
}

impl LezChain {
    /// Loads `<home>/wallet_config.json` + `<home>/storage.json`, resolves the hot account
    /// (`Public/<id>` or a label) and checks the wallet holds its signing key.
    pub fn start(home: PathBuf, hot_selector: &str, expected_sequencer: &str) -> Result<Self> {
        let config_path = home.join("wallet_config.json");
        let storage_path = home.join("storage.json");
        anyhow::ensure!(
            storage_path.exists(),
            "{} not found: initialise the hot wallet first (see README Operations)",
            storage_path.display()
        );
        let hot_selector = hot_selector.to_owned();
        let expected_sequencer = expected_sequencer.to_owned();

        type Ready = Result<(SequencerClient, AccountId, u64)>;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Ready>();
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<SendCmd>(16);

        std::thread::Builder::new()
            .name("lez-wallet".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.into()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let init = (|| -> Result<(WalletCore, AccountId)> {
                        let wallet = WalletCore::new_update_chain(config_path, storage_path, None)
                            .context("loading hot wallet")?;
                        let configured = wallet.client_url().to_string();
                        let want = expected_sequencer.trim_end_matches('/');
                        anyhow::ensure!(
                            configured.trim_end_matches('/') == want,
                            "wallet_config.json points at {configured} but CLAIMD_SEQUENCER is {expected_sequencer}"
                        );
                        let hot = resolve_hot(&wallet, &hot_selector)?;
                        anyhow::ensure!(
                            wallet.get_account_public_signing_key(hot).is_some(),
                            "the hot wallet does not hold the signing key for Public/{hot}"
                        );
                        Ok((wallet, hot))
                    })();
                    let (wallet, hot) = match init {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let gas_limit = wallet.config().gas_limit;
                    if ready_tx
                        .send(Ok((wallet.client_owned(), hot, gas_limit)))
                        .is_err()
                    {
                        return;
                    }
                    while let Some(cmd) = cmd_rx.recv().await {
                        let res = NativeTokenTransfer(&wallet)
                            .send_public_transfer(
                                AccountIdentity::Public(hot),
                                AccountIdentity::PublicNoSign(cmd.to),
                                cmd.amount,
                            )
                            .await
                            .map(|h| h.to_string())
                            .map_err(map_exec_err);
                        let _ = cmd.reply.send(res);
                    }
                    info!("wallet thread: command channel closed");
                });
            })
            .context("spawning wallet thread")?;

        let (client, hot, gas_limit) = ready_rx
            .recv()
            .context("wallet thread exited before reporting readiness")??;
        Ok(Self {
            client,
            hot,
            hot_b58: hot.to_string(),
            gas_limit,
            sender: cmd_tx,
        })
    }
}

fn resolve_hot(wallet: &WalletCore, selector: &str) -> Result<AccountId> {
    if let Some(b58) = selector.strip_prefix("Public/") {
        return AccountId::from_str(b58).map_err(|e| anyhow::anyhow!("CLAIMD_HOT_ACCOUNT: {e}"));
    }
    anyhow::ensure!(
        !selector.starts_with("Private/"),
        "CLAIMD_HOT_ACCOUNT must be a public account"
    );
    match wallet.storage().resolve_label(&Label::new(selector)) {
        Some(AccountIdWithPrivacy::Public(id)) => Ok(id),
        Some(AccountIdWithPrivacy::Private(_)) => {
            anyhow::bail!("wallet label {selector:?} is a private account")
        }
        None => anyhow::bail!("wallet has no account labelled {selector:?}"),
    }
}

fn parse_account(b58: &str) -> Result<AccountId, ChainError> {
    AccountId::from_str(b58).map_err(|e| ChainError::Rejected(format!("bad account id: {e}")))
}

fn map_client_err(e: ClientError) -> ChainError {
    match e {
        ClientError::Call(obj) => {
            ChainError::Rejected(format!("sequencer error {}: {}", obj.code(), obj.message()))
        }
        ClientError::Transport(err) => ChainError::Unknown(format!("transport: {err}")),
        ClientError::RequestTimeout => ChainError::Unknown("request timed out".into()),
        other => ChainError::Rpc(other.to_string()),
    }
}

fn map_exec_err(e: ExecutionFailureKind) -> ChainError {
    match e {
        ExecutionFailureKind::InsufficientFundsError => {
            ChainError::InsufficientFunds(e.to_string())
        }
        ExecutionFailureKind::SequencerClientError(ce) => map_client_err(ce),
        ExecutionFailureKind::SequencerError(inner) => ChainError::Rpc(format!("{inner:#}")),
        other => ChainError::Rejected(other.to_string()),
    }
}

fn read_err(e: ClientError) -> ChainError {
    match e {
        ClientError::Call(obj) => {
            ChainError::Rpc(format!("sequencer error {}: {}", obj.code(), obj.message()))
        }
        other => ChainError::Rpc(other.to_string()),
    }
}

#[async_trait]
impl Chain for LezChain {
    fn hot_account(&self) -> &str {
        &self.hot_b58
    }

    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    async fn balance(&self, account: &str) -> Result<u128, ChainError> {
        let id = parse_account(account)?;
        self.client.get_account_balance(id).await.map_err(read_err)
    }

    async fn send(&self, to: &str, amount: u128) -> Result<String, ChainError> {
        let to = parse_account(to)?;
        if to == self.hot {
            return Err(ChainError::Rejected("recipient is the hot account".into()));
        }
        // Pre-flight read: if the sequencer is unreachable we learn it here as a retryable
        // `Rpc` error instead of an ambiguous transport failure around the send itself.
        self.client
            .get_accounts_nonces(vec![self.hot])
            .await
            .map_err(read_err)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.sender
            .send(SendCmd {
                to,
                amount,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ChainError::Rpc("wallet thread is gone".into()))?;
        match reply_rx.await {
            Ok(r) => r,
            Err(_) => {
                error!("wallet thread dropped a send reply");
                Err(ChainError::Unknown(
                    "wallet thread dropped the reply".into(),
                ))
            }
        }
    }

    async fn poll(&self, tx_hash: &str) -> Result<TxStatus, ChainError> {
        let hash = HashType::from_str(tx_hash)
            .map_err(|e| ChainError::Rpc(format!("bad tx hash in ledger: {e}")))?;
        match self.client.get_transaction(hash).await.map_err(read_err)? {
            Some((_tx, block)) => Ok(TxStatus::Included { block }),
            None => Ok(TxStatus::Pending),
        }
    }

    async fn fee_state(&self) -> Result<FeeQuote, ChainError> {
        let q = self.client.get_fee_state().await.map_err(read_err)?;
        Ok(FeeQuote {
            height: q.height,
            base_fee_exec: q.base_fee_exec,
            base_fee_stor: q.base_fee_stor,
            next_base_fee_exec_ceiling: q.next_base_fee_exec_ceiling,
            next_base_fee_stor_ceiling: q.next_base_fee_stor_ceiling,
        })
    }

    async fn health(&self) -> Result<(), ChainError> {
        match self.client.check_health().await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!("checkHealth failed: {e}");
                Err(read_err(e))
            }
        }
    }
}
