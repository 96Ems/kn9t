#!/bin/bash
# SSE attach race regression test wiring.
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
# shellcheck source=scripts/_cargo.sh
. "$(dirname "$0")/_cargo.sh"

echo "== SSE race regression test wiring =="

# 1. The hook env var must still be honored in the store
if ! grep -rq "KN9T_SSE_TEST_DELAY_MS" crates --include="*.rs"; then
  echo "FAIL: KN9T_SSE_TEST_DELAY_MS hook not found in crates/"
  exit 1
fi
echo "  hook KN9T_SSE_TEST_DELAY_MS: present"

# 2. The regression test must exist and be runnable (not ignored)
if ! grep -rq "attach_does_not_lose_interleaved_event" crates --include="*.rs"; then
  echo "FAIL: attach_does_not_lose_interleaved_event* test not found"
  exit 1
fi
echo "  test attach_does_not_lose_interleaved_event: present"

# 3. Actually run it (fast — single test). Run ONCE and reuse the output: the
#    previous version invoked `cargo test` twice, doubling the cost and making
#    the pass/fail verdict depend on a second, independent run.
echo "  running cargo test attach_does_not_lose..."
# The test lives in `tests/unit_sse.rs` — an integration target, not the lib.
out="$("$CARGO" test -p kn9t-server --test unit_sse attach_does_not_lose 2>&1)" || true
echo "$out" | tail -n 20

if echo "$out" | grep -Eq "[1-9][0-9]* passed"; then
  if echo "$out" | grep -Eq "[1-9][0-9]* ignored"; then
    echo "FAIL: SSE race test is #[ignore]d — it must actually run"
    exit 1
  fi
  echo "SSE race: OK (test exercised and passing)"
else
  echo "FAIL: SSE race test did not report passed"
  exit 1
fi
