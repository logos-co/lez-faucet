//! The HTTP API (README "HTTP API (v1)"). Handlers validate, consult the ledger and the
//! limits, enqueue, and answer; they never touch the signer.

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{rejection::JsonRejection, ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tracing::{info, warn};

use crate::{
    chain::{Chain, ChainError, FeeQuote},
    config::{DECIMALS, LEZ_VERSION, NETWORK},
    db::{day_ago, hour_ago, Claim, Db, Scope, Status},
    policy::{self, Limits, ValidationError, DAY, HOUR},
};

#[derive(Clone)]
pub struct AppConfig {
    pub sequencer: String,
    pub prize: u128,
    pub limits: Limits,
    pub pause_file: PathBuf,
    pub ip_salt: Vec<u8>,
}

pub struct AppState {
    pub db: Arc<Db>,
    pub chain: Arc<dyn Chain>,
    pub cfg: AppConfig,
    pub notify: Arc<Notify>,
    hot_cache: Mutex<Option<(Instant, u128)>>,
    fee_cache: Mutex<Option<(Instant, FeeQuote)>>,
}

const HOT_TTL: Duration = Duration::from_secs(5);
const FEE_TTL: Duration = Duration::from_secs(60);

impl AppState {
    pub fn new(
        db: Arc<Db>,
        chain: Arc<dyn Chain>,
        cfg: AppConfig,
        notify: Arc<Notify>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            chain,
            cfg,
            notify,
            hot_cache: Mutex::new(None),
            fee_cache: Mutex::new(None),
        })
    }

    pub fn paused(&self) -> bool {
        self.cfg.pause_file.exists()
    }

    #[cfg(test)]
    pub fn clear_caches(&self) {
        *self.hot_cache.lock().unwrap() = None;
        *self.fee_cache.lock().unwrap() = None;
    }

    pub async fn hot_balance(&self) -> Result<u128, ChainError> {
        if let Some((at, v)) = *self.hot_cache.lock().unwrap() {
            if at.elapsed() < HOT_TTL {
                return Ok(v);
            }
        }
        let v = self.chain.balance(self.chain.hot_account()).await?;
        *self.hot_cache.lock().unwrap() = Some((Instant::now(), v));
        Ok(v)
    }

    pub async fn reserve(&self) -> u128 {
        let cached = *self.fee_cache.lock().unwrap();
        let quote = match cached {
            Some((at, q)) if at.elapsed() < FEE_TTL => q,
            _ => match self.chain.fee_state().await {
                Ok(q) => {
                    *self.fee_cache.lock().unwrap() = Some((Instant::now(), q));
                    q
                }
                Err(e) => {
                    warn!("getFeeState failed ({e}); using fallback reserve");
                    FeeQuote::FALLBACK
                }
            },
        };
        policy::fee_reserve(self.chain.gas_limit(), &quote)
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/claim", post(claim))
        .route("/v1/claim/:request_id", get(get_claim))
        .route("/healthz", get(healthz))
        .with_state(state)
}

// ---- errors ------------------------------------------------------------------------------

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: &'static str,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable,
            details: None,
        }
    }

    fn internal(e: impl std::fmt::Display) -> Self {
        warn!("internal error: {e}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "internal error",
            true,
        )
    }
}

impl From<ValidationError> for ApiError {
    fn from(e: ValidationError) -> Self {
        match e {
            ValidationError::InvalidPublicAccountId(m) => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_public_account_id",
                m,
                false,
            ),
            ValidationError::InvalidRequestKey(m) => {
                Self::new(StatusCode::BAD_REQUEST, "invalid_request_key", m, false)
            }
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(format!("{e:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut err = json!({
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
        });
        if let Some(d) = self.details {
            err["details"] = d;
        }
        (self.status, Json(json!({ "error": err }))).into_response()
    }
}

fn sequencer_unreachable(e: ChainError) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "sequencer_unavailable",
        format!("sequencer unreachable: {e}"),
        true,
    )
}

// ---- wire types --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    pub recipient: String,
    pub request_id: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimResponse {
    pub request_id: String,
    pub recipient: String,
    pub status: String,
    pub prize: String,
    pub tx_hash: Option<String>,
    pub balance_before: Option<String>,
    pub balance_after: Option<String>,
    pub error: Option<ClaimError>,
    pub created_at: String,
    pub updated_at: String,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl ClaimResponse {
    fn from_claim(c: Claim, prize: u128) -> Self {
        let error = match (&c.status, c.error_code.clone()) {
            (Status::Failed, Some(code)) => Some(ClaimError {
                code,
                message: c.error_message.clone().unwrap_or_default(),
            }),
            (Status::Failed, None) => Some(ClaimError {
                code: "internal_error".to_owned(),
                message: c.error_message.clone().unwrap_or_default(),
            }),
            _ => None,
        };
        Self {
            request_id: c.request_id,
            recipient: c.recipient,
            status: c.status.as_str().to_owned(),
            prize: prize.to_string(),
            tx_hash: c.tx_hash,
            balance_before: c.balance_before.map(|v| v.to_string()),
            balance_after: c.balance_after.map(|v| v.to_string()),
            error,
            created_at: rfc3339(c.created_at),
            updated_at: rfc3339(c.updated_at),
        }
    }
}

fn claim_response(c: Claim, prize: u128) -> Response {
    let status = if c.status.is_terminal() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    (status, Json(ClaimResponse::from_claim(c, prize))).into_response()
}

// ---- handlers ----------------------------------------------------------------------------

async fn info(State(st): State<Arc<AppState>>) -> Result<Response, ApiError> {
    let hot_balance = st.hot_balance().await.map_err(sequencer_unreachable)?;
    let reserve = st.reserve().await;
    let now = Utc::now();
    let used_today = st.db.count_since(Scope::Global, day_ago(now))?;
    let global_left = st.cfg.limits.per_day.saturating_sub(used_today);
    let by_balance = hot_balance.saturating_sub(reserve) / st.cfg.prize;
    let claims_remaining =
        u64::from(global_left).min(u64::try_from(by_balance).unwrap_or(u64::MAX));
    let l = st.cfg.limits;
    Ok(Json(json!({
        "network": NETWORK,
        "sequencer_url": st.cfg.sequencer,
        "lez_version": LEZ_VERSION,
        "prize": st.cfg.prize.to_string(),
        "decimals": DECIMALS,
        "hot_account": format!("Public/{}", st.chain.hot_account()),
        "hot_balance": hot_balance.to_string(),
        "claims_remaining": claims_remaining,
        "paused": st.paused(),
        "limits": {
            "per_recipient_per_day": l.per_recipient_per_day,
            "per_ip_per_day": l.per_ip_per_day,
            "per_hour": l.per_hour,
            "per_day": l.per_day,
        },
    }))
    .into_response())
}

async fn healthz(State(st): State<Arc<AppState>>) -> Response {
    let sequencer = st.chain.health().await;
    let hot = st.hot_balance().await;
    let reserve = st.reserve().await;
    let floor = st.cfg.prize.saturating_add(reserve);
    let paused = st.paused();
    let ok = sequencer.is_ok() && matches!(hot, Ok(b) if b >= floor);
    let body = json!({
        "ok": ok,
        "hot_balance": hot.as_ref().map(|b| b.to_string()).unwrap_or_else(|_| "unknown".to_owned()),
        "hot_floor": floor.to_string(),
        "paused": paused,
        "sequencer": match &sequencer { Ok(()) => "ok".to_owned(), Err(e) => format!("error: {e}") },
    });
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body)).into_response()
}

async fn get_claim(
    State(st): State<Arc<AppState>>,
    Path(request_id): Path<String>,
) -> Result<Response, ApiError> {
    policy::validate_request_id(&request_id)?;
    match st.db.get(&request_id)? {
        Some(c) => Ok(claim_response(c, st.cfg.prize)),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_job",
            "no claim with that request_id",
            false,
        )),
    }
}

async fn claim(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
    body: Result<Json<ClaimRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(req) = body.map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("bad JSON body: {e}"),
            false,
        )
    })?;
    let recipient = policy::validate_recipient(&req.recipient)?;
    let request_id = policy::validate_request_id(&req.request_id)?;

    // Idempotency first: a replay always answers, regardless of limits or pause.
    if let Some(existing) = st.db.get(&request_id)? {
        if existing.recipient != recipient.canonical {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "request_id was already used with a different recipient",
                false,
            ));
        }
        return Ok(claim_response(existing, st.cfg.prize));
    }

    if st.paused() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "faucet_paused",
            "the faucet is paused",
            true,
        ));
    }

    let forwarded = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
    let peer_ip = peer.map(|ConnectInfo(a)| a.ip().to_string());
    let ip = policy::client_ip(forwarded, peer_ip.as_deref());
    let ip_hash = policy::ip_hash(&st.cfg.ip_salt, &ip);

    let now = Utc::now();
    check_limits(&st, &recipient.canonical, &ip_hash, now)?;

    let hot_balance = st.hot_balance().await.map_err(sequencer_unreachable)?;
    let reserve = st.reserve().await;
    let pending = u128::from(st.db.count_pending()?);
    let needed = st
        .cfg
        .prize
        .saturating_mul(pending + 1)
        .saturating_add(reserve);
    if hot_balance < needed {
        warn!(hot_balance, needed, "refusing claim: hot balance low");
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "hot_balance_low",
            "the faucet's hot account cannot cover this claim right now",
            true,
        ));
    }

    let inserted = match st
        .db
        .insert_queued(&request_id, &recipient.canonical, &ip_hash, now)
    {
        Ok(c) => c,
        Err(_) => {
            // Lost a race with an identical request: answer with the winner's row.
            match st.db.get(&request_id)? {
                Some(c) if c.recipient == recipient.canonical => {
                    return Ok(claim_response(c, st.cfg.prize))
                }
                Some(_) => {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "idempotency_conflict",
                        "request_id was already used with a different recipient",
                        false,
                    ))
                }
                None => return Err(ApiError::internal("insert failed")),
            }
        }
    };
    info!(request_id = %request_id, recipient = %recipient.canonical, "claim queued");
    st.notify.notify_one();
    Ok(claim_response(inserted, st.cfg.prize))
}

fn rate_limited(
    st: &AppState,
    scope: Scope<'_>,
    window: chrono::Duration,
    now: DateTime<Utc>,
    which: &str,
) -> Result<ApiError, ApiError> {
    let oldest = st.db.oldest_since(scope, now - window)?.unwrap_or(now);
    let retry = policy::retry_after_seconds(oldest, window, now);
    let mut e = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        format!("{which} limit reached; retry in {retry}s"),
        true,
    );
    e.details = Some(json!({ "retry_after_seconds": retry, "limit": which }));
    Ok(e)
}

fn check_limits(
    st: &AppState,
    recipient: &str,
    ip_hash: &str,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let l = st.cfg.limits;
    if st
        .db
        .count_since(Scope::Recipient(recipient), day_ago(now))?
        >= l.per_recipient_per_day
    {
        return Err(rate_limited(
            st,
            Scope::Recipient(recipient),
            DAY,
            now,
            "per_recipient_per_day",
        )?);
    }
    if st.db.count_since(Scope::Ip(ip_hash), day_ago(now))? >= l.per_ip_per_day {
        return Err(rate_limited(
            st,
            Scope::Ip(ip_hash),
            DAY,
            now,
            "per_ip_per_day",
        )?);
    }
    if st.db.count_since(Scope::Global, hour_ago(now))? >= l.per_hour {
        return Err(rate_limited(st, Scope::Global, HOUR, now, "per_hour")?);
    }
    if st.db.count_since(Scope::Global, day_ago(now))? >= l.per_day {
        return Err(rate_limited(st, Scope::Global, DAY, now, "per_day")?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request},
    };
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use super::*;
    use crate::chain::mock::MockChain;

    const R1: &str = "Public/4sAkk9FjWpTirvhXLcE3iXsT2K3J8XfUdd31X5v4y8s9";
    const R2: &str = "Public/5hYsVGp2Qf7MACJ5kw7EQopB3BRc8jthcgZBcwcKJSYT";
    const R3: &str = "Public/ChutHUnQd7uwbA96AWe4KSS77Fq5fYJDm3WzXQ3E6tTx";
    const R4: &str = "Public/3f7M3si6qKcBS6eR8z3q1WgqMtH1YQHVqQ2jGTD7vW7e";
    const ID1: &str = "6f1e2c3a-9b8d-4e5f-a1b2-c3d4e5f60718";
    const ID2: &str = "0b7f8a9c-1d2e-4f30-8a1b-2c3d4e5f6071";
    const ID3: &str = "9e8d7c6b-5a4f-4e3d-9c2b-1a0f9e8d7c6b";
    const ID4: &str = "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";
    const PRIZE: u128 = 100_000_000;

    struct Harness {
        app: Router,
        db: Arc<Db>,
        chain: Arc<MockChain>,
        state: Arc<AppState>,
        pause: PathBuf,
    }

    fn harness(hot_balance: u128) -> Harness {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let chain = Arc::new(MockChain::new(hot_balance));
        let pause =
            std::env::temp_dir().join(format!("claimd-test-pause-{}", uuid::Uuid::new_v4()));
        let cfg = AppConfig {
            sequencer: "https://testnet.lez.logos.co".into(),
            prize: PRIZE,
            limits: Limits::default(),
            pause_file: pause.clone(),
            ip_salt: b"test-salt".to_vec(),
        };
        let st = AppState::new(db.clone(), chain.clone(), cfg, Arc::new(Notify::new()));
        Harness {
            app: router(st.clone()),
            db,
            chain,
            state: st,
            pause,
        }
    }

    async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, v)
    }

    fn post(recipient: &str, id: &str, ip: &str) -> Request<Body> {
        Request::post("/v1/claim")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                json!({ "recipient": recipient, "request_id": id }).to_string(),
            ))
            .unwrap()
    }

    fn get(path: &str) -> Request<Body> {
        Request::get(path).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn info_reports_balance_limits_and_remaining() {
        let h = harness(1_000_000_000);
        let (status, v) = call(&h.app, get("/v1/info")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["network"], "lez-v03-testnet");
        assert_eq!(v["prize"], "100000000");
        assert_eq!(v["decimals"], 9);
        assert_eq!(
            v["hot_account"],
            format!("Public/{}", crate::chain::mock::HOT)
        );
        assert_eq!(v["hot_balance"], "1000000000");
        assert_eq!(v["paused"], false);
        assert_eq!(v["limits"]["per_day"], 10);
        // (1_000_000_000 - 18_900_000) / 100_000_000 = 9 < 10 global remaining
        assert_eq!(v["claims_remaining"], 9);
    }

    #[tokio::test]
    async fn claim_is_queued_replayed_and_conflicts_on_other_recipient() {
        let h = harness(1_000_000_000);
        let (status, v) = call(&h.app, post(R1, ID1, "203.0.113.1")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        assert_eq!(v["status"], "queued");
        assert_eq!(v["request_id"], ID1);
        assert_eq!(v["recipient"], R1);
        assert_eq!(v["tx_hash"], Value::Null);
        assert_eq!(v["error"], Value::Null);
        assert_eq!(v["prize"], "100000000");

        // Replay: same row, no second queue entry, even from another IP.
        let (status, v2) = call(&h.app, post(R1, ID1, "203.0.113.2")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(v2, v);
        assert_eq!(h.db.count_pending().unwrap(), 1);

        let (status, v) = call(&h.app, post(R2, ID1, "203.0.113.1")).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["error"]["code"], "idempotency_conflict");

        let (status, g) = call(&h.app, get(&format!("/v1/claim/{ID1}"))).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(g, v2);
        let (status, v) = call(&h.app, get(&format!("/v1/claim/{ID2}"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["code"], "unknown_job");
    }

    #[tokio::test]
    async fn terminal_claims_answer_200_with_error_body_when_failed() {
        let h = harness(1_000_000_000);
        call(&h.app, post(R1, ID1, "203.0.113.1")).await;
        let now = Utc::now();
        h.db.set_balance_before(ID1, 0, now).unwrap();
        h.db.mark_submitted(ID1, "ab".repeat(32).as_str(), now)
            .unwrap();
        h.db.mark_included(ID1, PRIZE, now).unwrap();
        let (status, v) = call(&h.app, get(&format!("/v1/claim/{ID1}"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["status"], "included");
        assert_eq!(v["balance_before"], "0");
        assert_eq!(v["balance_after"], "100000000");
        assert_eq!(v["tx_hash"], "ab".repeat(32));

        call(&h.app, post(R2, ID2, "203.0.113.1")).await;
        h.db.mark_failed(ID2, "submission_rejected", "nope", None, now)
            .unwrap();
        let (status, v) = call(&h.app, post(R2, ID2, "203.0.113.1")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["status"], "failed");
        assert_eq!(v["error"]["code"], "submission_rejected");
        assert_eq!(v["error"]["message"], "nope");
    }

    #[tokio::test]
    async fn validation_errors() {
        let h = harness(1_000_000_000);
        let (status, v) = call(
            &h.app,
            post(
                "Private/4sAkk9FjWpTirvhXLcE3iXsT2K3J8XfUdd31X5v4y8s9",
                ID1,
                "1.1.1.1",
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["code"], "invalid_public_account_id");
        let (status, v) = call(&h.app, post(R1, "NOT-A-UUID", "1.1.1.1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["code"], "invalid_request_key");
        let (status, v) = call(&h.app, post(R1, &ID1.to_uppercase(), "1.1.1.1")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["code"], "invalid_request_key");
        let bad = Request::post("/v1/claim")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        let (status, v) = call(&h.app, bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["code"], "invalid_request");
        assert!(
            h.db.get(ID1).unwrap().is_none(),
            "rejections record nothing"
        );
    }

    #[tokio::test]
    async fn per_recipient_and_per_ip_limits() {
        let h = harness(10_000_000_000);
        let (status, _) = call(&h.app, post(R1, ID1, "203.0.113.1")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, v) = call(&h.app, post(R1, ID2, "203.0.113.9")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(v["error"]["code"], "rate_limited");
        assert_eq!(v["error"]["retryable"], true);
        let retry = v["error"]["details"]["retry_after_seconds"]
            .as_u64()
            .unwrap();
        assert!(retry > 86_000 && retry <= 86_400, "{retry}");
        assert!(h.db.get(ID2).unwrap().is_none());

        // Same IP, second distinct recipient OK; third hits per_ip_per_day.
        let (status, _) = call(&h.app, post(R2, ID2, "203.0.113.1, 10.0.0.1")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let (status, v) = call(&h.app, post(R3, ID3, "203.0.113.1")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(v["error"]["details"]["limit"], "per_ip_per_day");
    }

    #[tokio::test]
    async fn global_hourly_limit() {
        let h = harness(10_000_000_000);
        for (r, id, ip) in [
            (R1, ID1, "1.1.1.1"),
            (R2, ID2, "2.2.2.2"),
            (R3, ID3, "3.3.3.3"),
        ] {
            let (status, v) = call(&h.app, post(r, id, ip)).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        }
        let (status, v) = call(&h.app, post(R4, ID4, "4.4.4.4")).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(v["error"]["details"]["limit"], "per_hour");
        assert!(
            v["error"]["details"]["retry_after_seconds"]
                .as_u64()
                .unwrap()
                <= 3600
        );
        // A replay of an accepted claim still answers while limited.
        let (status, _) = call(&h.app, post(R1, ID1, "1.1.1.1")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn pause_file_and_low_hot_balance_refuse_new_claims_only() {
        let h = harness(1_000_000_000);
        call(&h.app, post(R1, ID1, "1.1.1.1")).await;
        std::fs::write(&h.pause, b"").unwrap();
        let (status, v) = call(&h.app, post(R2, ID2, "2.2.2.2")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["error"]["code"], "faucet_paused");
        let (status, _) = call(&h.app, post(R1, ID1, "1.1.1.1")).await;
        assert_eq!(status, StatusCode::ACCEPTED, "replay works while paused");
        let (_, info) = call(&h.app, get("/v1/info")).await;
        assert_eq!(info["paused"], true);
        std::fs::remove_file(&h.pause).unwrap();

        // Hot balance covers the already-queued claim but not a second one.
        h.chain.set_balance(crate::chain::mock::HOT, 2 * PRIZE);
        h.state.clear_caches();
        let (status, v) = call(&h.app, post(R2, ID2, "2.2.2.2")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{v}");
        assert_eq!(v["error"]["code"], "hot_balance_low");
        assert!(h.db.get(ID2).unwrap().is_none());
    }

    #[tokio::test]
    async fn healthz_reflects_sequencer_and_floor() {
        let h = harness(1_000_000_000);
        let (status, v) = call(&h.app, get("/healthz")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ok"], true);
        assert_eq!(v["sequencer"], "ok");
        assert_eq!(v["hot_balance"], "1000000000");

        let h = harness(PRIZE); // below prize + reserve
        let (status, v) = call(&h.app, get("/healthz")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["ok"], false);

        let h = harness(1_000_000_000);
        h.chain
            .with(|s| s.health = Err(ChainError::Rpc("down".into())));
        let (status, v) = call(&h.app, get("/healthz")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["ok"], false);
        assert!(v["sequencer"].as_str().unwrap().starts_with("error"));
    }
}
