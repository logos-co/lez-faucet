//! Structured error taxonomy shared by every layer.
//!
//! Callers dispatch on [`ErrorCode`], never on message text. The C++ module and
//! the QML view both depend on that: a message is for a human, a code is for a
//! program.

use sequencer_service_rpc::ClientError;
use serde::Serialize;

/// Stable machine-readable error codes.
///
/// The wire representation is the snake_case name. Adding a variant is a
/// compatible change; renaming one is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotConfigured,
    InvalidSequencerUrl,
    InvalidPublicAccountId,
    InvalidRequestKey,
    RecipientUninitialized,
    RecipientWrongOwner,
    /// A previous claim to this recipient ended with an unproven outcome, so
    /// this session refuses to send another one to the same account.
    RecipientUnreconciled,
    ProgramFingerprintMismatch,
    PinataWrongOwner,
    InvalidChallenge,
    UnsupportedDifficulty,
    PoolDepleted,
    RecipientBalanceOverflow,
    NetworkUnavailable,
    RpcTimeout,
    /// The claim service refused this request under one of its caps.
    /// `details.retry_after_seconds` says when it will accept another.
    RateLimited,
    /// The claim service's operator has paused payouts.
    FaucetPaused,
    /// The claim service could not be reached or answered with a server
    /// failure. Nothing was recorded there, so a repeat is safe.
    ClaimApiUnavailable,
    SolveTimeout,
    SolveAttemptLimit,
    StaleChallengeExhausted,
    SubmissionRejected,
    OutcomeUnknown,
    Cancelled,
    DropInProgress,
    IdempotencyConflict,
    JobLimitReached,
    UnknownJob,
    JobNotTerminal,
    ModuleStopping,
    InternalError,
}

impl ErrorCode {
    /// Whether repeating the *same* request could plausibly succeed later.
    ///
    /// This is advice for the presentation layer only. It never authorizes the
    /// client to resubmit a transaction on its own: an ambiguous or already
    /// submitted claim is never retryable, and the drop state machine refuses
    /// to cross those boundaries regardless of this flag.
    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(
            self,
            Self::NetworkUnavailable
                | Self::RpcTimeout
                | Self::SolveTimeout
                | Self::ClaimApiUnavailable
        )
    }
}

/// The phase a drop had reached when something went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DropPhase {
    ValidatingInput,
    VerifyingPrograms,
    InspectingRecipient,
    FetchingChallenge,
    Solving,
    RefreshingChallenge,
    Submitting,
    Reconciling,
}

/// A safe, user-presentable error.
///
/// `message` is written for a person and must never contain a raw request or
/// response body. There is no secret to redact here — this API accepts none —
/// but an address is still the user's data and raw JSON is still noise.
#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<DropPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            phase: None,
            details: None,
        }
    }

    #[must_use]
    pub const fn at(mut self, phase: DropPhase) -> Self {
        self.phase = Some(phase);
        self
    }

    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    /// Map a failed read-only RPC call onto the taxonomy.
    ///
    /// A `Call` variant means the sequencer answered and refused, which is not
    /// a connectivity problem and must not be presented as one. Everything
    /// else is transport.
    pub fn from_rpc(context: &str, error: &ClientError) -> Self {
        match error {
            ClientError::RequestTimeout => Self::new(
                ErrorCode::RpcTimeout,
                format!("{context}: the LEZ testnet sequencer did not respond in time."),
            ),
            ClientError::Call(_) => Self::new(
                ErrorCode::InternalError,
                format!("{context}: the LEZ testnet sequencer rejected the request."),
            ),
            _ => Self::new(
                ErrorCode::NetworkUnavailable,
                format!("{context}: the LEZ testnet sequencer could not be reached."),
            ),
        }
    }

    /// Map a failed `sendTransaction` onto the taxonomy.
    ///
    /// Kept separate from [`Self::from_rpc`] on purpose. A refusal here is the
    /// sequencer declining the transaction outright, which is terminal and not
    /// retryable, whereas a transport failure leaves the outcome genuinely
    /// unknown — the transaction may well have been accepted before the
    /// response was lost. The two must never collapse into one code.
    pub fn from_submission(error: &ClientError) -> Self {
        match error {
            ClientError::Call(_) => Self::new(
                ErrorCode::SubmissionRejected,
                "The LEZ testnet sequencer rejected the claim transaction.",
            ),
            ClientError::RequestTimeout => Self::new(
                ErrorCode::RpcTimeout,
                "The claim transaction was sent but the sequencer did not respond in time.",
            ),
            _ => Self::new(
                ErrorCode::NetworkUnavailable,
                "The claim transaction could not be delivered to the LEZ testnet sequencer.",
            ),
        }
    }

    /// Map a failed raw HTTP call to the sequencer onto the taxonomy.
    ///
    /// The v0.3 path speaks JSON-RPC over `reqwest` directly, so transport
    /// failures arrive as `reqwest` errors rather than `ClientError`s. The
    /// same distinction holds: a timeout, a connection failure and a body
    /// the sequencer answered with are three different things.
    pub fn from_http(context: &str, error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::new(
                ErrorCode::RpcTimeout,
                format!("{context}: the LEZ testnet sequencer did not respond in time."),
            )
        } else if error.is_decode() || error.is_status() {
            Self::new(
                ErrorCode::InternalError,
                format!("{context}: the LEZ testnet sequencer gave an unexpected answer."),
            )
        } else {
            Self::new(
                ErrorCode::NetworkUnavailable,
                format!("{context}: the LEZ testnet sequencer could not be reached."),
            )
        }
    }

    pub fn internal(context: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, context)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_as_stable_snake_case() {
        let rendered = |code: ErrorCode| serde_json::to_value(code).unwrap();
        assert_eq!(rendered(ErrorCode::OutcomeUnknown), "outcome_unknown");
        assert_eq!(rendered(ErrorCode::PoolDepleted), "pool_depleted");
        assert_eq!(rendered(ErrorCode::RateLimited), "rate_limited");
        assert_eq!(rendered(ErrorCode::FaucetPaused), "faucet_paused");
        assert_eq!(
            rendered(ErrorCode::ClaimApiUnavailable),
            "claim_api_unavailable"
        );
        assert_eq!(
            rendered(ErrorCode::InvalidPublicAccountId),
            "invalid_public_account_id"
        );
    }

    #[test]
    fn phases_serialize_as_stable_snake_case() {
        assert_eq!(
            serde_json::to_value(DropPhase::RefreshingChallenge).unwrap(),
            "refreshing_challenge"
        );
    }

    #[test]
    fn no_terminal_or_ambiguous_outcome_is_marked_retryable() {
        // A retryable flag on any of these would invite a bridge or a view to
        // resubmit a claim that may already have credited the recipient.
        for code in [
            ErrorCode::OutcomeUnknown,
            ErrorCode::SubmissionRejected,
            ErrorCode::PoolDepleted,
            ErrorCode::ProgramFingerprintMismatch,
            ErrorCode::UnsupportedDifficulty,
            ErrorCode::RecipientUninitialized,
            ErrorCode::RecipientWrongOwner,
            ErrorCode::IdempotencyConflict,
            ErrorCode::DropInProgress,
            ErrorCode::Cancelled,
            // The service said no under a cap or a pause; repeating the same
            // request immediately is exactly what it asked us not to do.
            ErrorCode::RateLimited,
            ErrorCode::FaucetPaused,
        ] {
            assert!(!code.retryable(), "{code:?} must not be retryable");
        }
    }

    #[test]
    fn an_unreachable_claim_service_is_retryable_because_nothing_was_recorded() {
        assert!(ErrorCode::ClaimApiUnavailable.retryable());
    }

    #[test]
    fn omitted_fields_stay_absent_from_the_wire() {
        let json =
            serde_json::to_value(ApiError::new(ErrorCode::UnknownJob, "no such job")).unwrap();
        assert!(json.get("phase").is_none());
        assert!(json.get("details").is_none());
        assert_eq!(json["code"], "unknown_job");
        assert_eq!(json["retryable"], false);
    }
}
