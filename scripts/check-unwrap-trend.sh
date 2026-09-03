#!/bin/bash
# 96E-18/96E-20 — unwrap/expect trend guard.
#
# Counts bare `.unwrap()` outside `#[cfg(test)]` and reports the total.
# In CI we compare against the committed baseline on `main` (or a stored
# baseline file) and fail if the count increases in security-critical files.
# Locally (no baseline) we just report and warn on increase vs last commit.
#
# Security-critical files: policy.rs, host.rs (approval + plugin bus).
# The 96E-18 fix drove policy.rs to 0 bare unwraps in non-test code —
# this script prevents silent regrowth.

set -e
cd "$(dirname "$0")/.."

# Count bare .unwrap() in non-test code (exclude lines inside #[cfg(test)] modules
# and test files). We use a simple heuristic: strip everything from #[cfg(test)]
# onward in each file, then grep. Good enough for a trend signal.

count_file() {
  local file="$1"
  local c
  c=$(awk 'BEGIN{p=1} /#\[cfg\(test\)/{p=0} p' "$file" 2>/dev/null | grep -c "\.unwrap()" 2>/dev/null || true)
  # grep -c outputs even with no matches; ensure numeric
  if ! echo "$c" | grep -qE '^[0-9]+$'; then c=0; fi
  echo "$c"
}

CRITICAL="crates/kn9t-server/src/policy.rs crates/kn9t-plugin/src/host.rs"
TOTAL=0
FAILED=0

echo "== unwrap trend check (non-test, bare .unwrap() only) =="

for f in $CRITICAL; do
  if [ -f "$f" ]; then
    c=$(count_file "$f")
    TOTAL=$((TOTAL + c))
    echo "  $f: $c bare .unwrap() (non-test)"
    if [ "$c" -gt 0 ]; then
      echo "    WARN: $f has bare .unwrap() in non-test code — prefer .expect(\"reason\")"
      # Fail only if critical file regresses (has any bare unwrap)
      # 96E-20 criterion says "even if just a warning initially" — we warn, but also
      # enforce for policy.rs which was fixed to 0.
      if [ "$f" = "crates/kn9t-server/src/policy.rs" ]; then
        echo "    FAIL: policy.rs must have 0 bare .unwrap() in non-test (96E-18)"
        FAILED=1
      fi
    fi
  fi
done

# Overall src count (informational, not failing)
OVERALL=$(grep -rn "\.unwrap()" --include="*.rs" crates/ 2>/dev/null | grep -v "/tests/" | wc -l | tr -d ' ' || true)
echo "  overall crates/ (excl. tests/): $OVERALL .unwrap() occurrences"
echo "  critical total (policy.rs + host.rs non-test): $TOTAL"

# Compare against baseline if we have a main to diff against
if git rev-parse --verify main >/dev/null 2>&1; then
  BASELINE=0
  for f in $CRITICAL; do
    if git show main:"$f" >/tmp/unwrap-baseline 2>/dev/null; then
      c=$(awk 'BEGIN{p=1} /#\[cfg\(test\)/{p=0} p' /tmp/unwrap-baseline 2>/dev/null | grep -c "\.unwrap()" || true)
      BASELINE=$((BASELINE + c))
    fi
  done
  if [ "$TOTAL" -gt "$BASELINE" ]; then
    echo ""
    echo "UNWRAP TREND: critical count $TOTAL > baseline $BASELINE (main) — regression"
    echo "Fix: replace .unwrap() with .expect(\"reason\") or handle the error."
    # Warning mode for now except policy.rs which already fails above
    if [ "$FAILED" -eq 0 ]; then
      echo "(warning only — not failing CI yet, per 96E-20)"
    fi
  else
    echo "  trend vs main ($BASELINE): OK (no increase)"
  fi
fi

if [ "$FAILED" -ne 0 ]; then
  exit 1
fi

echo "unwrap trend: OK"
