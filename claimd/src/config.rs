//! Runtime configuration from the environment (see README "Environment").

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context as _, Result};

use crate::policy::Limits;

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "lez"), allow(dead_code))]
pub struct Config {
    pub bind: SocketAddr,
    pub sequencer: String,
    pub db_path: PathBuf,
    pub prize: u128,
    pub limits: Limits,
    pub pause_file: PathBuf,
    pub wallet_home: Option<PathBuf>,
    /// `Public/<id>` or a wallet label; defaults to the label `faucet-hot`.
    pub hot_account: String,
    pub poll_secs: u64,
    pub poll_interval_secs: u64,
    /// Secret mixed into the IP hash; when unset, `<state dir>/ip_salt` is created on first start.
    pub ip_salt: Option<String>,
}

pub const NETWORK: &str = "lez-v03-testnet";
pub const LEZ_VERSION: &str = "v0.3.1";
pub const DECIMALS: u8 = 9;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T: std::str::FromStr>(name: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match env(name) {
        Some(v) => v
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("{name}={v:?}: {e}")),
        None => Ok(default),
    }
}

fn home() -> PathBuf {
    env("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let state_dir = home().join("lez-faucet").join("state");
        let db_path = env("CLAIMD_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir.join("claimd.sqlite"));
        let pause_file = env("CLAIMD_PAUSE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir.join("paused"));
        let cfg = Self {
            bind: env_parse("CLAIMD_BIND", "127.0.0.1:8787".parse().unwrap())?,
            sequencer: env("CLAIMD_SEQUENCER")
                .unwrap_or_else(|| "https://testnet.lez.logos.co".to_owned()),
            db_path,
            prize: env_parse("CLAIMD_PRIZE", 100_000_000)?,
            limits: Limits {
                per_recipient_per_day: env_parse("CLAIMD_PER_RECIPIENT_PER_DAY", 1)?,
                per_ip_per_day: env_parse("CLAIMD_PER_IP_PER_DAY", 2)?,
                per_hour: env_parse("CLAIMD_PER_HOUR", 3)?,
                per_day: env_parse("CLAIMD_PER_DAY", 10)?,
            },
            pause_file,
            wallet_home: env("LEE_WALLET_HOME_DIR").map(PathBuf::from),
            hot_account: env("CLAIMD_HOT_ACCOUNT").unwrap_or_else(|| "faucet-hot".to_owned()),
            poll_secs: env_parse("CLAIMD_POLL_SECS", 300)?,
            poll_interval_secs: env_parse("CLAIMD_POLL_INTERVAL_SECS", 5)?,
            ip_salt: env("CLAIMD_IP_SALT"),
        };
        anyhow::ensure!(cfg.prize > 0, "CLAIMD_PRIZE must be > 0");
        anyhow::ensure!(
            cfg.poll_interval_secs > 0,
            "CLAIMD_POLL_INTERVAL_SECS must be > 0"
        );
        Ok(cfg)
    }

    /// The IP-hash salt: env, else a file next to the ledger (created 0600 on first use).
    pub fn load_ip_salt(&self) -> Result<Vec<u8>> {
        if let Some(s) = &self.ip_salt {
            return Ok(s.as_bytes().to_vec());
        }
        let path = self
            .db_path
            .parent()
            .map(|d| d.join("ip_salt"))
            .context("CLAIMD_DB has no parent directory")?;
        if let Ok(existing) = std::fs::read(&path) {
            if !existing.is_empty() {
                return Ok(existing);
            }
        }
        use rand::RngCore as _;
        let mut salt = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let hex = hex::encode(salt);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_private(&path, hex.as_bytes())?;
        Ok(hex.into_bytes())
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("creating {}", path.display()))
}
