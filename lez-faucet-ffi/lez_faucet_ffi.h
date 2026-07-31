#ifndef LEZ_FAUCET_FFI_H
#define LEZ_FAUCET_FFI_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Challenge data as stored in the Piñata account: one difficulty byte
 * followed by a 32-byte seed.
 */
#define CHALLENGE_LEN 33

/**
 * The protocol permits 0..=32. Anything above this is refused by the client
 * because the work is not bounded in a way we are willing to impose on a
 * user's machine.
 */
#define MAX_SUPPORTED_DIFFICULTY 3

/**
 * Opaque handle owned by the caller.
 */
typedef struct LezFaucetClient LezFaucetClient;

typedef struct LezFaucetClientOutput {
  struct LezFaucetClient *client;
  /**
   * `{"ok":true,...}` or `{"ok":false,"error":ApiError}`. Free with
   * [`lez_faucet_string_free`].
   */
  char *result_json;
} LezFaucetClientOutput;

/**
 * The exact prize the deployed Piñata guest pays per successful claim.
 */
#define PRIZE 150

/**
 * Create a faucet client for `sequencer_url`.
 *
 * Performs no filesystem access and creates no key material.
 *
 * # Safety
 * `sequencer_url` must be a valid NUL-terminated UTF-8 string.
 */
struct LezFaucetClientOutput lez_faucet_client_new(const char *sequencer_url);

/**
 * Destroy a client handle.
 *
 * # Safety
 * `client` must be null, or a live handle returned by this library that is
 * not in use and is never used or destroyed again.
 */
void lez_faucet_client_destroy(struct LezFaucetClient *client);

/**
 * Free a string returned by this library.
 *
 * # Safety
 * `value` must be null, or a pointer returned by this library that is never
 * used or freed again.
 */
void lez_faucet_string_free(char *value);

/**
 * Read faucet and pool status.
 *
 * # Safety
 * `client` must be a live handle returned by this library.
 */
char *lez_faucet_get_info(struct LezFaucetClient *client);

/**
 * Inspect a recipient without changing any network state.
 *
 * # Safety
 * `client` must be live and `address` a valid NUL-terminated UTF-8 string for
 * the duration of the call.
 */
char *lez_faucet_inspect_recipient(struct LezFaucetClient *client, const char *address);

/**
 * Request exactly one faucet credit.
 *
 * Repeating the same `request_key` with the same address replays the original
 * outcome and never produces a second claim.
 *
 * # Safety
 * `client` must be live and both strings valid NUL-terminated UTF-8 for the
 * duration of the call.
 */
char *lez_faucet_request_drop(struct LezFaucetClient *client,
                              const char *address,
                              const char *request_key,
                              uint64_t operation_token);

/**
 * Read the live phase of the drop identified by `operation_token`.
 *
 * The polled companion to [`lez_faucet_request_drop`], and deliberately not a
 * callback: this ABI carries no function pointers. Call it from a different
 * thread while that call is blocked — it reads two atomics and returns, and
 * never takes the drop permit or any lock the drop holds, so it can neither
 * block nor be blocked by the drop it observes. The result is
 * `{"phase":"solving"}` while that drop is running and `{"phase":null}` for an
 * unknown or finished token, which is a normal answer, not an error.
 *
 * # Safety
 * `client` must be a live handle returned by this library.
 */
char *lez_faucet_current_phase(struct LezFaucetClient *client, uint64_t operation_token);

/**
 * Ask the operation identified by `operation_token` to stop.
 *
 * Cooperative and scoped: a cancel that arrives after its operation has
 * finished cannot affect a later one. If submission has already begun the
 * chain action cannot be recalled, and the call in flight finishes its bounded
 * reconciliation rather than reporting a cancellation it cannot honour.
 *
 * Takes no lock and is safe to call from another thread.
 *
 * # Safety
 * `client` must be a live handle returned by this library.
 */
void lez_faucet_cancel(struct LezFaucetClient *client, uint64_t operation_token);

#endif  /* LEZ_FAUCET_FFI_H */
