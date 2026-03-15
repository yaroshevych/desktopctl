#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: render-settings-regions.sh <input.png> <output.png>"
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

cargo run \
  --manifest-path "$WORKSPACE_DIR/Cargo.toml" \
  -p desktopctld \
  --example render_settings_regions \
  -- "$1" "$2"
