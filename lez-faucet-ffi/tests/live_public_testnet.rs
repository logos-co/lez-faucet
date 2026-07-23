use std::path::Path;

use anyhow::{ensure, Context as _};
use common::HashType;
use lee::AccountId;
use lez_faucet_ffi::{CreatedWallet, FaucetClient, PINATA_PRIZE};
use sequencer_service_rpc::{RpcClient as _, SequencerClientBuilder};
use zeroize::Zeroize as _;

const LIVE_CONFIRMATION: &str = "I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ";
const PUBLIC_TESTNET: &str = "https://testnet.lez.logos.co";
const SAFE_PINATA_DIFFICULTY: u8 = 3;

async fn create_isolated_wallet(root: &Path, sequencer_url: &str) -> anyhow::Result<FaucetClient> {
    let CreatedWallet {
        client,
        mut mnemonic,
        ..
    } = FaucetClient::create(
        root.join("config.json"),
        root.join("storage.json"),
        sequencer_url,
        "testnet-only",
    )
    .await?;
    mnemonic.zeroize();
    drop(mnemonic);
    Ok(client)
}

fn public_sequencer_url() -> String {
    std::env::var("LEZ_FAUCET_SEQUENCER_URL").unwrap_or_else(|_| PUBLIC_TESTNET.to_owned())
}

fn require_external_funding_live_gate() {
    assert_eq!(
        std::env::var("LEZ_FAUCET_RUN_LIVE").as_deref(),
        Ok(LIVE_CONFIRMATION),
        "set LEZ_FAUCET_RUN_LIVE={LIVE_CONFIRMATION} in addition to passing --ignored"
    );
}

#[tokio::test]
#[ignore = "mutates the public LEZ testnet and consumes one 150 LEZ Piñata claim"]
async fn create_initialize_and_claim_once_on_public_testnet() {
    assert_eq!(
        std::env::var("LEZ_FAUCET_LIVE_TEST").as_deref(),
        Ok(LIVE_CONFIRMATION),
        "set LEZ_FAUCET_LIVE_TEST={LIVE_CONFIRMATION} in addition to passing --ignored"
    );

    let temp = tempfile::tempdir().expect("temporary wallet directory");
    let config = temp.path().join("wallet/config.json");
    let storage = temp.path().join("wallet/storage.json");
    let created = FaucetClient::create(config, storage, PUBLIC_TESTNET, "testnet-only")
        .await
        .expect("create wallet");
    let mut client = created.client;
    // Never print the mnemonic; remove the extra in-memory copy immediately.
    drop(created.mnemonic);

    let fingerprint = client
        .verify_program_fingerprint()
        .await
        .expect("v0.2.0 fingerprint");
    assert_eq!(
        fingerprint.pinata,
        "66f6a58d92c159c3c13ea54d1e37a68a814f0fd3b8fd44b7d35c0617ac4456f8"
    );

    let initialized = client
        .create_and_initialize_public_account()
        .await
        .expect("signed account initialization");
    assert_eq!(initialized.balance, 0);
    let account_id = initialized.account_id.parse().expect("account ID");

    let receipt = client
        .claim_once(account_id)
        .await
        .expect("unsigned Piñata claim");
    assert_eq!(receipt.balance_before, 0);
    assert_eq!(receipt.balance_after, PINATA_PRIZE);

    eprintln!(
        "live LEZ faucet proof: account={} init_tx={} claim_tx={} balance={}",
        initialized.account_id,
        initialized
            .init_tx_hash
            .as_deref()
            .unwrap_or("already-initialized"),
        receipt.tx_hash.as_deref().unwrap_or("unknown"),
        receipt.balance_after
    );
}

/// Run only as an optimized binary so difficulty-3 proof of work stays within
/// the same 60-second solver limit used by the product:
///
/// ```text
/// LEZ_FAUCET_RUN_LIVE=I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ \
/// cargo test --release -p lez-faucet-ffi --test live_public_testnet \
/// client_wallet_funds_distinct_external_public_account_on_public_testnet \
/// -- --ignored --exact --nocapture
/// ```
#[tokio::test]
#[ignore = "run with cargo test --release; initializes two public accounts and consumes one 150 LEZ Piñata claim"]
async fn client_wallet_funds_distinct_external_public_account_on_public_testnet(
) -> anyhow::Result<()> {
    require_external_funding_live_gate();

    let sequencer_url = public_sequencer_url();
    let temp = tempfile::tempdir().context("temporary live-test wallet root")?;

    // Wallet A is only the faucet client context. Its recovery phrase is
    // zeroized before any network mutation or account initialization.
    let mut client_a = create_isolated_wallet(&temp.path().join("wallet-a"), &sequencer_url)
        .await
        .context("create isolated faucet client wallet A")?;
    client_a
        .verify_program_fingerprint()
        .await
        .context("public testnet program fingerprint preflight")?;

    // Abort before account initialization if the public Piñata pool or
    // challenge is not healthy enough for one bounded claim.
    let rpc = SequencerClientBuilder::default()
        .build(&sequencer_url)
        .context("connect public sequencer for live preflight")?;
    let pinata_id = system_accounts::pinata_account_id();
    let pinata = rpc
        .get_account(pinata_id)
        .await
        .context("fetch public Piñata pool")?;
    ensure!(
        pinata.program_owner == programs::pinata().id(),
        "public Piñata pool has an unexpected program owner"
    );
    ensure!(
        pinata.balance >= PINATA_PRIZE,
        "public Piñata pool cannot fund one {PINATA_PRIZE} LEZ claim"
    );
    let challenge = pinata.data.as_ref();
    ensure!(
        challenge.len() == 33,
        "public Piñata challenge must be 33 bytes"
    );
    ensure!(
        challenge[0] <= SAFE_PINATA_DIFFICULTY,
        "public Piñata difficulty {} exceeds live-test safety limit {SAFE_PINATA_DIFFICULTY}",
        challenge[0]
    );

    let mut client_b = create_isolated_wallet(&temp.path().join("wallet-b"), &sequencer_url)
        .await
        .context("create isolated recipient wallet B")?;
    let account_a = client_a
        .create_and_initialize_public_account()
        .await
        .context("initialize wallet A public account")?;
    let account_b = client_b
        .create_and_initialize_public_account()
        .await
        .context("initialize wallet B public recipient")?;
    ensure!(
        account_a.account_id != account_b.account_id,
        "wallet A and wallet B unexpectedly derived the same public account"
    );

    let account_a_id: AccountId = account_a
        .account_id
        .parse()
        .context("wallet A account ID")?;
    let account_b_id: AccountId = account_b
        .account_id
        .parse()
        .context("wallet B account ID")?;

    // This balance lookup is also the production preflight: it proves wallet
    // B's recipient is initialized and owned by authenticated-transfer.
    let balance_before = client_a
        .balance(account_b_id)
        .await
        .context("wallet A preflight for wallet B recipient")?;
    ensure!(
        balance_before == account_b.balance,
        "wallet A and wallet B disagree on recipient balance before claim"
    );

    // Invoke exactly once. FaucetClient reconciles a known/unknown submission
    // outcome internally and never retries an ambiguous outcome.
    let receipt = client_a
        .claim_once(account_b_id)
        .await
        .context("wallet A claim for wallet B recipient")?;
    let expected_balance = balance_before
        .checked_add(PINATA_PRIZE)
        .context("recipient balance overflow")?;
    ensure!(
        receipt.balance_before == balance_before,
        "claim receipt has an unexpected starting balance"
    );
    ensure!(
        receipt.balance_after == expected_balance,
        "wallet B recipient did not increase by exactly {PINATA_PRIZE} LEZ"
    );
    ensure!(
        client_a.balance(account_b_id).await? == expected_balance,
        "wallet A does not observe the exact post-claim recipient balance"
    );
    ensure!(
        client_b.balance(account_b_id).await? == expected_balance,
        "wallet B does not observe the exact post-claim recipient balance"
    );

    let claim_tx_hash = receipt
        .tx_hash
        .as_deref()
        .context("confirmed live claim did not return a transaction hash")?;
    let claim_hash: HashType = claim_tx_hash.parse().context("claim transaction hash")?;
    let claim_tx = rpc
        .get_transaction(claim_hash)
        .await
        .context("resolve confirmed claim transaction")?
        .context("confirmed claim transaction is missing from the sequencer")?;
    ensure!(
        claim_tx.hash() == claim_hash,
        "resolved claim transaction hash does not match its receipt"
    );
    let affected_accounts = claim_tx.affected_public_account_ids();
    ensure!(
        affected_accounts.contains(&account_b_id),
        "resolved claim transaction does not target wallet B's recipient"
    );
    ensure!(
        !affected_accounts.contains(&account_a_id),
        "resolved claim transaction unexpectedly targets wallet A"
    );

    eprintln!(
        "live external funding proof: client_account={} recipient_account={} client_init_tx={} recipient_init_tx={} claim_tx={} balance_before={} balance_after={}",
        account_a.account_id,
        account_b.account_id,
        account_a
            .init_tx_hash
            .as_deref()
            .unwrap_or("already-initialized"),
        account_b
            .init_tx_hash
            .as_deref()
            .unwrap_or("already-initialized"),
        claim_tx_hash,
        balance_before,
        receipt.balance_after
    );

    drop(client_a);
    drop(client_b);
    temp.close().context("remove isolated live-test wallets")?;
    Ok(())
}
