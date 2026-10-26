//! Transport for the LEZ v0.3 testnet: raw JSON-RPC and the hosted claim
//! service.
//!
//! This crate links LEZ v0.2.4, whose typed `SequencerClient` cannot parse a
//! v0.3 `getAccount` (the account body moved into `data.shards`). The three
//! reads the v0.3 path needs — a balance, whether a transaction is on chain,
//! and the deployed program IDs — have simple wire shapes, so they are spoken
//! here directly over `reqwest` with no v0.3 crate in the build.
//!
//! The claim service's contract is `claimd/README.md`. Everything this module
//! returns is parsed and re-typed; no body from either host is ever echoed
//! into a message a person reads.

use std::{collections::HashMap, time::Duration};

use serde::{de::DeserializeOwned, Deserialize};

use crate::error::{ApiError, ApiResult, ErrorCode};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest response body either host is allowed to hand us. The answers this
/// module reads are a few hundred bytes; a multi-megabyte one is not an answer.
const MAX_BODY_BYTES: usize = 64 * 1024;

fn http_client() -> ApiResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("lez-faucet/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| ApiError::internal("could not build the HTTP client"))
}

/// Accept `https` anywhere and `http` on loopback only, like the sequencer
/// rule in `FaucetClient::new`.
fn parse_pinned_url(raw: &str, what: &str) -> ApiResult<url::Url> {
    let parsed = url::Url::parse(raw).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidSequencerUrl,
            format!("The {what} URL is not a valid URL."),
        )
    })?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
        return Err(ApiError::new(
            ErrorCode::InvalidSequencerUrl,
            format!("The {what} URL must use https."),
        ));
    }
    Ok(parsed)
}

async fn bounded_body(response: reqwest::Response) -> Result<Vec<u8>, reqwest::Error> {
    let body = response.bytes().await?;
    Ok(body[..body.len().min(MAX_BODY_BYTES)].to_vec())
}

// ---- JSON-RPC --------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    #[allow(dead_code)]
    code: i64,
    #[allow(dead_code)]
    message: String,
}

/// The three JSON-RPC reads the v0.3 claim path depends on.
#[derive(Debug, Clone)]
pub struct SequencerV03 {
    http: reqwest::Client,
    url: url::Url,
}

impl SequencerV03 {
    pub fn new(url: url::Url) -> ApiResult<Self> {
        Ok(Self {
            http: http_client()?,
            url,
        })
    }

    /// One JSON-RPC round trip. `None` is a `null` result, which some
    /// methods use as a normal answer.
    async fn call_optional<T: DeserializeOwned>(
        &self,
        context: &str,
        method: &str,
        params: serde_json::Value,
    ) -> ApiResult<Option<T>> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response = self
            .http
            .post(self.url.clone())
            .json(&request)
            .send()
            .await
            .map_err(|error| ApiError::from_http(context, &error))?;
        let body = bounded_body(response)
            .await
            .map_err(|error| ApiError::from_http(context, &error))?;
        let parsed: RpcResponse<T> = serde_json::from_slice(&body).map_err(|_| {
            ApiError::internal(format!(
                "{context}: the LEZ testnet sequencer gave an unexpected answer."
            ))
        })?;
        if parsed.error.is_some() {
            // The sequencer answered and refused. Not a connectivity problem,
            // and never presented as one.
            return Err(ApiError::internal(format!(
                "{context}: the LEZ testnet sequencer rejected the request."
            )));
        }
        Ok(parsed.result)
    }

    /// One JSON-RPC round trip whose result must be present.
    async fn call<T: DeserializeOwned>(
        &self,
        context: &str,
        method: &str,
        params: serde_json::Value,
    ) -> ApiResult<T> {
        self.call_optional(context, method, params)
            .await?
            .ok_or_else(|| {
                ApiError::internal(format!(
                    "{context}: the LEZ testnet sequencer gave an unexpected answer."
                ))
            })
    }

    /// `getAccountBalance`: the native balance of a public account in the
    /// smallest unit. An account that does not exist yet reads as 0, which is
    /// a real balance on v0.3 — there is no initialization step there.
    pub async fn get_account_balance(&self, account_id: &str) -> ApiResult<u128> {
        self.call(
            "Reading the recipient balance",
            "getAccountBalance",
            serde_json::json!([account_id]),
        )
        .await
    }

    /// `getTransaction`: whether a transaction with this hash is on chain.
    /// The body is not inspected beyond presence; what proves the credit is
    /// the balance, read separately.
    pub async fn is_transaction_included(&self, tx_hash: &str) -> ApiResult<bool> {
        let found: Option<serde_json::Value> = self
            .call_optional(
                "Looking up the claim transaction",
                "getTransaction",
                serde_json::json!([tx_hash]),
            )
            .await?;
        Ok(found.is_some_and(|value| !value.is_null()))
    }

    /// `getProgramIds`: every builtin program the sequencer runs, as RISC
    /// Zero ImageIDs (eight little-endian words).
    pub async fn get_program_ids(&self) -> ApiResult<HashMap<String, [u32; 8]>> {
        self.call(
            "Reading the deployed program IDs",
            "getProgramIds",
            serde_json::json!([]),
        )
        .await
    }
}

// ---- the claim service -----------------------------------------------------

/// `GET /v1/info`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ClaimInfo {
    pub network: String,
    /// Decimal string of the smallest unit.
    pub prize: String,
    pub decimals: u8,
    #[serde(default)]
    pub hot_account: Option<String>,
    /// Decimal string of the smallest unit.
    pub hot_balance: String,
    pub claims_remaining: u64,
    pub paused: bool,
}

/// Lifecycle of one claim at the service.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimState {
    Queued,
    Submitted,
    Included,
    Failed,
}

/// Why the service gave up on a claim.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ClaimFailure {
    pub code: String,
    #[allow(dead_code)]
    pub message: String,
}

/// `POST /v1/claim` and `GET /v1/claim/{id}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ClaimRecord {
    pub request_id: String,
    pub recipient: String,
    pub status: ClaimState,
    #[serde(default)]
    pub tx_hash: Option<String>,
    #[serde(default)]
    pub error: Option<ClaimFailure>,
}

#[derive(Debug, Deserialize)]
struct ServiceErrorBody {
    error: ServiceError,
}

#[derive(Debug, Deserialize)]
struct ServiceError {
    code: String,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

/// Translate a service refusal into the app's taxonomy.
///
/// Only the code decides. The service's sentence is written for its operator
/// and is not reproduced; every message here is this app's own.
fn map_service_error(status: reqwest::StatusCode, error: Option<ServiceError>) -> ApiError {
    let Some(error) = error else {
        return if status.is_server_error() {
            ApiError::new(
                ErrorCode::ClaimApiUnavailable,
                "The faucet service is not available right now. Nothing was sent.",
            )
        } else {
            ApiError::internal("The faucet service gave an unexpected answer.")
        };
    };
    match error.code.as_str() {
        "rate_limited" => {
            let retry_after = error
                .details
                .as_ref()
                .and_then(|details| details.get("retry_after_seconds"))
                .and_then(serde_json::Value::as_u64);
            let message = match retry_after {
                Some(seconds) if seconds >= 3600 => format!(
                    "The faucet service is limiting claims right now. Try again in about {} hours.",
                    seconds.div_ceil(3600)
                ),
                Some(seconds) if seconds >= 60 => format!(
                    "The faucet service is limiting claims right now. Try again in about {} minutes.",
                    seconds.div_ceil(60)
                ),
                Some(seconds) => format!(
                    "The faucet service is limiting claims right now. Try again in about {seconds} seconds."
                ),
                None => "The faucet service is limiting claims right now. Try again later.".to_owned(),
            };
            let mut mapped = ApiError::new(ErrorCode::RateLimited, message);
            if let Some(seconds) = retry_after {
                mapped = mapped.with_details(serde_json::json!({ "retry_after_seconds": seconds }));
            }
            mapped
        }
        "faucet_paused" => ApiError::new(
            ErrorCode::FaucetPaused,
            "The faucet service is paused by its operator. Nothing was sent.",
        ),
        "hot_balance_low" => ApiError::new(
            ErrorCode::PoolDepleted,
            "The faucet service's hot balance is too low to pay a claim. Nothing was sent.",
        ),
        "invalid_public_account_id" => ApiError::new(
            ErrorCode::InvalidPublicAccountId,
            "The faucet service did not accept that account ID.",
        ),
        "invalid_request_key" => ApiError::new(
            ErrorCode::InvalidRequestKey,
            "The faucet service did not accept the request key.",
        ),
        "idempotency_conflict" => ApiError::new(
            ErrorCode::IdempotencyConflict,
            "That request key was already used for a different address.",
        ),
        "unknown_job" => ApiError::new(
            ErrorCode::UnknownJob,
            "The faucet service has no record of this request.",
        ),
        _ if status.is_server_error() => ApiError::new(
            ErrorCode::ClaimApiUnavailable,
            "The faucet service is not available right now. Nothing was sent.",
        ),
        _ => ApiError::internal("The faucet service refused the request."),
    }
}

/// Client for the hosted claim service.
#[derive(Debug, Clone)]
pub struct ClaimApi {
    http: reqwest::Client,
    base: url::Url,
}

impl ClaimApi {
    pub fn new(base_url: &str) -> ApiResult<Self> {
        let mut base = parse_pinned_url(base_url, "claim service")?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Self {
            http: http_client()?,
            base,
        })
    }

    fn endpoint(&self, path: &str) -> ApiResult<url::Url> {
        self.base
            .join(path)
            .map_err(|_| ApiError::internal("could not build a claim service URL"))
    }

    async fn send<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> ApiResult<T> {
        let response = request.send().await.map_err(|error| {
            // A lost response to a POST is handled by the caller, which asks
            // the service what it recorded before concluding anything.
            ApiError::new(
                ErrorCode::ClaimApiUnavailable,
                if error.is_timeout() {
                    "The faucet service did not respond in time."
                } else {
                    "The faucet service could not be reached."
                },
            )
        })?;
        let status = response.status();
        let body = bounded_body(response).await.map_err(|_| {
            ApiError::new(
                ErrorCode::ClaimApiUnavailable,
                "The faucet service's answer was cut short.",
            )
        })?;
        if status.is_success() {
            return serde_json::from_slice(&body)
                .map_err(|_| ApiError::internal("The faucet service gave an unexpected answer."));
        }
        let parsed = serde_json::from_slice::<ServiceErrorBody>(&body)
            .ok()
            .map(|body| body.error);
        Err(map_service_error(status, parsed))
    }

    pub async fn info(&self) -> ApiResult<ClaimInfo> {
        self.send(self.http.get(self.endpoint("v1/info")?)).await
    }

    /// Ask for one drop. `request_id` is the idempotency key: the same id with
    /// the same recipient returns the same claim and never pays twice.
    pub async fn claim(&self, recipient: &str, request_id: &str) -> ApiResult<ClaimRecord> {
        let body = serde_json::json!({
            "recipient": recipient,
            "request_id": request_id,
        });
        self.send(self.http.post(self.endpoint("v1/claim")?).json(&body))
            .await
    }

    pub async fn status(&self, request_id: &str) -> ApiResult<ClaimRecord> {
        self.send(
            self.http
                .get(self.endpoint(&format!("v1/claim/{request_id}"))?),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_claim_service_url_is_pinned_to_https_outside_loopback() {
        assert!(ClaimApi::new("https://rog-devbox.tail957e2.ts.net:8443").is_ok());
        assert!(ClaimApi::new("http://127.0.0.1:8787").is_ok());
        assert_eq!(
            ClaimApi::new("http://example.com").unwrap_err().code,
            ErrorCode::InvalidSequencerUrl
        );
        assert_eq!(
            ClaimApi::new("not a url").unwrap_err().code,
            ErrorCode::InvalidSequencerUrl
        );
    }

    #[test]
    fn endpoints_join_onto_the_root_with_or_without_a_trailing_slash() {
        for root in [
            "https://rog-devbox.tail957e2.ts.net:8443",
            "https://rog-devbox.tail957e2.ts.net:8443/",
        ] {
            let api = ClaimApi::new(root).unwrap();
            assert_eq!(
                api.endpoint("v1/claim/abc").unwrap().as_str(),
                "https://rog-devbox.tail957e2.ts.net:8443/v1/claim/abc"
            );
        }
    }

    #[test]
    fn service_refusals_map_onto_codes_and_never_echo_the_service_text() {
        let mapped = |code: &str, status: u16, details: Option<serde_json::Value>| {
            map_service_error(
                reqwest::StatusCode::from_u16(status).unwrap(),
                Some(ServiceError {
                    code: code.to_owned(),
                    details,
                }),
            )
        };
        let limited = mapped(
            "rate_limited",
            429,
            Some(serde_json::json!({ "retry_after_seconds": 5400 })),
        );
        assert_eq!(limited.code, ErrorCode::RateLimited);
        assert!(!limited.retryable);
        assert_eq!(limited.details.unwrap()["retry_after_seconds"], 5400);
        assert!(limited.message.contains("2 hours"));

        assert_eq!(
            mapped("faucet_paused", 503, None).code,
            ErrorCode::FaucetPaused
        );
        assert_eq!(
            mapped("hot_balance_low", 503, None).code,
            ErrorCode::PoolDepleted
        );
        assert_eq!(
            mapped("idempotency_conflict", 409, None).code,
            ErrorCode::IdempotencyConflict
        );
        assert_eq!(mapped("unknown_job", 404, None).code, ErrorCode::UnknownJob);
        // An unrecognised code from a failing server is "unavailable", which
        // is retryable because the service records nothing it does not
        // acknowledge; the same from a 4xx is not.
        assert_eq!(
            mapped("something_new", 500, None).code,
            ErrorCode::ClaimApiUnavailable
        );
        assert_eq!(
            mapped("something_new", 400, None).code,
            ErrorCode::InternalError
        );
        // No body at all.
        assert_eq!(
            map_service_error(reqwest::StatusCode::BAD_GATEWAY, None).code,
            ErrorCode::ClaimApiUnavailable
        );
    }

    #[test]
    fn claim_records_parse_with_optional_fields_absent_or_null() {
        let queued: ClaimRecord = serde_json::from_str(
            r#"{"request_id":"a","recipient":"Public/b","status":"queued","tx_hash":null,"error":null}"#,
        )
        .unwrap();
        assert_eq!(queued.status, ClaimState::Queued);
        assert_eq!(queued.tx_hash, None);

        let failed: ClaimRecord = serde_json::from_str(
            r#"{"request_id":"a","recipient":"Public/b","status":"failed","error":{"code":"outcome_unknown","message":"x"}}"#,
        )
        .unwrap();
        assert_eq!(failed.status, ClaimState::Failed);
        assert_eq!(failed.error.unwrap().code, "outcome_unknown");
    }

    #[test]
    fn balances_survive_as_exact_u128() {
        let parsed: RpcResponse<u128> = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"result":340282366920938463463374607431768211455}"#,
        )
        .unwrap();
        assert_eq!(parsed.result, Some(u128::MAX));
        let parsed: RpcResponse<u128> =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":9007199254740993}"#).unwrap();
        assert_eq!(parsed.result, Some((1_u128 << 53) + 1));
    }
}
