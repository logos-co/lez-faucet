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
command -v jq >/dev/null 2>&1 ||
    fail "jq is required"

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

payload=$(
    jq -cn --arg account_id "$account_id" \
        '{jsonrpc:"2.0",id:1,method:"getAccountBalance",params:[$account_id]}'
) || fail "could not construct the JSON-RPC request"

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
        jq -er '
            if type != "object" then
                error("response is not a JSON object")
            elif .error != null then
                error("JSON-RPC error: " + (.error.message // (.error | tostring)))
            elif has("result") | not then
                error("response has no result")
            elif (.result | type) != "number" then
                error("balance result is not a number")
            else
                .result | tostring
            end
        '
); then
    fail "sequencer returned an invalid balance response"
fi

case "$balance" in
    '' | *[!0-9]*)
        fail "balance result is not a non-negative decimal integer"
        ;;
esac

printf '%s\n' "$balance"
