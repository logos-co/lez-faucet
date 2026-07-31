#ifndef LEZ_FAUCET_FFI_H
#define LEZ_FAUCET_FFI_H

/*
 * Test stub of the generated `lez-faucet-ffi/lez_faucet_ffi.h`.
 *
 * It must stay symbol-for-symbol identical to the generated header: the module
 * is compiled against this one in tests and against the real one in Basecamp,
 * so any drift here is a defect the test suite cannot see.
 */

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define CHALLENGE_LEN 33
#define MAX_SUPPORTED_DIFFICULTY 3
#define PRIZE 150

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LezFaucetClient LezFaucetClient;

typedef struct LezFaucetClientOutput {
    struct LezFaucetClient* client;
    char* result_json;
} LezFaucetClientOutput;

struct LezFaucetClientOutput lez_faucet_client_new(const char* sequencer_url);
void lez_faucet_client_destroy(struct LezFaucetClient* client);
void lez_faucet_string_free(char* value);
char* lez_faucet_get_info(struct LezFaucetClient* client);
char* lez_faucet_inspect_recipient(struct LezFaucetClient* client, const char* address);
char* lez_faucet_request_drop(struct LezFaucetClient* client, const char* address, const char* request_key, uint64_t operation_token);
char* lez_faucet_current_phase(struct LezFaucetClient* client, uint64_t operation_token);
void lez_faucet_cancel(struct LezFaucetClient* client, uint64_t operation_token);

#ifdef __cplusplus
}
#endif

#endif /* LEZ_FAUCET_FFI_H */
