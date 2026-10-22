//! The single payout worker. The HTTP layer only enqueues rows; this task drains them in
//! order, one transfer at a time (one signer, one nonce), and never re-signs a row that
//! already carries a `tx_hash`.

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::{
    chain::{Chain, ChainError, TxStatus},
    db::{Claim, Db, Status},
    policy,
};

#[derive(Clone)]
pub struct WorkerConfig {
    pub prize: u128,
    pub poll_secs: u64,
    pub poll_interval_secs: u64,
}

pub struct Worker {
    pub db: Arc<Db>,
    pub chain: Arc<dyn Chain>,
    pub cfg: WorkerConfig,
    pub notify: Arc<Notify>,
}

pub const E_SUBMISSION_REJECTED: &str = "submission_rejected";
pub const E_OUTCOME_UNKNOWN: &str = "outcome_unknown";
pub const E_HOT_BALANCE_LOW: &str = "hot_balance_low";
pub const E_INTERNAL: &str = "internal_error";

/// Reads are retried a few times before a claim is failed with `internal_error`.
const READ_ATTEMPTS: u32 = 6;
const READ_BACKOFF: Duration = Duration::from_secs(5);

impl Worker {
    pub async fn run(self) {
        info!("payout worker started; resuming pending rows");
        loop {
            let next = match self.db.next_pending() {
                Ok(n) => n,
                Err(e) => {
                    error!("ledger read failed: {e:#}; retrying in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            match next {
                Some(claim) => {
                    if let Err(e) = self.process(claim).await {
                        error!("claim processing error: {e:#}");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
                None => self.notify.notified().await,
            }
        }
    }

    /// Runs one claim to a terminal state (or leaves it `submitted` only on a ledger error).
    pub async fn process(&self, claim: Claim) -> anyhow::Result<()> {
        let id = claim.request_id.clone();
        let recipient_b58 = claim
            .recipient
            .strip_prefix("Public/")
            .unwrap_or(&claim.recipient)
            .to_owned();

        let (tx_hash, balance_before) = match (claim.status, claim.tx_hash.clone()) {
            (Status::Queued, None) => {
                match self.submit(&claim, &recipient_b58).await? {
                    Some(hash) => {
                        let before = self.db.get(&id)?.and_then(|c| c.balance_before);
                        (hash, before)
                    }
                    None => return Ok(()), // failed terminally in submit()
                }
            }
            (Status::Submitted, Some(hash)) => {
                info!(request_id = %id, tx_hash = %hash, "resuming inclusion poll");
                (hash, claim.balance_before)
            }
            (status, hash) => {
                // A `queued` row with a hash (crashed between send and mark) must never be
                // re-sent; treat it as submitted. A `submitted` row without a hash is corrupt.
                if let Some(hash) = hash {
                    warn!(request_id = %id, ?status, "row has tx_hash but status {status:?}; polling");
                    (hash, claim.balance_before)
                } else {
                    self.db.mark_failed(
                        &id,
                        E_INTERNAL,
                        "ledger row in an inconsistent state",
                        None,
                        Utc::now(),
                    )?;
                    return Ok(());
                }
            }
        };

        self.await_inclusion(&id, &recipient_b58, &tx_hash, balance_before)
            .await
    }

    /// Checks the hot balance, records `balance_before`, signs and sends. Returns the hash on
    /// success; `None` when the claim was failed terminally.
    async fn submit(&self, claim: &Claim, recipient: &str) -> anyhow::Result<Option<String>> {
        let id = &claim.request_id;
        let hot = self.chain.hot_account().to_owned();

        let reserve = match self.chain.fee_state().await {
            Ok(q) => policy::fee_reserve(self.chain.gas_limit(), &q),
            Err(e) => {
                warn!("getFeeState failed ({e}); using fallback reserve");
                policy::fee_reserve(self.chain.gas_limit(), &crate::chain::FeeQuote::FALLBACK)
            }
        };
        let hot_balance = match self.read_balance(&hot).await {
            Ok(b) => b,
            Err(e) => {
                self.fail(
                    id,
                    E_INTERNAL,
                    &format!("hot balance unreadable: {e}"),
                    None,
                )?;
                return Ok(None);
            }
        };
        if hot_balance < self.cfg.prize.saturating_add(reserve) {
            warn!(
                hot_balance,
                reserve, "hot balance too low to pay out; failing claim {id}"
            );
            self.fail(
                id,
                E_HOT_BALANCE_LOW,
                &format!(
                    "hot balance {hot_balance} < prize {} + reserve {reserve}",
                    self.cfg.prize
                ),
                None,
            )?;
            return Ok(None);
        }

        let before = match self.read_balance(recipient).await {
            Ok(b) => b,
            Err(e) => {
                self.fail(
                    id,
                    E_INTERNAL,
                    &format!("recipient balance unreadable: {e}"),
                    None,
                )?;
                return Ok(None);
            }
        };
        self.db.set_balance_before(id, before, Utc::now())?;

        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            match self.chain.send(recipient, self.cfg.prize).await {
                Ok(hash) => {
                    self.db.mark_submitted(id, &hash, Utc::now())?;
                    info!(request_id = %id, tx_hash = %hash, recipient, "transfer submitted");
                    return Ok(Some(hash));
                }
                Err(ChainError::Rpc(m)) if attempt < READ_ATTEMPTS => {
                    warn!(request_id = %id, "pre-send read failed ({m}); retry {attempt}");
                    tokio::time::sleep(READ_BACKOFF).await;
                }
                Err(ChainError::Rpc(m)) => {
                    self.fail(id, E_INTERNAL, &format!("sequencer unreachable: {m}"), None)?;
                    return Ok(None);
                }
                Err(ChainError::InsufficientFunds(m)) => {
                    self.fail(id, E_HOT_BALANCE_LOW, &m, None)?;
                    return Ok(None);
                }
                Err(ChainError::Rejected(m)) => {
                    self.fail(id, E_SUBMISSION_REJECTED, &m, None)?;
                    return Ok(None);
                }
                Err(ChainError::Unknown(m)) => {
                    // The sequencer may have the transaction; we have no hash to poll with.
                    self.fail(
                        id,
                        E_OUTCOME_UNKNOWN,
                        &format!("send did not complete: {m}; reconcile on chain"),
                        None,
                    )?;
                    return Ok(None);
                }
            }
        }
    }

    async fn await_inclusion(
        &self,
        id: &str,
        recipient: &str,
        tx_hash: &str,
        balance_before: Option<u128>,
    ) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(self.cfg.poll_secs);
        let interval = Duration::from_secs(self.cfg.poll_interval_secs);
        loop {
            match self.chain.poll(tx_hash).await {
                Ok(TxStatus::Included { block }) => {
                    info!(request_id = %id, tx_hash, block, "transaction included");
                    let after = match self.read_balance(recipient).await {
                        Ok(a) => a,
                        Err(e) => {
                            self.fail(
                                id,
                                E_OUTCOME_UNKNOWN,
                                &format!("included in block {block} but balance unreadable: {e}"),
                                None,
                            )?;
                            return Ok(());
                        }
                    };
                    let expected = balance_before.map(|b| b.saturating_add(self.cfg.prize));
                    if expected == Some(after) {
                        self.db.mark_included(id, after, Utc::now())?;
                        info!(request_id = %id, after, "claim included and verified");
                    } else {
                        self.fail(
                            id,
                            E_OUTCOME_UNKNOWN,
                            &format!(
                                "included in block {block} but balance_after {after} != balance_before {:?} + prize {}; reconcile on chain",
                                balance_before, self.cfg.prize
                            ),
                            Some(after),
                        )?;
                    }
                    return Ok(());
                }
                Ok(TxStatus::Pending) => {}
                Err(e) => warn!(request_id = %id, "poll error: {e}"),
            }
            if tokio::time::Instant::now() >= deadline {
                self.fail(
                    id,
                    E_OUTCOME_UNKNOWN,
                    &format!(
                        "inclusion not observed within {}s; reconcile on chain with tx_hash",
                        self.cfg.poll_secs
                    ),
                    None,
                )?;
                return Ok(());
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn read_balance(&self, account: &str) -> Result<u128, ChainError> {
        let mut last = ChainError::Rpc("no attempts".into());
        for attempt in 1..=READ_ATTEMPTS {
            match self.chain.balance(account).await {
                Ok(b) => return Ok(b),
                Err(e) => {
                    warn!("balance read for {account} failed (attempt {attempt}): {e}");
                    last = e;
                    if attempt < READ_ATTEMPTS {
                        tokio::time::sleep(READ_BACKOFF).await;
                    }
                }
            }
        }
        Err(last)
    }

    fn fail(&self, id: &str, code: &str, message: &str, after: Option<u128>) -> anyhow::Result<()> {
        warn!(request_id = %id, code, message, "claim failed");
        self.db.mark_failed(id, code, message, after, Utc::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::mock::{MockChain, HOT};

    const R1: &str = "Public/4sAkk9FjWpTirvhXLcE3iXsT2K3J8XfUdd31X5v4y8s9";
    const R1_B58: &str = "4sAkk9FjWpTirvhXLcE3iXsT2K3J8XfUdd31X5v4y8s9";
    const PRIZE: u128 = 100_000_000;

    fn worker(chain: Arc<MockChain>) -> (Worker, Arc<Db>) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let w = Worker {
            db: db.clone(),
            chain,
            cfg: WorkerConfig {
                prize: PRIZE,
                poll_secs: 2,
                poll_interval_secs: 1,
            },
            notify: Arc::new(Notify::new()),
        };
        (w, db)
    }

    #[tokio::test(start_paused = true)]
    async fn queued_claim_is_paid_and_verified() {
        let chain = Arc::new(MockChain::new(130_000_000));
        chain.set_balance(R1_B58, 7);
        chain.with(|s| s.include_after = 2);
        let (w, db) = worker(chain.clone());
        let claim = db.insert_queued("a", R1, "h", Utc::now()).unwrap();
        w.process(claim).await.unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(c.status, Status::Included);
        assert_eq!(c.balance_before, Some(7));
        assert_eq!(c.balance_after, Some(7 + PRIZE));
        assert!(c.tx_hash.is_some());
        assert_eq!(chain.sent(), vec![(R1_B58.to_owned(), PRIZE)]);
        assert_eq!(chain.with(|s| s.balances[HOT]), 30_000_000);
    }

    #[tokio::test(start_paused = true)]
    async fn included_but_balance_mismatch_is_outcome_unknown() {
        let chain = Arc::new(MockChain::new(130_000_000));
        chain.with(|s| s.skip_credit = true);
        let (w, db) = worker(chain);
        let claim = db.insert_queued("a", R1, "h", Utc::now()).unwrap();
        w.process(claim).await.unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(c.status, Status::Failed);
        assert_eq!(c.error_code.as_deref(), Some(E_OUTCOME_UNKNOWN));
        assert!(c.tx_hash.is_some());
        assert_eq!(c.balance_after, Some(0));
    }

    #[tokio::test(start_paused = true)]
    async fn rejection_and_low_balance_map_to_their_codes() {
        let chain = Arc::new(MockChain::new(130_000_000));
        chain.with(|s| {
            s.send_results
                .push_back(Err(ChainError::Rejected("nonce".into())))
        });
        let (w, db) = worker(chain.clone());
        let claim = db.insert_queued("a", R1, "h", Utc::now()).unwrap();
        w.process(claim).await.unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(
            (c.status, c.error_code.as_deref()),
            (Status::Failed, Some(E_SUBMISSION_REJECTED))
        );
        assert_eq!(c.tx_hash, None);

        chain.set_balance(HOT, PRIZE); // below prize + reserve
        let claim = db.insert_queued("b", R1, "h", Utc::now()).unwrap();
        w.process(claim).await.unwrap();
        let c = db.get("b").unwrap().unwrap();
        assert_eq!(
            (c.status, c.error_code.as_deref()),
            (Status::Failed, Some(E_HOT_BALANCE_LOW))
        );
        assert!(chain.sent().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn poll_window_expiry_is_outcome_unknown_and_keeps_the_hash() {
        let chain = Arc::new(MockChain::new(130_000_000));
        chain.with(|s| s.include_after = 1_000);
        let (w, db) = worker(chain);
        let claim = db.insert_queued("a", R1, "h", Utc::now()).unwrap();
        w.process(claim).await.unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(c.status, Status::Failed);
        assert_eq!(c.error_code.as_deref(), Some(E_OUTCOME_UNKNOWN));
        assert!(c.tx_hash.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn submitted_row_is_repolled_never_resigned() {
        let chain = Arc::new(MockChain::new(130_000_000));
        // Pretend a previous process sent this transaction; the mock knows it as pending.
        let hash = chain.send(R1_B58, PRIZE).await.unwrap();
        chain.with(|s| s.sent.clear());
        let (w, db) = worker(chain.clone());
        let now = Utc::now();
        db.insert_queued("a", R1, "h", now).unwrap();
        db.set_balance_before("a", 0, now).unwrap();
        db.mark_submitted("a", &hash, now).unwrap();
        let claim = db.get("a").unwrap().unwrap();
        w.process(claim).await.unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(c.status, Status::Included);
        assert_eq!(c.tx_hash.as_deref(), Some(hash.as_str()));
        assert!(chain.sent().is_empty(), "resume must not sign again");
    }

    #[tokio::test(start_paused = true)]
    async fn run_loop_drains_queue_in_order_and_sleeps_when_idle() {
        let chain = Arc::new(MockChain::new(1_000_000_000));
        let (w, db) = worker(chain.clone());
        let notify = w.notify.clone();
        let now = Utc::now();
        db.insert_queued("first", R1, "h", now - chrono::Duration::seconds(2))
            .unwrap();
        db.insert_queued(
            "second",
            "Public/5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z",
            "h",
            now,
        )
        .unwrap();
        let handle = tokio::spawn(w.run());
        tokio::time::sleep(Duration::from_secs(30)).await;
        let sent: Vec<_> = chain.sent().into_iter().map(|(to, _)| to).collect();
        assert_eq!(sent, vec![R1_B58.to_owned(), HOT.to_owned()]);
        assert_eq!(db.count_pending().unwrap(), 0);

        db.insert_queued("third", R1, "h", Utc::now()).unwrap();
        notify.notify_one();
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(chain.sent().len(), 3);
        handle.abort();
    }
}
