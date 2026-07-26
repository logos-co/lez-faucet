//! Headless LEZ public-testnet faucet client and its small C ABI.
//!
//! The Faucet funds one eligible public LEZ account by exactly one fixed prize
//! per user action. It is a faucet client, not a wallet:
//!
//! - it never creates, opens, imports or manages a wallet;
//! - it never accepts a password, recovery phrase, private key or signature;
//! - it never writes a file or persists anything across a restart.
//!
//! Those are not merely absent features. A Piñata claim names both accounts as
//! unsigned public participants, so there is nothing to sign with — any key
//! material in this process would be material it did not need.
//!
//! Every LEZ crate is pinned by the workspace manifest to the revision deployed
//! on the public testnet, and a runtime fingerprint check still runs before any
//! state-changing operation so version skew fails loudly rather than quietly.

pub mod address;
pub mod client;
pub mod error;
pub mod solver;

use std::{
    ffi::{c_char, CStr, CString},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::OnceLock,
};

use serde::Serialize;
use tokio::runtime::Runtime;

pub use crate::{
    client::{FaucetClient, FaucetPolicy, DEFAULT_SEQUENCER_URL, PRIZE},
    error::{ApiError, ErrorCode},
};

/// Opaque handle owned by the caller.
pub struct LezFaucetClient {
    client: FaucetClient,
}

#[repr(C)]
pub struct LezFaucetClientOutput {
    pub client: *mut LezFaucetClient,
    /// `{"ok":true,...}` or `{"ok":false,"error":ApiError}`. Free with
    /// [`lez_faucet_string_free`].
    pub result_json: *mut c_char,
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("failed to create the Tokio runtime"))
}

fn c_string(value: &str) -> *mut c_char {
    CString::new(value.replace('\0', "\u{fffd}")).map_or(ptr::null_mut(), CString::into_raw)
}

fn envelope<T: Serialize>(result: Result<T, ApiError>) -> *mut c_char {
    let value = match result {
        Ok(result) => serde_json::json!({ "ok": true, "result": result }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    c_string(&value.to_string())
}

fn error_envelope(error: &ApiError) -> *mut c_char {
    c_string(&serde_json::json!({ "ok": false, "error": error }).to_string())
}

/// Read a caller-supplied string.
///
/// A null pointer or non-UTF-8 bytes are a caller bug, but they must produce a
/// structured error rather than undefined behaviour.
unsafe fn required_str<'a>(value: *const c_char, name: &str) -> Result<&'a str, ApiError> {
    if value.is_null() {
        return Err(ApiError::internal(format!("{name} was null")));
    }
    // SAFETY: the caller contract requires a valid NUL-terminated string.
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| ApiError::internal(format!("{name} was not valid UTF-8")))
}

/// Run an FFI body, converting a panic into a structured error.
///
/// A panic unwinding across the FFI boundary is undefined behaviour, so every
/// entry point is wrapped. A panic here is a bug in this library; the caller
/// should still get an error instead of a crashed host application.
fn guarded<T: Serialize>(body: impl FnOnce() -> Result<T, ApiError>) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => envelope(result),
        Err(_) => error_envelope(&ApiError::internal(
            "The faucet hit an internal error and stopped the request safely.",
        )),
    }
}

/// Borrow a client handle.
fn with_client<T: Serialize>(
    handle: *mut LezFaucetClient,
    body: impl FnOnce(&FaucetClient) -> Result<T, ApiError>,
) -> *mut c_char {
    guarded(|| {
        if handle.is_null() {
            return Err(ApiError::new(
                ErrorCode::NotConfigured,
                "The faucet client is not configured.",
            ));
        }
        // SAFETY: the caller must pass a live handle returned by this library
        // and must not destroy it concurrently.
        let handle = unsafe { &*handle };
        body(&handle.client)
    })
}

/// Create a faucet client for `sequencer_url`.
///
/// Performs no filesystem access and creates no key material.
///
/// # Safety
/// `sequencer_url` must be a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_client_new(
    sequencer_url: *const c_char,
) -> LezFaucetClientOutput {
    let built = catch_unwind(AssertUnwindSafe(|| {
        let sequencer_url = unsafe { required_str(sequencer_url, "sequencer_url") }?;
        FaucetClient::new(sequencer_url)
    }));

    match built {
        Ok(Ok(client)) => LezFaucetClientOutput {
            client: Box::into_raw(Box::new(LezFaucetClient { client })),
            result_json: c_string(&serde_json::json!({ "ok": true }).to_string()),
        },
        Ok(Err(error)) => LezFaucetClientOutput {
            client: ptr::null_mut(),
            result_json: error_envelope(&error),
        },
        Err(_) => LezFaucetClientOutput {
            client: ptr::null_mut(),
            result_json: error_envelope(&ApiError::internal("The faucet could not start safely.")),
        },
    }
}

/// Destroy a client handle.
///
/// # Safety
/// `client` must be null, or a live handle returned by this library that is
/// not in use and is never used or destroyed again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_client_destroy(client: *mut LezFaucetClient) {
    if !client.is_null() {
        // SAFETY: the caller must destroy a handle exactly once.
        drop(unsafe { Box::from_raw(client) });
    }
}

/// Free a string returned by this library.
///
/// # Safety
/// `value` must be null, or a pointer returned by this library that is never
/// used or freed again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: the pointer must have been allocated by `c_string`.
        drop(unsafe { CString::from_raw(value) });
    }
}

/// Read faucet and pool status.
///
/// # Safety
/// `client` must be a live handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_get_info(client: *mut LezFaucetClient) -> *mut c_char {
    with_client(client, |client| runtime().block_on(client.get_info()))
}

/// Inspect a recipient without changing any network state.
///
/// # Safety
/// `client` must be live and `address` a valid NUL-terminated UTF-8 string for
/// the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_inspect_recipient(
    client: *mut LezFaucetClient,
    address: *const c_char,
) -> *mut c_char {
    with_client(client, |client| {
        let address = unsafe { required_str(address, "address") }?;
        runtime().block_on(client.inspect_recipient(address))
    })
}

/// Request exactly one faucet credit.
///
/// Repeating the same `request_key` with the same address replays the original
/// outcome and never produces a second claim.
///
/// # Safety
/// `client` must be live and both strings valid NUL-terminated UTF-8 for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_request_drop(
    client: *mut LezFaucetClient,
    address: *const c_char,
    request_key: *const c_char,
    operation_token: u64,
) -> *mut c_char {
    with_client(client, |client| {
        let address = unsafe { required_str(address, "address") }?;
        let request_key = unsafe { required_str(request_key, "request_key") }?;
        runtime().block_on(client.request_drop(address, request_key, operation_token))
    })
}

/// Read the live phase of the drop identified by `operation_token`.
///
/// The polled companion to [`lez_faucet_request_drop`], and deliberately not a
/// callback: this ABI carries no function pointers. Call it from a different
/// thread while that call is blocked — it reads two atomics and returns, and
/// never takes the drop permit or any lock the drop holds, so it can neither
/// block nor be blocked by the drop it observes. The result is
/// `{"phase":"solving"}` while that drop is running and `{"phase":null}` for an
/// unknown or finished token, which is a normal answer, not an error.
///
/// # Safety
/// `client` must be a live handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_current_phase(
    client: *mut LezFaucetClient,
    operation_token: u64,
) -> *mut c_char {
    with_client(client, |client| {
        Ok(serde_json::json!({ "phase": client.current_phase(operation_token) }))
    })
}

/// Ask the operation identified by `operation_token` to stop.
///
/// Cooperative and scoped: a cancel that arrives after its operation has
/// finished cannot affect a later one. If submission has already begun the
/// chain action cannot be recalled, and the call in flight finishes its bounded
/// reconciliation rather than reporting a cancellation it cannot honour.
///
/// Takes no lock and is safe to call from another thread.
///
/// # Safety
/// `client` must be a live handle returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lez_faucet_cancel(client: *mut LezFaucetClient, operation_token: u64) {
    if client.is_null() {
        return;
    }
    // SAFETY: the caller contract requires a live handle.
    let handle = unsafe { &*client };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        handle.client.cancellation().request(operation_token);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// Take ownership of an FFI string and parse it, freeing the original.
    fn take(pointer: *mut c_char) -> serde_json::Value {
        assert!(!pointer.is_null());
        let json = unsafe { CStr::from_ptr(pointer) }
            .to_str()
            .unwrap()
            .to_owned();
        unsafe { lez_faucet_string_free(pointer) };
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn a_null_url_is_an_error_not_a_crash() {
        let output = unsafe { lez_faucet_client_new(ptr::null()) };
        assert!(output.client.is_null());
        let json = take(output.result_json);
        assert_eq!(json["ok"], false);
        unsafe { lez_faucet_client_destroy(output.client) };
    }

    #[test]
    fn an_invalid_url_is_reported_structurally() {
        let url = CString::new("http://example.com").unwrap();
        let output = unsafe { lez_faucet_client_new(url.as_ptr()) };
        assert!(output.client.is_null());
        let json = take(output.result_json);
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "invalid_sequencer_url");
        // Callers dispatch on the code; a message is for a human only.
        assert!(json["error"]["message"].is_string());
    }

    #[test]
    fn calls_on_a_null_client_are_reported_not_dereferenced() {
        let address = CString::new("CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r").unwrap();
        let key = CString::new("3f2504e0-4f89-41d3-9a0c-0305e82c3301").unwrap();

        for json in [
            take(unsafe { lez_faucet_get_info(ptr::null_mut()) }),
            take(unsafe { lez_faucet_inspect_recipient(ptr::null_mut(), address.as_ptr()) }),
            take(unsafe {
                lez_faucet_request_drop(ptr::null_mut(), address.as_ptr(), key.as_ptr(), 1)
            }),
            take(unsafe { lez_faucet_current_phase(ptr::null_mut(), 1) }),
        ] {
            assert_eq!(json["ok"], false);
            assert_eq!(json["error"]["code"], "not_configured");
        }

        // Must not crash.
        unsafe { lez_faucet_cancel(ptr::null_mut(), 1) };
        unsafe { lez_faucet_client_destroy(ptr::null_mut()) };
    }

    #[test]
    fn a_bad_address_is_rejected_before_any_network_call() {
        let url = CString::new("https://testnet.lez.logos.co").unwrap();
        let output = unsafe { lez_faucet_client_new(url.as_ptr()) };
        assert!(!output.client.is_null());
        take(output.result_json);

        let address = CString::new("Private/CbgR6tj5kWx5oziiFptM7jMvrQeYY3Mzaao6ciuhSr2r").unwrap();
        let key = CString::new("3f2504e0-4f89-41d3-9a0c-0305e82c3301").unwrap();
        let json = take(unsafe {
            lez_faucet_request_drop(output.client, address.as_ptr(), key.as_ptr(), 1)
        });
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "invalid_public_account_id");
        assert_eq!(json["error"]["phase"], "validating_input");

        // A malformed request key is likewise refused offline.
        let bad_key = CString::new("nope").unwrap();
        let json = take(unsafe {
            lez_faucet_request_drop(output.client, address.as_ptr(), bad_key.as_ptr(), 1)
        });
        assert_eq!(json["error"]["code"], "invalid_request_key");

        unsafe { lez_faucet_client_destroy(output.client) };
    }

    #[test]
    fn an_idle_or_unknown_token_reports_no_phase_not_an_error() {
        let url = CString::new("https://testnet.lez.logos.co").unwrap();
        let output = unsafe { lez_faucet_client_new(url.as_ptr()) };
        assert!(!output.client.is_null());
        take(output.result_json);

        // No drop is running, so every token — including the 0 sentinel —
        // answers with a null phase inside a successful envelope.
        for token in [0, 1, u64::MAX] {
            let json = take(unsafe { lez_faucet_current_phase(output.client, token) });
            assert_eq!(json["ok"], true);
            assert_eq!(json["result"]["phase"], serde_json::Value::Null);
        }

        unsafe { lez_faucet_client_destroy(output.client) };
    }

    #[test]
    fn null_string_arguments_are_errors_not_crashes() {
        let url = CString::new("https://testnet.lez.logos.co").unwrap();
        let output = unsafe { lez_faucet_client_new(url.as_ptr()) };
        take(output.result_json);

        let json = take(unsafe { lez_faucet_inspect_recipient(output.client, ptr::null()) });
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "internal_error");

        unsafe { lez_faucet_client_destroy(output.client) };
    }

    #[test]
    fn this_library_exposes_no_wallet_or_secret_surface() {
        // A static guard against the shape of the previous release, which asked
        // for a password the underlying wallet ignored and wrote key material
        // to disk. If any of these ever reappear in this file, the product has
        // regressed into being a wallet again.
        let source = include_str!("lib.rs");
        let body: String = source
            .split("mod tests")
            .next()
            .expect("the test module marker must exist")
            .lines()
            // Prose may name these in order to say they are absent; code may not.
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "password",
            "mnemonic",
            "wallet_core",
            "WalletCore",
            "private_key",
            "storage_path",
            "config_path",
            "std::fs",
            "claim_until",
            "target_balance",
        ] {
            assert!(
                !body.contains(forbidden),
                "the public FFI must not mention {forbidden:?}"
            );
        }
    }
}
