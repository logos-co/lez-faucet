#!/usr/bin/env bash

# Compatibility setup for logos-scaffold 0.1.1 and the LEZ v0.2.0 source tree.
# Scaffold still looks for wallet/ and sequencer/ at the repository root, while
# v0.2.0 keeps both under lez/. The links live only in Scaffold's cached clone.

set -euo pipefail

project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$project_dir"

scaffold_bin=${LOGOS_SCAFFOLD_BIN:-logos-scaffold}
lez_pin=$(sed -n '/^\[repos\.lez\]/,/^\[/p' scaffold.toml \
  | sed -n 's/^pin = "\([0-9a-f]\{40\}\)".*/\1/p' \
  | head -n 1)

if [[ -z "$lez_pin" ]]; then
  echo "scaffold-setup: could not read the LEZ pin from scaffold.toml" >&2
  exit 1
fi

lez_checkout=".scaffold/lez-cache/repos/lez/$lez_pin"

add_layout_links() {
  local component source_path target_path

  [[ -d "$lez_checkout" ]] || return 0

  for component in wallet sequencer; do
    source_path="$lez_checkout/lez/$component"
    target_path="$lez_checkout/$component"

    if [[ -d "$source_path" && ! -e "$target_path" && ! -L "$target_path" ]]; then
      ln -s "lez/$component" "$target_path"
      echo "scaffold-setup: added $target_path -> lez/$component"
    fi
  done
}

# Reuse links from a prior partial setup when the cached checkout already exists.
add_layout_links

if "$scaffold_bin" setup; then
  exit 0
fi

# A first run normally clones LEZ before failing on the old root-level paths.
echo "scaffold-setup: initial setup did not complete; applying the v0.2.0 layout compatibility links" >&2
add_layout_links

if [[ ! -L "$lez_checkout/wallet" || ! -L "$lez_checkout/sequencer" ]]; then
  echo "scaffold-setup: LEZ checkout is missing the expected lez/wallet or lez/sequencer directories" >&2
  exit 1
fi

exec "$scaffold_bin" setup
