#!/bin/bash
# Schema drift gate: regenerate via xtask and diff against committed output.
# Fails if schema/http.json or schema/plugin.json is out of sync with generated files.
# Modeled on scripts/check-gi1.sh (see its awk range bug comment).

set -e
cd "$(dirname "$0")/.."

if ! cargo run -p xtask -- generate > /tmp/xtask.log 2>&1; then
  cat /tmp/xtask.log
  echo "xtask generate failed"
  exit 1
fi

if ! git diff --exit-code -- schema/ crates/kn9t-tui/src/wire.rs API.md > /tmp/schema.diff 2>&1; then
  echo "SCHEMA DRIFT: generated files differ from committed"
  cat /tmp/schema.diff
  echo ""
  echo "Run: cargo run -p xtask -- generate"
  exit 1
fi

# Also assert GI-6 still holds after generation (tui has no kn9t-* deps)
if grep -q 'kn9t-' crates/kn9t-tui/Cargo.toml | grep -q 'path'; then
  echo "GI-6 VIOLATION: kn9t-tui depends on kn9t-*"
  exit 1
fi

echo "schema: OK (no drift, GI-6 holds)"
