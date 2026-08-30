#!/bin/bash
# Schema drift gate: every generated file must be byte-identical to what the
# schema produces right now. Fails on drift — unlike the pre-Phase-2 status quo
# where api.rs / wire.rs / API.md were hand-maintained three ways and silently
# disagreed (F6).
# Modeled on scripts/check-gi1.sh (see its awk range bug comment); this file must
# never pass vacuously.
#
# Lesson (TRACKING.md:31-35): "the invariant claim was untrue for an unknown period
# because nothing checked it. Prefer a script over an assertion."

set -e
cd "$(dirname "$0")/.."

echo "== schema drift check =="

# 1. Re-derive every generated output in memory and compare byte-for-byte against
#    the committed files. `xtask --check` refuses to write anything to the tree, so
#    a failure cannot leave half-regenerated files behind.
if ! cargo run -p xtask -- --check > /tmp/xtask-check.log 2>&1; then
  cat /tmp/xtask-check.log
  echo ""
  echo "SCHEMA DRIFT: generated files differ from committed output."
  echo "Run: cargo run -p xtask -- generate   (then commit the regenerated files)"
  exit 1
fi

# 2. GI-6 still holds after generation: kn9t-tui must not depend on any kn9t-* crate.
#    NOTE: the old form `grep -q 'kn9t-' file | grep -q 'path'` silently no-oped —
#    the second grep read empty stdin and always failed, so the branch never fired.
#    An explicit anchored pattern test replaces it.
if grep -nE '^[[:space:]]*kn9t-' crates/kn9t-tui/Cargo.toml; then
  echo "GI-6 VIOLATION: kn9t-tui/Cargo.toml contains a kn9t-* dependency"
  exit 1
fi

echo "schema: OK (no drift, GI-6 holds)"