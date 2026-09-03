#!/bin/bash
# 96E-7/96E-20 — SSE attach race regression test wiring.
#
# The attach race (subscribe→read→dedup) had a two-query gap where a durable
# event committed between the two reads was lost. The fix made the read atomic
# via read_attach_snapshot (single txn). The regression is caught by the
# deterministic test that uses KN9T_SSE_TEST_DELAY_MS to widen the window.
#
# This script confirms the test is not just manually triggerable but actually
# exercised by `cargo test` (i.e., not #[ignore] and not missing).

set -e
cd "$(dirname "$0")/.."

echo "== SSE race regression test wiring =="

# 1. The hook env var must still be honored in the store
if ! grep -rq "KN9T_SSE_TEST_DELAY_MS" crates --include="*.rs"; then
  echo "FAIL: KN9T_SSE_TEST_DELAY_MS hook not found in crates/"
  exit 1
fi
echo "  hook KN9T_SSE_TEST_DELAY_MS: present"

# 2. The regression test must exist and be runnable (not ignored)
if ! grep -rq "p1_96e7_attach_does_not_lose" crates --include="*.rs"; then
  echo "FAIL: p1_96e7_attach_does_not_lose* test not found"
  exit 1
fi
echo "  test p1_96e7_attach*: present"

# 3. Actually run it (fast — single test)
echo "  running cargo test p1_96e7..."
if ! cargo test -p kn9t-server --lib p1_96e7 --quiet 2>&1 | tail -n 20; then
  echo "  cargo test p1_96e7 finished (see above)"
fi

# Verify it passed (not ignored)
if cargo test -p kn9t-server --lib p1_96e7 2>&1 | grep -Eq "1 passed|2 passed"; then
  echo "SSE race: OK (test exercised and passing)"
else
  echo "FAIL: SSE race test did not report passed"
  exit 1
fi
