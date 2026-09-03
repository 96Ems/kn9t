#!/bin/bash
# 96E-20 — CI aggregate: all mechanical guardrails in one call.
# Run this in CI (or locally) to get the same gates as pre-commit plus the
# heavier SSE race test.

set -e
cd "$(dirname "$0")/.."

echo "=== kn9t CI guardrails (96E-20) ==="
./scripts/check-gi1.sh
./scripts/check-schema.sh
./scripts/check-mojibake.sh
./scripts/check-unwrap-trend.sh
./scripts/check-sse-race.sh
echo ""
echo "All CI guardrails passed."
