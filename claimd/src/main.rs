//! claimd: the LEZ v0.3 testnet faucet claim service. See README.md for the contract.

mod chain;
mod config;
mod db;
mod http;
#[cfg(feature = "lez")]
mod lez;
mod policy;
mod worker;

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context as _, Result};
use tokio::sync::Notify;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::{chain::Chain, config::Config, db::Db};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cfg = Config::from_env()?;
    info!(bind = %cfg.bind, db = %cfg.db_path.display(), prize = cfg.prize, "starting claimd");

    let db = Arc::new(Db::open(&cfg.db_path)?);
    let ip_salt = cfg.load_ip_salt()?;
    let chain = build_chain(&cfg)?;
    info!(hot_account = %format!("Public/{}", chain.hot_account()), gas_limit = chain.gas_limit(), "chain backend ready");

    let notify = Arc::new(Notify::new());
    let worker = worker::Worker {
        db: db.clone(),
        chain: chain.clone(),
        cfg: worker::WorkerConfig {
            prize: cfg.prize,
            poll_secs: cfg.poll_secs,
            poll_interval_secs: cfg.poll_interval_secs,
        },
        notify: notify.clone(),
    };
    tokio::spawn(worker.run());

    let state = http::AppState::new(
        db,
        chain,
        http::AppConfig {
            sequencer: cfg.sequencer.clone(),
            prize: cfg.prize,
            limits: cfg.limits,
            pause_file: cfg.pause_file.clone(),
            ip_salt,
        },
        notify,
    );
    let app = http::router(state);
    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("binding {}", cfg.bind))?;
    info!("listening on http://{}", cfg.bind);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    info!("claimd stopped");
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = term.recv() => {},
    }
    info!("shutdown signal received");
}

#[cfg(feature = "lez")]
fn build_chain(cfg: &Config) -> Result<Arc<dyn Chain>> {
    let home = cfg
        .wallet_home
        .clone()
        .context("LEE_WALLET_HOME_DIR is required (hot wallet home)")?;
    let backend = lez::LezChain::start(home, &cfg.hot_account, &cfg.sequencer)?;
    Ok(Arc::new(backend))
}

#[cfg(not(feature = "lez"))]
fn build_chain(_cfg: &Config) -> Result<Arc<dyn Chain>> {
    anyhow::bail!(
        "this claimd was built without the `lez` feature and has no chain backend; \
         build with `cargo build --release --features lez`"
    )
}
