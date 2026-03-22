#!/bin/bash
# Run golden label tests + broad regression gate.
# Usage: ./src/desktop/scripts/dev/run_golden_tests.sh
set -e
cd "$(dirname "$0")/../../../.."
echo "=== Golden labels ==="
cd src/desktop
cargo test -p desktopctld --test golden_labels golden_labels_per_category_recall -- --nocapture 2>&1 | grep -E "MISS|===|------|category|text_field|container|text_or_paragraph|button|icon|list|global|ok|FAIL"
echo
echo "=== Broad regression gate ==="
cargo test -p desktopctld --test tokenize_box_labels broad_grounding_labels_have_minimum_box_recall -- --nocapture 2>&1 | grep -E "metrics:|ok|FAIL"
echo
echo "=== Unit tests ==="
cargo test -p desktopctld vision::tokenize_boxes::tests -- --nocapture 2>&1 | grep -E "test |ok|FAIL"
