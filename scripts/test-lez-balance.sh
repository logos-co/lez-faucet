#!/bin/sh

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
balance_script=$script_dir/lez-balance.sh
real_python=$(command -v python3) || {
    printf 'test-lez-balance: Python 3 is required\n' >&2
    exit 1
}
python_dir=$(dirname -- "$real_python")

test_root=$(mktemp -d "${TMPDIR:-/tmp}/lez-balance-test.XXXXXX")
trap '/bin/rm -rf -- "$test_root"' EXIT HUP INT TERM

mock_bin=$test_root/bin
empty_bin=$test_root/empty
payload_file=$test_root/payload.json
url_file=$test_root/url.txt
mkdir -p "$mock_bin" "$empty_bin"

printf '%s\n' \
    '#!/bin/sh' \
    'set -eu' \
    'while [ "$#" -gt 0 ]; do' \
    '    case "$1" in' \
    '        --data-binary)' \
    '            shift' \
    '            printf "%s\\n" "$1" > "$MOCK_CURL_PAYLOAD_FILE"' \
    '            ;;' \
    '        http://* | https://*)' \
    '            printf "%s\\n" "$1" > "$MOCK_CURL_URL_FILE"' \
    '            ;;' \
    '    esac' \
    '    shift' \
    'done' \
    'printf "%s\\n" "${MOCK_CURL_RESPONSE:-}"' \
    'exit "${MOCK_CURL_EXIT:-0}"' \
    >"$mock_bin/curl"
chmod +x "$mock_bin/curl"

tests_run=0

pass() {
    tests_run=$((tests_run + 1))
    printf 'ok %s - %s\n' "$tests_run" "$1"
}

fail_test() {
    printf 'not ok %s - %s\n' "$((tests_run + 1))" "$1" >&2
    exit 1
}

run_script() {
    PATH="$mock_bin:$python_dir:/usr/bin:/bin" \
        MOCK_CURL_PAYLOAD_FILE="$payload_file" \
        MOCK_CURL_URL_FILE="$url_file" \
        "$balance_script" "$@"
}

account_id=11111111111111111111111111111111

payload_value() {
    "$real_python" -c '
import json
import sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
if sys.argv[2] == "account":
    print(value["params"][0])
elif sys.argv[2] == "method":
    print(value["method"])
' "$payload_file" "$1"
}

output=$(
    MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":150}' \
        run_script "Public/$account_id"
) || fail_test "strips Public/ and prints the balance"
[ "$output" = 150 ] || fail_test "strips Public/ and prints the balance"
[ "$(payload_value account)" = "$account_id" ] ||
    fail_test "strips Public/ and prints the balance"
[ "$(payload_value method)" = getAccountBalance ] ||
    fail_test "strips Public/ and prints the balance"
pass "strips Public/ and prints the balance"

output=$(
    LEZ_FAUCET_SEQUENCER_URL='https://sequencer.example.test' \
        MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":1050}' \
        run_script "$account_id"
) || fail_test "accepts a bare ID and honors the sequencer override"
[ "$output" = 1050 ] ||
    fail_test "accepts a bare ID and honors the sequencer override"
[ "$(cat "$url_file")" = 'https://sequencer.example.test' ] ||
    fail_test "accepts a bare ID and honors the sequencer override"
pass "accepts a bare ID and honors the sequencer override"

output=$(
    MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":9007199254740993}' \
        run_script "$account_id"
) || fail_test "preserves an integer above the IEEE-754 safe range"
[ "$output" = 9007199254740993 ] ||
    fail_test "preserves an integer above the IEEE-754 safe range"
pass "preserves an integer above the IEEE-754 safe range"

u128_max=340282366920938463463374607431768211455
output=$(
    MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":340282366920938463463374607431768211455}' \
        run_script "$account_id"
) || fail_test "preserves u128 max exactly"
[ "$output" = "$u128_max" ] || fail_test "preserves u128 max exactly"
pass "preserves u128 max exactly"

rm -f "$payload_file"
if run_script >"$test_root/no-arg.out" 2>"$test_root/no-arg.err"; then
    fail_test "rejects a missing account ID before network access"
fi
[ ! -e "$payload_file" ] ||
    fail_test "rejects a missing account ID before network access"
grep -q '^Usage:' "$test_root/no-arg.err" ||
    fail_test "rejects a missing account ID before network access"
pass "rejects a missing account ID before network access"

rm -f "$payload_file"
if run_script 'Private/11111111111111111111111111111111' \
    >"$test_root/private.out" 2>"$test_root/private.err"; then
    fail_test "rejects a private account before network access"
fi
[ ! -e "$payload_file" ] ||
    fail_test "rejects a private account before network access"
grep -q 'private account balances are not public' "$test_root/private.err" ||
    fail_test "rejects a private account before network access"
pass "rejects a private account before network access"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Invalid params"}}' \
    run_script "$account_id" >"$test_root/rpc.out" 2>"$test_root/rpc.err"; then
    fail_test "rejects JSON-RPC errors"
fi
grep -q 'JSON-RPC error: Invalid params' "$test_root/rpc.err" ||
    fail_test "rejects JSON-RPC errors"
pass "rejects JSON-RPC errors"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":' \
    run_script "$account_id" >"$test_root/malformed.out" 2>"$test_root/malformed.err"; then
    fail_test "rejects malformed JSON"
fi
grep -q 'sequencer returned malformed JSON' "$test_root/malformed.err" ||
    fail_test "rejects malformed JSON"
pass "rejects malformed JSON"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":"150"}' \
    run_script "$account_id" >"$test_root/type.out" 2>"$test_root/type.err"; then
    fail_test "rejects a result string"
fi
grep -q 'balance result is not a decimal integer' "$test_root/type.err" ||
    fail_test "rejects a result string"
pass "rejects a result string"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":1e3}' \
    run_script "$account_id" >"$test_root/exponent.out" 2>"$test_root/exponent.err"; then
    fail_test "rejects exponent notation"
fi
grep -q 'balance result is not a decimal integer' "$test_root/exponent.err" ||
    fail_test "rejects exponent notation"
pass "rejects exponent notation"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":1.5}' \
    run_script "$account_id" >"$test_root/fraction.out" 2>"$test_root/fraction.err"; then
    fail_test "rejects a fractional result"
fi
grep -q 'balance result is not a decimal integer' "$test_root/fraction.err" ||
    fail_test "rejects a fractional result"
pass "rejects a fractional result"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":-1}' \
    run_script "$account_id" >"$test_root/negative.out" 2>"$test_root/negative.err"; then
    fail_test "rejects a negative result"
fi
grep -q 'balance result is negative' "$test_root/negative.err" ||
    fail_test "rejects a negative result"
pass "rejects a negative result"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":-0}' \
    run_script "$account_id" >"$test_root/negative-zero.out" 2>"$test_root/negative-zero.err"; then
    fail_test "rejects a negative-zero lexeme"
fi
grep -q 'balance result is negative' "$test_root/negative-zero.err" ||
    fail_test "rejects a negative-zero lexeme"
pass "rejects a negative-zero lexeme"

if MOCK_CURL_RESPONSE='{"jsonrpc":"2.0","id":1,"result":340282366920938463463374607431768211456}' \
    run_script "$account_id" >"$test_root/overflow.out" 2>"$test_root/overflow.err"; then
    fail_test "rejects a result above u128 max"
fi
grep -q 'balance result exceeds u128' "$test_root/overflow.err" ||
    fail_test "rejects a result above u128 max"
pass "rejects a result above u128 max"

if MOCK_CURL_EXIT=22 MOCK_CURL_RESPONSE='upstream HTTP error' \
    run_script "$account_id" >"$test_root/http.out" 2>"$test_root/http.err"; then
    fail_test "reports HTTP and transport failures"
fi
grep -q 'sequencer request failed' "$test_root/http.err" ||
    fail_test "reports HTTP and transport failures"
pass "reports HTTP and transport failures"

if PATH="$empty_bin" /bin/sh "$balance_script" "$account_id" \
    >"$test_root/no-curl.out" 2>"$test_root/no-curl.err"; then
    fail_test "reports a missing curl dependency"
fi
grep -q 'curl is required' "$test_root/no-curl.err" ||
    fail_test "reports a missing curl dependency"
pass "reports a missing curl dependency"

if PATH="$mock_bin" /bin/sh "$balance_script" "$account_id" \
    >"$test_root/no-python.out" 2>"$test_root/no-python.err"; then
    fail_test "reports a missing Python 3 dependency"
fi
grep -q 'Python 3 is required' "$test_root/no-python.err" ||
    fail_test "reports a missing Python 3 dependency"
pass "reports a missing Python 3 dependency"

printf '1..%s\n' "$tests_run"
