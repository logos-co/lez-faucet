#ifndef LEZ_FAUCET_FFI_H
#define LEZ_FAUCET_FFI_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

typedef struct FaucetHandle FaucetHandle;

typedef struct FfiCreateOutput {
  struct FaucetHandle *handle;
  /**
   * JSON containing the one-time mnemonic or an error. Free with
   * `lez_faucet_string_free`.
   */
  char *result_json;
} FfiCreateOutput;

/**
 * Called synchronously after each successful claim. The JSON pointer is valid
 * only for the duration of the callback; copy it before returning.
 */
typedef void (*FfiProgressCallback)(const char*, void*);

/**
 * The exact public-testnet reward encoded by the v0.2.0 Piñata guest.
 */
#define PINATA_PRIZE 150

/**
 * Create a new wallet and return its opaque handle plus one-time recovery JSON.
 *
 * # Safety
 * All string pointers must be non-null, valid, NUL-terminated UTF-8 strings.
 */
struct FfiCreateOutput lez_faucet_create(const char *config_path,
                                         const char *storage_path,
                                         const char *sequencer_url,
                                         const char *password);

/**
 * Open an existing wallet and return its handle plus status JSON.
 *
 * # Safety
 * All string pointers must be non-null, valid, NUL-terminated UTF-8 strings.
 */
struct FfiCreateOutput lez_faucet_open(const char *config_path,
                                       const char *storage_path,
                                       const char *sequencer_url);

/**
 * Destroy a wallet handle.
 *
 * # Safety
 * `handle` must be null or a live handle returned by this library, and must
 * not be used or destroyed again after this call.
 */
void lez_faucet_destroy(struct FaucetHandle *handle);

/**
 * Free a JSON string returned by this library.
 *
 * # Safety
 * `value` must be null or a pointer returned by this library, and must not be
 * used or freed again after this call.
 */
void lez_faucet_string_free(char *value);

/**
 * Verify that the sequencer's builtin program IDs match the pinned client.
 *
 * # Safety
 * `handle` must be a live handle returned by this library.
 */
char *lez_faucet_verify_fingerprint(struct FaucetHandle *handle);

/**
 * Create or resume and initialize one public account.
 *
 * # Safety
 * `handle` must be a live handle returned by this library.
 */
char *lez_faucet_create_and_initialize_account(struct FaucetHandle *handle);

/**
 * Read a public account's balance.
 *
 * # Safety
 * `handle` must be live and `account_id` must be a valid NUL-terminated UTF-8
 * string for the duration of this call.
 */
char *lez_faucet_get_balance(struct FaucetHandle *handle, const char *account_id);

/**
 * Solve and submit one Piñata claim.
 *
 * # Safety
 * `handle` must be live and `account_id` must be a valid NUL-terminated UTF-8
 * string for the duration of this call.
 */
char *lez_faucet_claim_once(struct FaucetHandle *handle, const char *account_id);

/**
 * Claim repeatedly until the decimal target is reached or an error occurs.
 *
 * # Safety
 * `handle` must be live; string pointers must be valid NUL-terminated UTF-8;
 * and any callback/context pair must remain valid until this call returns.
 * The callback must not re-enter this API while the wallet operation is live.
 */
char *lez_faucet_claim_until_target(struct FaucetHandle *handle,
                                    const char *account_id,
                                    const char *target,
                                    size_t max_claims,
                                    FfiProgressCallback progress_callback,
                                    void *progress_context);

#endif  /* LEZ_FAUCET_FFI_H */
