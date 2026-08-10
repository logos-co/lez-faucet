#!/bin/sh

set -eu

usage() {
    cat <<'EOF'
Usage: scripts/check-program-fingerprint.sh

Compare the builtin program ImageIDs the sequencer reports against the ones
recorded in docs/testnet.md. Exit 0 when they agree, 1 when they have drifted.

Drift means the testnet was upgraded and this app's pinned LEZ revision no
longer matches it. Every claim will then fail closed with
`program_fingerprint_mismatch`, so the fix is to repin, not to edit the table:
see the procedure at the bottom of docs/testnet.md.

Environment:
  LEZ_FAUCET_SEQUENCER_URL  Sequencer JSON-RPC URL
                            (default: https://testnet.lez.logos.co)
EOF
}

fail() {
    printf 'check-program-fingerprint: %s\n' "$*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 ||
    fail "curl is required"
command -v python3 >/dev/null 2>&1 ||
    fail "Python 3 is required"

if [ "$#" -gt 0 ]; then
    case "$1" in
        -h | --help)
            usage
            exit 0
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
fi

# Resolve docs/testnet.md relative to this script, so the check works from any
# working directory (CI runs it from the repository root, humans rarely do).
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
contract=$script_dir/../docs/testnet.md
[ -f "$contract" ] ||
    fail "cannot find docs/testnet.md at $contract"

sequencer_url=${LEZ_FAUCET_SEQUENCER_URL:-https://testnet.lez.logos.co}

if ! response=$(
    curl -fsS \
        -H 'content-type: application/json' \
        --data-binary '{"jsonrpc":"2.0","id":1,"method":"getProgramIds","params":[]}' \
        "$sequencer_url"
); then
    fail "sequencer request failed: $sequencer_url"
fi

printf '%s\n' "$response" |
    CONTRACT_PATH="$contract" SEQUENCER_URL="$sequencer_url" python3 -c '
import json
import os
import re
import struct
import sys

# The two programs this client builds transactions against. The sequencer
# reports others (amm, token, privacy_preserving_circuit); drift in those
# cannot affect the faucet, so it is not an error here.
WATCHED = ("authenticated_transfer", "pinata")


def fail(message):
    print(f"check-program-fingerprint: {message}", file=sys.stderr)
    raise SystemExit(1)


def expected_ids(path):
    """Read the ImageID table out of docs/testnet.md.

    The document is the single source of truth for what this build expects, so
    the check reads it rather than carrying a second copy of the hex that could
    drift from it silently.
    """
    row = re.compile(r"^\|\s*`([a-z_]+)`\s*\|\s*`([0-9a-f]{64})`\s*\|\s*$")
    found = {}
    try:
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                match = row.match(line.rstrip("\n"))
                if match:
                    found[match.group(1)] = match.group(2)
    except OSError as error:
        fail(f"could not read {path}: {error}")

    missing = [name for name in WATCHED if name not in found]
    if missing:
        fail(
            "docs/testnet.md has no ImageID row for: "
            + ", ".join(missing)
            + " (has the table format changed?)"
        )
    return found


def deployed_ids(stream):
    try:
        envelope = json.load(stream)
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

    result = envelope.get("result")
    if not isinstance(result, dict):
        fail("getProgramIds result is not a JSON object")

    ids = {}
    for name, words in result.items():
        # A ProgramId is [u32; 8] on the wire; its hex form is the little-endian
        # encoding of those words, which is what docs/testnet.md records.
        if (
            not isinstance(words, list)
            or len(words) != 8
            or not all(isinstance(w, int) and 0 <= w <= 0xFFFFFFFF for w in words)
        ):
            fail(f"{name} program ID is not a [u32; 8]")
        ids[name] = b"".join(struct.pack("<I", w) for w in words).hex()
    return ids


expected = expected_ids(os.environ["CONTRACT_PATH"])
deployed = deployed_ids(sys.stdin)
sequencer = os.environ["SEQUENCER_URL"]

drifted = []
for name in WATCHED:
    if name not in deployed:
        drifted.append(f"  {name}: the sequencer did not report this program")
        continue
    if deployed[name] != expected[name]:
        drifted.append(
            f"  {name}\n"
            f"    docs/testnet.md: {expected[name]}\n"
            f"    {sequencer}: {deployed[name]}"
        )

if drifted:
    print(
        "check-program-fingerprint: the deployed program IDs no longer match "
        "the pinned ones.\n"
        "The testnet has been upgraded; this build will fail every claim with "
        "program_fingerprint_mismatch.\n"
        + "\n".join(drifted)
        + "\n\nRepin to the deployed LEZ revision, rebuild, then refresh the "
        "table in docs/testnet.md.",
        file=sys.stderr,
    )
    raise SystemExit(1)

for name in WATCHED:
    print(f"{name} {deployed[name]}")
print(f"fingerprint matches {sequencer}")
'
