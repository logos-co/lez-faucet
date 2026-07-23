#!/bin/sh

set -eu

usage() {
    cat <<'EOF'
Usage: scripts/lez-balance.sh <Public/account-id|account-id>

Print the public account's native LEZ balance as an exact decimal integer.

Environment:
  LEZ_FAUCET_SEQUENCER_URL  Sequencer JSON-RPC URL
                            (default: https://testnet.lez.logos.co)
EOF
}

fail() {
    printf 'lez-balance: %s\n' "$*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 ||
    fail "curl is required"
command -v python3 >/dev/null 2>&1 ||
    fail "Python 3 is required"

if [ "$#" -ne 1 ]; then
    usage >&2
    exit 2
fi

case "$1" in
    -h | --help)
        usage
        exit 0
        ;;
esac

account_id=$1
case "$account_id" in
    Public/*)
        account_id=${account_id#Public/}
        ;;
    Private/*)
        fail "private account balances are not public; provide a public account ID"
        ;;
    */*)
        fail "account ID must be bare base58 or use the Public/ prefix"
        ;;
esac

case "$account_id" in
    '' | *[!123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz]*)
        fail "account ID is not valid base58"
        ;;
esac

sequencer_url=${LEZ_FAUCET_SEQUENCER_URL:-https://testnet.lez.logos.co}

# account_id has already been restricted to the base58 alphabet, so it cannot
# inject JSON syntax into this request.
payload=$(printf \
    '{"jsonrpc":"2.0","id":1,"method":"getAccountBalance","params":["%s"]}' \
    "$account_id")

if ! response=$(
    curl -fsS \
        -H 'content-type: application/json' \
        --data-binary "$payload" \
        "$sequencer_url"
); then
    fail "sequencer request failed: $sequencer_url"
fi

if ! balance=$(
    printf '%s\n' "$response" |
        python3 -c '
import json
import sys

U128_MAX = "340282366920938463463374607431768211455"


class JsonInteger:
    def __init__(self, lexeme):
        self.lexeme = lexeme


class JsonNonInteger:
    pass


def fail(message):
    print(f"lez-balance: {message}", file=sys.stderr)
    raise SystemExit(1)


try:
    envelope = json.load(
        sys.stdin,
        parse_int=JsonInteger,
        parse_float=lambda _lexeme: JsonNonInteger(),
        parse_constant=lambda _lexeme: JsonNonInteger(),
    )
except (json.JSONDecodeError, UnicodeDecodeError):
    fail("sequencer returned malformed JSON")

if not isinstance(envelope, dict):
    fail("sequencer response is not a JSON object")

rpc_error = envelope.get("error")
if rpc_error is not None:
    if isinstance(rpc_error, dict) and isinstance(rpc_error.get("message"), str):
        detail = rpc_error["message"]
    else:
        detail = "unspecified error"
    fail(f"JSON-RPC error: {detail}")

if "result" not in envelope:
    fail("sequencer response has no result")

result = envelope["result"]
if not isinstance(result, JsonInteger):
    fail("balance result is not a decimal integer")
lexeme = result.lexeme
if lexeme.startswith("-"):
    fail("balance result is negative")
if len(lexeme) > len(U128_MAX) or (
    len(lexeme) == len(U128_MAX) and lexeme > U128_MAX
):
    fail("balance result exceeds u128")

print(lexeme)
'
); then
    exit 1
fi

printf '%s\n' "$balance"
