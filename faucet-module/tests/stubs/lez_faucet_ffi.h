#ifndef LEZ_FAUCET_FFI_H
#define LEZ_FAUCET_FFI_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct FaucetHandle FaucetHandle;

typedef struct FfiCreateOutput {
    FaucetHandle* handle;
    char* result_json;
} FfiCreateOutput;

typedef void (*FfiProgressCallback)(const char*, void*);

FfiCreateOutput lez_faucet_create(const char* config_path, const char* storage_path, const char* sequencer_url, const char* password);
FfiCreateOutput lez_faucet_open(const char* config_path, const char* storage_path, const char* sequencer_url);
void lez_faucet_destroy(FaucetHandle* handle);
void lez_faucet_string_free(char* value);
void lez_faucet_cancel(FaucetHandle* handle);
char* lez_faucet_verify_fingerprint(FaucetHandle* handle);
char* lez_faucet_create_and_initialize_account(FaucetHandle* handle);
char* lez_faucet_get_balance(FaucetHandle* handle, const char* account_id);
char* lez_faucet_claim_once(FaucetHandle* handle, const char* account_id);
char* lez_faucet_claim_until_target(FaucetHandle* handle, const char* account_id, const char* target, size_t max_claims, FfiProgressCallback progress_callback, void* progress_context);

#ifdef __cplusplus
}
#endif

#endif
