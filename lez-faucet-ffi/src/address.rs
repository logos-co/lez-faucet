//! Public account address normalization.
//!
//! The Faucet's entire user input is one public LEZ address. Everything that
//! is not unambiguously a public account is rejected here, before any network
//! call and long before anything is submitted.

use std::str::FromStr as _;

use lee::AccountId;
use serde::Serialize;

use crate::error::{ApiError, ApiResult, ErrorCode};

/// The one accepted address prefix.
const PUBLIC_PREFIX: &str = "Public/";

/// Longest input we will even look at, after the prefix is stripped.
///
/// A base58-encoded 32-byte account id is at most 44 characters, so 64 is
/// generous. This bound is load-bearing, not cosmetic: the `base58` crate used
/// by `AccountId::from_str` subtracts with overflow on an input of 133 or more
/// leading `1` characters and panics, and it checks the decoded length only
/// *after* decoding. A panic unwinding out of this crate's `extern "C"`
/// functions would abort the whole host application.
const MAX_INPUT_LEN: usize = 64;

/// A validated public account address in both of its presentation forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicAddress {
    /// Canonical bare base58 account id.
    pub account_id: String,
    /// `"Public/" + account_id`.
    pub address: String,
}

impl PublicAddress {
    #[must_use]
    pub fn new(account_id: AccountId) -> Self {
        let account_id = account_id.to_string();
        Self {
            address: format!("{PUBLIC_PREFIX}{account_id}"),
            account_id,
        }
    }
}

fn invalid(message: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidPublicAccountId, message)
}

/// Parse user input into a public [`AccountId`].
///
/// Accepts a bare base58 id or `Public/<base58>`. Every other prefix is
/// rejected by an **explicit** check rather than by relying on base58 decoding
/// to fail incidentally — notably `Private/`, which must never be routed to a
/// faucet drop and must produce a clear message rather than a confusing
/// "malformed id".
pub fn parse_public_address(input: &str) -> ApiResult<(AccountId, PublicAddress)> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Err(invalid("Enter a public LEZ account address."));
    }
    // Bound the raw value before doing any work on it, then bound the bare id
    // again below once the prefix is off.
    if trimmed.len() > MAX_INPUT_LEN + PUBLIC_PREFIX.len() {
        return Err(invalid("That address is too long to be a LEZ account ID."));
    }
    // A control character or an embedded NUL means the value was mangled on the
    // way in; refuse rather than silently normalizing it away.
    if trimmed.chars().any(char::is_control) {
        return Err(invalid("That address contains invalid characters."));
    }

    let bare = match trimmed.split_once('/') {
        None => trimmed,
        Some((prefix, rest)) => {
            if prefix == "Public" {
                rest
            } else if prefix == "Private" {
                return Err(invalid(
                    "This is a private account address. The faucet funds public accounts only.",
                ));
            } else {
                return Err(invalid(
                    "Unrecognized address prefix. Use a bare account ID or Public/<account-id>.",
                ));
            }
        }
    };

    // Reject a second separator, e.g. "Public/Private/x" or "Public/a/b".
    if bare.contains('/') {
        return Err(invalid(
            "Unrecognized address format. Use a bare account ID or Public/<account-id>.",
        ));
    }

    // The decoder below panics on long all-`1` inputs and only validates the
    // decoded length afterwards, so this gate must come first.
    if bare.len() > MAX_INPUT_LEN {
        return Err(invalid("That address is too long to be a LEZ account ID."));
    }

    let account_id = AccountId::from_str(bare).map_err(|_| {
        invalid("That is not a valid LEZ account ID. Expected 32 bytes encoded as base58.")
    })?;

    let normalized = PublicAddress::new(account_id);

    // Base58 has no canonical-form guarantee across arbitrary inputs; require
    // the input to round-trip so two spellings of one account cannot slip
    // through as distinct idempotency subjects.
    if normalized.account_id != bare {
        return Err(invalid(
            "That account ID is not in canonical form. Copy it directly from your wallet.",
        ));
    }

    Ok((account_id, normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r";

    fn code(input: &str) -> ErrorCode {
        parse_public_address(input).unwrap_err().code
    }

    #[test]
    fn accepts_bare_and_public_prefixed_forms_identically() {
        let (bare_id, bare) = parse_public_address(VALID).unwrap();
        let (prefixed_id, prefixed) = parse_public_address(&format!("Public/{VALID}")).unwrap();
        assert_eq!(bare_id, prefixed_id);
        assert_eq!(bare, prefixed);
        assert_eq!(bare.account_id, VALID);
        assert_eq!(bare.address, format!("Public/{VALID}"));
    }

    #[test]
    fn tolerates_surrounding_whitespace_from_a_paste() {
        let (_, address) = parse_public_address(&format!("  \t Public/{VALID}\n ")).unwrap();
        assert_eq!(address.account_id, VALID);
    }

    #[test]
    fn rejects_private_accounts_with_a_specific_message() {
        let error = parse_public_address(&format!("Private/{VALID}")).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidPublicAccountId);
        assert!(
            error.message.contains("private"),
            "a private address must be named as such, got: {}",
            error.message
        );
    }

    #[test]
    fn rejects_every_other_prefix_explicitly() {
        // Note: a *leading space* before "Public" is fine — it is trimmed. An
        // interior space is not part of the prefix and must be refused.
        for prefix in ["Shielded", "public", "PUBLIC", "Public ", "Pub", "x", ""] {
            assert_eq!(
                code(&format!("{prefix}/{VALID}")),
                ErrorCode::InvalidPublicAccountId,
                "prefix {prefix:?} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_nested_and_malformed_separators() {
        assert_eq!(
            code(&format!("Public/Private/{VALID}")),
            ErrorCode::InvalidPublicAccountId
        );
        assert_eq!(
            code(&format!("Public/{VALID}/x")),
            ErrorCode::InvalidPublicAccountId
        );
        assert_eq!(code("Public/"), ErrorCode::InvalidPublicAccountId);
        assert_eq!(code("/"), ErrorCode::InvalidPublicAccountId);
    }

    #[test]
    fn rejects_empty_oversized_and_control_characters() {
        assert_eq!(code(""), ErrorCode::InvalidPublicAccountId);
        assert_eq!(code("   "), ErrorCode::InvalidPublicAccountId);
        assert_eq!(
            code(&"a".repeat(MAX_INPUT_LEN + 1)),
            ErrorCode::InvalidPublicAccountId
        );
        assert_eq!(
            code(&format!("Public/{VALID}\u{0}")),
            ErrorCode::InvalidPublicAccountId
        );
        assert_eq!(
            code(&format!("Pub\u{7}lic/{VALID}")),
            ErrorCode::InvalidPublicAccountId
        );
    }

    #[test]
    fn rejects_malformed_base58() {
        // 0, O, I and l are not in the base58 alphabet.
        assert_eq!(code("0OIl"), ErrorCode::InvalidPublicAccountId);
        assert_eq!(code("not-an-account"), ErrorCode::InvalidPublicAccountId);
        // Right alphabet, wrong length.
        assert_eq!(code("abc"), ErrorCode::InvalidPublicAccountId);
    }

    #[test]
    fn a_pasted_secret_is_refused_and_never_echoed_back() {
        // If a user pastes a recovery phrase into the address field by mistake,
        // the value must be refused and must not appear in the message that is
        // shown, logged, or reported.
        let phrase = "abandon abandon abandon abandon abandon abandon \
                      abandon abandon abandon abandon abandon about";
        let error = parse_public_address(phrase).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidPublicAccountId);
        assert!(!error.message.contains("abandon"));
        assert!(error.details.is_none());
    }

    #[test]
    fn a_pasted_hex_private_key_is_refused_and_never_echoed_back() {
        let key = "0".repeat(64);
        let error = parse_public_address(&key).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidPublicAccountId);
        assert!(!error.message.contains(&key));
    }

    #[test]
    fn the_base58_decoder_panic_input_is_refused_by_length() {
        // `base58-0.2.0` computes `bin[len - 1 - leading_zeros ..]` and
        // subtracts with overflow once an input is all-`1` and 133 characters
        // or longer, and it validates the decoded length only afterwards. Since
        // this crate is loaded into the host application over a C ABI, that
        // panic would abort the entire app. The length gate must run first.
        for length in [133, 134, 512, 4096] {
            let bomb = "1".repeat(length);
            assert_eq!(code(&bomb), ErrorCode::InvalidPublicAccountId);
            assert_eq!(
                code(&format!("Public/{bomb}")),
                ErrorCode::InvalidPublicAccountId
            );
        }
    }

    #[test]
    fn arbitrary_input_never_panics() {
        // Cheap deterministic sweep over lengths, alphabets and separators
        // around every boundary this parser has.
        let alphabets = ["1", "z", "0", "/", " ", "\u{0}", "é", "1z0/ "];
        for alphabet in alphabets {
            for length in [0, 1, 31, 32, 43, 44, 45, 63, 64, 65, 132, 133, 200] {
                let input: String = alphabet.chars().cycle().take(length).collect();
                // Must return, never panic, for every prefix combination.
                let _ = parse_public_address(&input);
                let _ = parse_public_address(&format!("Public/{input}"));
                let _ = parse_public_address(&format!("Private/{input}"));
            }
        }
    }

    #[test]
    fn the_length_bound_applies_after_the_prefix_is_stripped() {
        let long_but_legal_prefix = format!("Public/{}", "1".repeat(MAX_INPUT_LEN + 1));
        assert_eq!(
            code(&long_but_legal_prefix),
            ErrorCode::InvalidPublicAccountId
        );
    }

    #[test]
    fn normalized_address_is_always_prefixed_form() {
        let (_, address) = parse_public_address(VALID).unwrap();
        assert!(address.address.starts_with(PUBLIC_PREFIX));
        assert!(!address.account_id.starts_with(PUBLIC_PREFIX));
    }
}
