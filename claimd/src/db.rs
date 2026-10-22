//! The durable idempotency ledger: one SQLite row per `request_id`.

use std::{path::Path, sync::Mutex};

use anyhow::{Context as _, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension as _, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Queued,
    Submitted,
    Included,
    Failed,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Submitted => "submitted",
            Self::Included => "included",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => Self::Queued,
            "submitted" => Self::Submitted,
            "included" => Self::Included,
            "failed" => Self::Failed,
            other => anyhow::bail!("unknown claim status in ledger: {other}"),
        })
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Included | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub request_id: String,
    pub recipient: String,
    pub ip_hash: String,
    pub status: Status,
    pub tx_hash: Option<String>,
    pub balance_before: Option<u128>,
    pub balance_after: Option<u128>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Which limit window a counting query covers.
#[derive(Debug, Clone, Copy)]
pub enum Scope<'a> {
    Recipient(&'a str),
    Ip(&'a str),
    Global,
}

pub struct Db {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS claims (
    request_id      TEXT PRIMARY KEY,
    recipient       TEXT NOT NULL,
    ip_hash         TEXT NOT NULL,
    status          TEXT NOT NULL,
    tx_hash         TEXT,
    balance_before  TEXT,
    balance_after   TEXT,
    error_code      TEXT,
    error_message   TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS claims_recipient_created ON claims(recipient, created_at);
CREATE INDEX IF NOT EXISTS claims_ip_created ON claims(ip_hash, created_at);
CREATE INDEX IF NOT EXISTS claims_status_created ON claims(status, created_at);
"#;

/// A claim counts against the limits unless it failed without ever reaching the chain.
const COUNTED: &str = "(status != 'failed' OR tx_hash IS NOT NULL)";

pub fn ts(t: DateTime<Utc>) -> String {
    // Fixed-width UTC form so string comparison in SQL equals time comparison.
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("bad timestamp in ledger: {s}"))?
        .with_timezone(&Utc))
}

fn opt_u128(s: Option<String>) -> Result<Option<u128>> {
    s.map(|v| {
        v.parse::<u128>()
            .with_context(|| format!("bad amount in ledger: {v}"))
    })
    .transpose()
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening sqlite at {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA).context("applying schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> Result<R> {
        let conn = self.conn.lock().expect("ledger mutex poisoned");
        f(&conn).map_err(Into::into)
    }

    fn row(row: &Row<'_>) -> rusqlite::Result<Claim> {
        let conv = |e: anyhow::Error| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        };
        Ok(Claim {
            request_id: row.get(0)?,
            recipient: row.get(1)?,
            ip_hash: row.get(2)?,
            status: Status::parse(&row.get::<_, String>(3)?).map_err(conv)?,
            tx_hash: row.get(4)?,
            balance_before: opt_u128(row.get(5)?).map_err(conv)?,
            balance_after: opt_u128(row.get(6)?).map_err(conv)?,
            error_code: row.get(7)?,
            error_message: row.get(8)?,
            created_at: parse_ts(&row.get::<_, String>(9)?).map_err(conv)?,
            updated_at: parse_ts(&row.get::<_, String>(10)?).map_err(conv)?,
        })
    }

    const COLS: &'static str = "request_id, recipient, ip_hash, status, tx_hash, balance_before, \
                                balance_after, error_code, error_message, created_at, updated_at";

    pub fn get(&self, request_id: &str) -> Result<Option<Claim>> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {} FROM claims WHERE request_id = ?1", Self::COLS),
                params![request_id],
                Self::row,
            )
            .optional()
        })
    }

    pub fn insert_queued(
        &self,
        request_id: &str,
        recipient: &str,
        ip_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<Claim> {
        let t = ts(now);
        self.with(|c| {
            c.execute(
                "INSERT INTO claims (request_id, recipient, ip_hash, status, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 'queued', ?4, ?4)",
                params![request_id, recipient, ip_hash, t],
            )
        })?;
        self.get(request_id)?
            .context("row vanished right after insert")
    }

    /// Counted claims inside `[since, now]` for the scope.
    pub fn count_since(&self, scope: Scope<'_>, since: DateTime<Utc>) -> Result<u32> {
        let s = ts(since);
        self.with(|c| match scope {
            Scope::Recipient(r) => c.query_row(
                &format!("SELECT COUNT(*) FROM claims WHERE recipient = ?1 AND created_at >= ?2 AND {COUNTED}"),
                params![r, s],
                |r| r.get(0),
            ),
            Scope::Ip(h) => c.query_row(
                &format!("SELECT COUNT(*) FROM claims WHERE ip_hash = ?1 AND created_at >= ?2 AND {COUNTED}"),
                params![h, s],
                |r| r.get(0),
            ),
            Scope::Global => c.query_row(
                &format!("SELECT COUNT(*) FROM claims WHERE created_at >= ?1 AND {COUNTED}"),
                params![s],
                |r| r.get(0),
            ),
        })
    }

    /// `created_at` of the oldest counted claim inside the window (drives `retry_after_seconds`).
    pub fn oldest_since(
        &self,
        scope: Scope<'_>,
        since: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>> {
        let s = ts(since);
        let v: Option<String> = self.with(|c| match scope {
            Scope::Recipient(r) => c.query_row(
                &format!("SELECT MIN(created_at) FROM claims WHERE recipient = ?1 AND created_at >= ?2 AND {COUNTED}"),
                params![r, s],
                |r| r.get(0),
            ),
            Scope::Ip(h) => c.query_row(
                &format!("SELECT MIN(created_at) FROM claims WHERE ip_hash = ?1 AND created_at >= ?2 AND {COUNTED}"),
                params![h, s],
                |r| r.get(0),
            ),
            Scope::Global => c.query_row(
                &format!("SELECT MIN(created_at) FROM claims WHERE created_at >= ?1 AND {COUNTED}"),
                params![s],
                |r| r.get(0),
            ),
        })?;
        v.as_deref().map(parse_ts).transpose()
    }

    /// Claims the worker still owes money for (queued or submitted), oldest first.
    pub fn pending(&self) -> Result<Vec<Claim>> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {} FROM claims WHERE status IN ('queued','submitted') ORDER BY created_at ASC, request_id ASC",
                Self::COLS
            ))?;
            let rows = st.query_map([], Self::row)?;
            rows.collect()
        })
    }

    pub fn next_pending(&self) -> Result<Option<Claim>> {
        Ok(self.pending()?.into_iter().next())
    }

    pub fn count_pending(&self) -> Result<u32> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM claims WHERE status IN ('queued','submitted')",
                [],
                |r| r.get(0),
            )
        })
    }

    pub fn set_balance_before(
        &self,
        request_id: &str,
        before: u128,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE claims SET balance_before = ?2, updated_at = ?3 WHERE request_id = ?1",
                params![request_id, before.to_string(), ts(now)],
            )
        })?;
        Ok(())
    }

    pub fn mark_submitted(
        &self,
        request_id: &str,
        tx_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE claims SET status = 'submitted', tx_hash = ?2, updated_at = ?3 \
                 WHERE request_id = ?1 AND status = 'queued'",
                params![request_id, tx_hash, ts(now)],
            )
        })?;
        Ok(())
    }

    pub fn mark_included(&self, request_id: &str, after: u128, now: DateTime<Utc>) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE claims SET status = 'included', balance_after = ?2, error_code = NULL, \
                 error_message = NULL, updated_at = ?3 WHERE request_id = ?1",
                params![request_id, after.to_string(), ts(now)],
            )
        })?;
        Ok(())
    }

    pub fn mark_failed(
        &self,
        request_id: &str,
        code: &str,
        message: &str,
        after: Option<u128>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE claims SET status = 'failed', error_code = ?2, error_message = ?3, \
                 balance_after = COALESCE(?4, balance_after), updated_at = ?5 WHERE request_id = ?1",
                params![request_id, code, message, after.map(|a| a.to_string()), ts(now)],
            )
        })?;
        Ok(())
    }
}

pub fn day_ago(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::hours(24)
}

pub fn hour_ago(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::hours(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R1: &str = "Public/5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z";
    const R2: &str = "Public/4sAkk9FjWpTirvhXLcE3iXsT2K3J8XfUdd31X5v4y8s9";

    #[test]
    fn insert_get_roundtrip_and_transitions() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now();
        let c = db.insert_queued("a", R1, "h1", now).unwrap();
        assert_eq!(c.status, Status::Queued);
        assert_eq!(c.tx_hash, None);
        assert_eq!(db.get("a").unwrap().unwrap(), c);
        assert!(db.get("b").unwrap().is_none());

        db.set_balance_before("a", 5, now).unwrap();
        db.mark_submitted("a", "ff", now).unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(c.status, Status::Submitted);
        assert_eq!(c.balance_before, Some(5));
        assert_eq!(c.tx_hash.as_deref(), Some("ff"));

        // mark_submitted is a no-op once the row has left `queued` (never re-sign).
        db.mark_submitted("a", "other", now).unwrap();
        assert_eq!(db.get("a").unwrap().unwrap().tx_hash.as_deref(), Some("ff"));

        db.mark_included("a", 105, now).unwrap();
        let c = db.get("a").unwrap().unwrap();
        assert_eq!(c.status, Status::Included);
        assert_eq!(c.balance_after, Some(105));
        assert!(db.pending().unwrap().is_empty());
    }

    #[test]
    fn duplicate_request_id_is_rejected_by_the_primary_key() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now();
        db.insert_queued("a", R1, "h1", now).unwrap();
        assert!(db.insert_queued("a", R2, "h1", now).is_err());
    }

    #[test]
    fn windows_count_only_claims_that_reached_the_chain() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc::now();
        db.insert_queued("old", R1, "h1", now - Duration::hours(25))
            .unwrap();
        db.insert_queued("fresh", R1, "h1", now - Duration::minutes(30))
            .unwrap();
        db.insert_queued("failed-clean", R1, "h2", now - Duration::minutes(10))
            .unwrap();
        db.mark_failed("failed-clean", "hot_balance_low", "x", None, now)
            .unwrap();
        db.insert_queued("failed-sent", R2, "h2", now - Duration::minutes(5))
            .unwrap();
        db.mark_submitted("failed-sent", "aa", now).unwrap();
        db.mark_failed("failed-sent", "outcome_unknown", "x", None, now)
            .unwrap();

        let day = day_ago(now);
        assert_eq!(db.count_since(Scope::Recipient(R1), day).unwrap(), 1);
        assert_eq!(db.count_since(Scope::Recipient(R2), day).unwrap(), 1);
        assert_eq!(db.count_since(Scope::Ip("h1"), day).unwrap(), 1);
        assert_eq!(db.count_since(Scope::Ip("h2"), day).unwrap(), 1);
        assert_eq!(db.count_since(Scope::Global, day).unwrap(), 2);
        assert_eq!(db.count_since(Scope::Global, hour_ago(now)).unwrap(), 2);
        assert_eq!(
            db.oldest_since(Scope::Recipient(R1), day).unwrap().map(ts),
            Some(ts(now - Duration::minutes(30)))
        );
        assert_eq!(db.count_pending().unwrap(), 2);
        let pending: Vec<_> = db
            .pending()
            .unwrap()
            .into_iter()
            .map(|c| c.request_id)
            .collect();
        assert_eq!(pending, vec!["old", "fresh"]);
    }
}
