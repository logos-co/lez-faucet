#!/usr/bin/env python3
"""Assert that a built .lgx's embedded manifest.json agrees with the module's
source metadata.json.

Why this exists
---------------

A .lgx is produced in two stages. `lgx create` (logos-package) writes a base
manifest.json describing the payload — `manifestVersion`, `main`, `hashes`.
Then nix-bundle-lgx's bundle.sh reopens the tarball and patches the module's
own descriptive fields in from metadata.json, through an explicit key
allowlist:

    for key in ('name', 'display_name', 'version', 'description',
                'author', 'type', 'category', 'dependencies', 'view'):
        if metadata.get(key):
            manifest[key] = metadata[key]

That allowlist is the hazard. It lives in a *different repo*, pinned by
flake.lock, and it enumerates keys by hand. A module can add a perfectly
valid metadata.json field and have the packaging tool silently drop it,
because the tool predates the key and simply does not look for it. Nothing
fails: not the build, not a test, not the release. The field is just gone
from the artifact users install.

This is not hypothetical. Sibling repo logos-co/eth-lez-atomic-swaps shipped
swap v0.3.0 with no `display_name` in its manifest, because its flake.lock
pinned nix-bundle-lgx at 3c44d99b — one commit before `display_name` was
added to that allowlist in 038f9cb. The module's metadata.json was correct
the whole time. See https://github.com/logos-co/eth-lez-atomic-swaps/issues/60

This repo escaped by a single commit: its lock pins b49074a8, the merge that
added `display_name`. Luck, not design. So: check the round trip in CI, on
every build, for both modules, and fail loudly naming the field.

What is checked
---------------

1. Round trip. For every key in ROUND_TRIP that carries a truthy value in
   metadata.json, manifest.json must carry exactly that value. Truthy is the
   right condition because bundle.sh's `if metadata.get(key)` is itself
   truthiness-gated: a source `[]` or `""` is deliberately left alone, so
   demanding it appear would be asserting something the tool never promised.

2. Classification. Every key in metadata.json must appear in either
   ROUND_TRIP or BUILD_ONLY. This is the part that catches the *next*
   instance of the bug rather than only this one: adding a new metadata key
   now forces a one-line decision about whether it is supposed to reach the
   manifest, instead of leaving the answer to whatever bundle.sh happens to
   know about on the day.

3. manifestVersion is reported, never asserted fatally. It is the packaging
   toolchain's schema version, not ours, and it legitimately moves. But a
   stale packaging pin shows up here first — swap's broken artifact stamped
   0.2.0 while current is 0.3.0 — so it is worth putting in front of a human.

Usage
-----

    check-lgx-manifest.py <bundle.lgx> <metadata.json>

Exits 0 on agreement, 1 on mismatch. Under GitHub Actions it also emits
::error:: / ::notice:: annotations and appends to $GITHUB_STEP_SUMMARY.
"""

from __future__ import annotations

import json
import os
import sys
import tarfile

# Exactly nix-bundle-lgx bundle.sh's allowlist as of b49074a8. Keeping it
# identical is the point: if the pinned tool's list is shorter than this one,
# the missing key fails here instead of shipping.
ROUND_TRIP = (
    "name",
    "display_name",
    "version",
    "description",
    "author",
    "type",
    "category",
    "dependencies",
    "view",
)

# Keys that legitimately do NOT appear verbatim in manifest.json, with the
# reason, because "it's fine, trust me" is how the round trip rotted in the
# first place. Add to this list only after checking bundle.sh.
BUILD_ONLY = {
    "main": "transformed: source is a bare plugin name, the manifest holds a "
    "variant -> filename map written by `lgx create`",
    "icon": "transformed: the manifest holds the bundled icon's basename, or "
    '"" when there is no icon',
    "interface": "consumed by the module builder's codegen, not a manifest field",
    "codegen": "consumed by the module builder's codegen, not a manifest field",
    "nix": "consumed by the module builder to shape the derivation",
    "capabilities": "not part of the manifest schema (logos-package README, "
    '"Manifest schema")',
    "include": "consumed by the module builder when staging the payload",
}


def fmt(value: object) -> str:
    """Render a JSON value for humans -- unescaped, so a display_name with an
    arrow in it reads as an arrow rather than as \\u2194."""
    return json.dumps(value, ensure_ascii=False)


def emit(line: str) -> None:
    print(line, flush=True)


def annotate(level: str, message: str) -> None:
    """GitHub Actions annotation, plain text elsewhere."""
    if os.environ.get("GITHUB_ACTIONS") == "true":
        emit(f"::{level}::{message}")
    else:
        emit(f"[{level}] {message}")


def summary(lines: list[str]) -> None:
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    with open(path, "a", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")


def read_manifest(lgx_path: str) -> dict:
    """Pull manifest.json out of the bundle.

    A .lgx is a *gzipped tar* — nix-bundle-lgx writes it with
    `tarfile.open(..., 'w:gz')`. It is not a zip, despite the extension
    looking like one, and reaching for unzip here has already cost a CI round
    trip once. `unzip -Z1 some.lgx` puts its complaint on stderr and emits no
    filenames on stdout, so piping it into a grep looks like "the archive
    contains nothing matching" rather than "I cannot read this archive" — a
    check built that way passes vacuously, which is worse than no check.
    Python's tarfile raises instead, which is the behaviour we want.
    """
    try:
        tar = tarfile.open(lgx_path, "r:gz")
    except (tarfile.TarError, OSError) as exc:
        raise SystemExit(
            f"error: cannot read {lgx_path} as a gzipped tar ({exc}). "
            f"A .lgx is a gzipped tar, not a zip -- if this is a real bundle, "
            f"something upstream changed the container format."
        )

    with tar:
        try:
            member = tar.getmember("manifest.json")
        except KeyError:
            raise SystemExit(
                f"error: {lgx_path} contains no manifest.json at the tar root"
            )
        payload = tar.extractfile(member)
        if payload is None:
            raise SystemExit(f"error: manifest.json in {lgx_path} is not a file")
        return json.loads(payload.read())


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        emit(__doc__ or "")
        return 2

    lgx_path, metadata_path = argv[1], argv[2]

    manifest = read_manifest(lgx_path)
    with open(metadata_path, encoding="utf-8") as fh:
        metadata = json.load(fh)

    emit(f"manifest round trip: {os.path.basename(lgx_path)} vs {metadata_path}")
    emit("")

    failures: list[str] = []
    rows: list[tuple[str, str, str]] = []

    # 1. Round trip.
    for key in ROUND_TRIP:
        source = metadata.get(key)
        if not source:
            # bundle.sh's `if metadata.get(key)` skips falsy source values, so
            # there is no promise to check here.
            rows.append((key, "skip", "absent or empty in metadata.json"))
            continue

        if key not in manifest:
            failures.append(
                f"{key}: metadata.json has {fmt(source)}, "
                f"but the bundled manifest.json has no '{key}' key at all "
                f"(the packaging tool dropped it)"
            )
            rows.append((key, "MISSING", fmt(source)))
            continue

        if manifest[key] != source:
            failures.append(
                f"{key}: metadata.json has {fmt(source)}, "
                f"bundled manifest.json has {fmt(manifest[key])}"
            )
            rows.append(
                (key, "DIFFERS", f"{fmt(source)} != {fmt(manifest[key])}")
            )
            continue

        rows.append((key, "ok", fmt(source)))

    # 2. Classification of every source key.
    for key in metadata:
        if key in ROUND_TRIP or key in BUILD_ONLY:
            continue
        failures.append(
            f"{key}: metadata.json carries an unclassified key. Decide which "
            f"it is and say so in scripts/check-lgx-manifest.py: if it belongs "
            f"in the bundled manifest add it to ROUND_TRIP (and confirm the "
            f"pinned nix-bundle-lgx copies it -- see bundle.sh's allowlist), "
            f"otherwise add it to BUILD_ONLY with the reason. An unclassified "
            f"key is exactly how display_name went missing from swap 0.3.0."
        )
        rows.append((key, "UNCLASSIFIED", fmt(metadata[key])))

    width = max(len(k) for k, _, _ in rows)
    for key, state, detail in rows:
        emit(f"  {key.ljust(width)}  {state.ljust(12)}  {detail}")

    # 3. Report manifestVersion.
    emit("")
    manifest_version = manifest.get("manifestVersion", "<absent>")
    annotate(
        "notice",
        f"{metadata.get('name', '?')}: manifest.json stamps "
        f"manifestVersion={manifest_version}. This comes from the pinned "
        f"packaging toolchain, not from this repo -- if it looks stale, the "
        f"flake.lock packaging inputs are stale.",
    )

    table = [
        f"### `.lgx` manifest round trip -- `{metadata.get('name', '?')}`",
        "",
        f"`manifestVersion`: **{manifest_version}** "
        "(stamped by the pinned packaging toolchain)",
        "",
        "| field | result | value |",
        "| --- | --- | --- |",
    ]
    for key, state, detail in rows:
        # Pipes would break the markdown table; the value is only a hint here.
        cell = detail.replace("|", "∣")[:120]
        table.append(f"| `{key}` | {state} | <code>{cell}</code> |")
    summary(table + [""])

    if failures:
        emit("")
        for failure in failures:
            annotate("error", f"lgx manifest mismatch -- {failure}")
        emit("")
        emit(
            f"FAIL: {len(failures)} field(s) in {metadata_path} did not survive "
            f"packaging into {os.path.basename(lgx_path)}."
        )
        return 1

    emit("")
    emit(f"PASS: every round-tripping field in {metadata_path} matches the bundle.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
