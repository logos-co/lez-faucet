//! Input validation, client identification and the limits table. Pure functions; no I/O.

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest as _, Sha256};

use crate::chain::FeeQuote;

/// Serialized-size allowance the wallet sizes its `max_fee` against (`wallet::ASSUMED_DATA_BYTES`);
/// a native transfer is far smaller, so this over-reserves on the safe side.
pub const ASSUMED_DATA_BYTES: u64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub per_recipient_per_day: u32,
    pub per_ip_per_day: u32,
    pub per_hour: u32,
    pub per_day: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_recipient_per_day: 1,
            per_ip_per_day: 2,
            per_hour: 3,
            per_day: 10,
        }
    }
}

pub const DAY: Duration = Duration::hours(24);
pub const HOUR: Duration = Duration::hours(1);

/// A validated public account id. `canonical` is `Public/<base58>` with the base58 re-encoded
/// from the decoded bytes, so two spellings of the same key collapse to one ledger key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub canonical: String,
    pub base58: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    InvalidPublicAccountId(&'static str),
    InvalidRequestKey(&'static str),
}

pub fn validate_recipient(input: &str) -> Result<Recipient, ValidationError> {
    use ValidationError::InvalidPublicAccountId as E;
    if input.starts_with("Private/") {
        return Err(E("private account ids cannot receive a public transfer"));
    }
    let Some(b58) = input.strip_prefix("Public/") else {
        return Err(E("expected `Public/<base58 account id>`"));
    };
    let bytes = bs58::decode(b58)
        .into_vec()
        .map_err(|_| E("account id is not valid base58"))?;
    if bytes.len() != 32 {
        return Err(E("account id must decode to exactly 32 bytes"));
    }
    let base58 = bs58::encode(&bytes).into_string();
    Ok(Recipient {
        canonical: format!("Public/{base58}"),
        base58,
    })
}

/// `request_id` must be a lowercase hyphenated UUID; it is echoed back verbatim.
pub fn validate_request_id(input: &str) -> Result<String, ValidationError> {
    use ValidationError::InvalidRequestKey as E;
    let parsed = uuid::Uuid::parse_str(input).map_err(|_| E("request_id must be a uuid"))?;
    if parsed.hyphenated().to_string() != input {
        return Err(E(
            "request_id must be the lowercase hyphenated form of the uuid",
        ));
    }
    if parsed.is_nil() {
        return Err(E("request_id must not be the nil uuid"));
    }
    Ok(input.to_owned())
}

/// Client IP: first hop of `X-Forwarded-For` when present (Funnel sets it), else the peer.
pub fn client_ip(forwarded_for: Option<&str>, peer: Option<&str>) -> String {
    forwarded_for
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| peer.map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Raw IPs are never stored; the ledger keeps `sha256(salt || ip)`.
pub fn ip_hash(salt: &[u8], ip: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(b"\0");
    h.update(ip.as_bytes());
    hex::encode(h.finalize())
}

/// `gas_limit·base_fee_exec + gas_stor·base_fee_stor` (`lez/programs/fee/core/src/assess.rs`),
/// priced at the next-block ceilings so a fee bump between quote and inclusion still fits.
pub fn fee_reserve(gas_limit: u64, quote: &FeeQuote) -> u128 {
    let exec = quote.next_base_fee_exec_ceiling.max(quote.base_fee_exec);
    let stor = quote.next_base_fee_stor_ceiling.max(quote.base_fee_stor);
    u128::from(gas_limit) * u128::from(exec) + u128::from(ASSUMED_DATA_BYTES) * u128::from(stor)
}

/// Seconds until the oldest claim inside `window` ages out; at least 1.
pub fn retry_after_seconds(oldest: DateTime<Utc>, window: Duration, now: DateTime<Utc>) -> u64 {
    let until = oldest + window - now;
    let secs = until.num_seconds();
    if secs <= 0 {
        1
    } else if until.subsec_nanos() > 0 {
        secs as u64 + 1
    } else {
        secs as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "Public/5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z";

    #[test]
    fn accepts_a_public_32_byte_base58_id() {
        let r = validate_recipient(GOOD).unwrap();
        assert_eq!(r.canonical, GOOD);
        assert_eq!(r.base58, "5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z");
    }

    #[test]
    fn rejects_private_bare_and_malformed_ids() {
        for bad in [
            "Private/5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z",
            "5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z",
            "Public/",
            "Public/0OIl",
            "Public/abc",
            "public/5eaMEJRpD9EcG3geaFia9rkQCRUPoBmTdr8jnttK31Z",
            "",
        ] {
            assert!(
                matches!(
                    validate_recipient(bad),
                    Err(ValidationError::InvalidPublicAccountId(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn request_id_must_be_lowercase_hyphenated_uuid() {
        assert!(validate_request_id("6f1e2c3a-9b8d-4e5f-a1b2-c3d4e5f60718").is_ok());
        for bad in [
            "6F1E2C3A-9B8D-4E5F-A1B2-C3D4E5F60718",
            "6f1e2c3a9b8d4e5fa1b2c3d4e5f60718",
            "{6f1e2c3a-9b8d-4e5f-a1b2-c3d4e5f60718}",
            "00000000-0000-0000-0000-000000000000",
            "not-a-uuid",
            "",
        ] {
            assert!(
                matches!(
                    validate_request_id(bad),
                    Err(ValidationError::InvalidRequestKey(_))
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn client_ip_prefers_first_forwarded_hop() {
        assert_eq!(
            client_ip(Some("203.0.113.9, 10.0.0.1"), Some("127.0.0.1")),
            "203.0.113.9"
        );
        assert_eq!(client_ip(Some("  "), Some("127.0.0.1")), "127.0.0.1");
        assert_eq!(client_ip(None, Some("127.0.0.1")), "127.0.0.1");
        assert_eq!(client_ip(None, None), "unknown");
    }

    #[test]
    fn ip_hash_is_salted_and_stable() {
        let a = ip_hash(b"salt", "203.0.113.9");
        assert_eq!(a, ip_hash(b"salt", "203.0.113.9"));
        assert_ne!(a, ip_hash(b"other", "203.0.113.9"));
        assert_ne!(a, ip_hash(b"salt", "203.0.113.10"));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn reserve_matches_assess_formula_at_ceilings() {
        let q = FeeQuote {
            height: 1,
            base_fee_exec: 8,
            base_fee_stor: 8,
            next_base_fee_exec_ceiling: 9,
            next_base_fee_stor_ceiling: 9,
        };
        assert_eq!(fee_reserve(2_000_000, &q), 2_000_000 * 9 + 100_000 * 9);
        assert_eq!(
            fee_reserve(2_000_000, &FeeQuote::FALLBACK),
            fee_reserve(2_000_000, &q)
        );
    }

    #[test]
    fn retry_after_rounds_up_and_never_zero() {
        let now = Utc::now();
        assert_eq!(
            retry_after_seconds(now - Duration::hours(23), DAY, now),
            3600
        );
        assert_eq!(retry_after_seconds(now - DAY, DAY, now), 1);
        assert_eq!(
            retry_after_seconds(now - Duration::milliseconds(500), HOUR, now),
            3600
        );
    }
}
