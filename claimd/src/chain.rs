//! The chain backend boundary. Everything the service needs from LEZ goes through [`Chain`],
//! so policy, storage and the HTTP layer compile and test without the LEZ crates; the real
//! backend lives in `lez.rs` behind the `lez` feature.

use std::fmt;

use async_trait::async_trait;

/// Why a chain call failed. The split matters for the payout state machine: only
/// [`ChainError::Unknown`] leaves a transaction possibly in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(feature = "lez"), allow(dead_code))]
pub enum ChainError {
    /// The transaction was refused before or by the sequencer; nothing moved.
    Rejected(String),
    /// The hot account cannot cover `amount + reserve`; nothing moved.
    InsufficientFunds(String),
    /// Transport failure around `sendTransaction`: the sequencer may or may not have it.
    Unknown(String),
    /// A read (balance, fee state, transaction lookup, health) failed; safe to retry.
    Rpc(String),
}

impl fmt::Display for ChainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(m) => write!(f, "rejected: {m}"),
            Self::InsufficientFunds(m) => write!(f, "insufficient funds: {m}"),
            Self::Unknown(m) => write!(f, "outcome unknown: {m}"),
            Self::Rpc(m) => write!(f, "rpc: {m}"),
        }
    }
}

impl std::error::Error for ChainError {}

/// The subset of `getFeeState` the reserve formula needs. The `next_*` ceilings bound the
/// base fees of the block the transaction will land in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeQuote {
    pub height: u64,
    pub base_fee_exec: u64,
    pub base_fee_stor: u64,
    pub next_base_fee_exec_ceiling: u64,
    pub next_base_fee_stor_ceiling: u64,
}

impl FeeQuote {
    /// Genesis base fees (8/8) with a one-step ceiling; used when `getFeeState` is unreachable.
    pub const FALLBACK: Self = Self {
        height: 0,
        base_fee_exec: 8,
        base_fee_stor: 8,
        next_base_fee_exec_ceiling: 9,
        next_base_fee_stor_ceiling: 9,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "lez"), allow(dead_code))]
pub enum TxStatus {
    /// Not in any block yet (or unknown to the sequencer).
    Pending,
    Included {
        block: u64,
    },
}

/// Account ids cross this boundary as bare base58 strings (no `Public/` prefix); transaction
/// hashes as 64-char lowercase hex. Amounts are lepta.
#[async_trait]
pub trait Chain: Send + Sync + 'static {
    /// The hot account that signs and pays, bare base58.
    fn hot_account(&self) -> &str;
    /// The `gas_limit` the wallet signs transactions with (sizes the fee reserve).
    fn gas_limit(&self) -> u64;
    async fn balance(&self, account: &str) -> Result<u128, ChainError>;
    /// Signs and submits one native transfer `hot -> to`; returns the transaction hash.
    async fn send(&self, to: &str, amount: u128) -> Result<String, ChainError>;
    async fn poll(&self, tx_hash: &str) -> Result<TxStatus, ChainError>;
    async fn fee_state(&self) -> Result<FeeQuote, ChainError>;
    async fn health(&self) -> Result<(), ChainError>;
}

#[cfg(test)]
pub mod mock {
    //! An in-memory chain for the policy / worker / HTTP tests.

    use std::{
        collections::{HashMap, VecDeque},
        sync::Mutex,
    };

    use super::*;

    pub struct MockState {
        pub balances: HashMap<String, u128>,
        /// Transactions the mock has accepted: hash -> (to, amount, polls left before inclusion).
        pub txs: HashMap<String, (String, u128, u32)>,
        pub sent: Vec<(String, u128)>,
        pub send_results: VecDeque<Result<(), ChainError>>,
        /// How many `poll` calls a new transaction stays pending.
        pub include_after: u32,
        /// When set, inclusion does not credit the recipient (simulates a reverted transfer).
        pub skip_credit: bool,
        pub fee: Result<FeeQuote, ChainError>,
        pub health: Result<(), ChainError>,
        pub balance_errors: u32,
        next_hash: u64,
    }

    pub struct MockChain {
        pub hot: String,
        pub gas_limit: u64,
        pub state: Mutex<MockState>,
    }

    pub const HOT: &str = "5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z";

    impl MockChain {
        pub fn new(hot_balance: u128) -> Self {
            let mut st = MockState {
                balances: HashMap::new(),
                txs: HashMap::new(),
                sent: Vec::new(),
                send_results: VecDeque::new(),
                include_after: 0,
                skip_credit: false,
                fee: Ok(FeeQuote::FALLBACK),
                health: Ok(()),
                balance_errors: 0,
                next_hash: 0,
            };
            st.balances.insert(HOT.to_owned(), hot_balance);
            Self {
                hot: HOT.to_owned(),
                gas_limit: 2_000_000,
                state: Mutex::new(st),
            }
        }

        pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
            f(&mut self.state.lock().unwrap())
        }

        pub fn sent(&self) -> Vec<(String, u128)> {
            self.with(|s| s.sent.clone())
        }

        pub fn set_balance(&self, account: &str, v: u128) {
            self.with(|s| {
                s.balances.insert(account.to_owned(), v);
            });
        }
    }

    #[async_trait]
    impl Chain for MockChain {
        fn hot_account(&self) -> &str {
            &self.hot
        }

        fn gas_limit(&self) -> u64 {
            self.gas_limit
        }

        async fn balance(&self, account: &str) -> Result<u128, ChainError> {
            self.with(|s| {
                if s.balance_errors > 0 {
                    s.balance_errors -= 1;
                    return Err(ChainError::Rpc("mock balance error".into()));
                }
                Ok(*s.balances.get(account).unwrap_or(&0))
            })
        }

        async fn send(&self, to: &str, amount: u128) -> Result<String, ChainError> {
            self.with(|s| {
                if let Some(r) = s.send_results.pop_front() {
                    r?;
                }
                let hot_bal = *s.balances.get(HOT).unwrap_or(&0);
                if hot_bal < amount {
                    return Err(ChainError::InsufficientFunds("mock".into()));
                }
                s.next_hash += 1;
                let hash = format!("{:064x}", s.next_hash);
                s.sent.push((to.to_owned(), amount));
                let after = s.include_after;
                s.txs.insert(hash.clone(), (to.to_owned(), amount, after));
                Ok(hash)
            })
        }

        async fn poll(&self, tx_hash: &str) -> Result<TxStatus, ChainError> {
            self.with(|s| {
                if s.txs.contains_key(&format!("done:{tx_hash}")) {
                    return Ok(TxStatus::Included { block: 42 });
                }
                let skip = s.skip_credit;
                let Some((to, amount, left)) = s.txs.get_mut(tx_hash) else {
                    return Ok(TxStatus::Pending);
                };
                if *left > 0 {
                    *left -= 1;
                    return Ok(TxStatus::Pending);
                }
                let (to, amount) = (to.clone(), *amount);
                // Credit exactly once; later polls hit the `done:` sentinel above.
                if !skip {
                    if let Some(h) = s.balances.get_mut(HOT) {
                        *h = h.saturating_sub(amount);
                    }
                    *s.balances.entry(to).or_insert(0) += amount;
                }
                s.txs.remove(tx_hash);
                s.txs
                    .insert(format!("done:{tx_hash}"), (String::new(), 0, 0));
                Ok(TxStatus::Included { block: 42 })
            })
        }

        async fn fee_state(&self) -> Result<FeeQuote, ChainError> {
            self.with(|s| s.fee.clone())
        }

        async fn health(&self) -> Result<(), ChainError> {
            self.with(|s| s.health.clone())
        }
    }
}
