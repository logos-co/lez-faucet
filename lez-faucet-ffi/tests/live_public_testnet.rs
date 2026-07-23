use lez_faucet_ffi::{FaucetClient, PINATA_PRIZE};

const LIVE_CONFIRMATION: &str = "I_UNDERSTAND_THIS_SPENDS_150_TESTNET_LEZ";
const PUBLIC_TESTNET: &str = "https://testnet.lez.logos.co";

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
