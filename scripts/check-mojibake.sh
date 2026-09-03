#!/bin/bash
# 96E-15/96E-20 — mojibake guard: fail on double-UTF-8 sequences that indicate
# a file was saved as UTF-8 but decoded as Latin-1 (or vice versa).
# Cheap grep, zero false-positive risk — runs in CI and pre-commit.

set -e
cd "$(dirname "$0")/.."

# Sequences observed in the 96E-15 fix (4 files had them in doc comments):
# Â§ = § double-encoded, â— = —, â€™ = ’, â€œ = “, â€ = ”/– etc.
# We grep for the raw bytes via literal UTF-8 characters.
PATTERN='Â§|â€”|â€™|â€œ|â€|Ã©|Ã¨|Ã '

if grep -rn -E "$PATTERN" --include="*.rs" --include="*.md" crates/ docs/ spec/ 2>/dev/null | grep -v "check-mojibake"; then
  echo ""
  echo "MOJIBAKE DETECTED: double-encoded UTF-8 sequences found (see above)."
  echo "Fix: save the file as UTF-8 without BOM; re-type the affected characters."
  exit 1
fi

echo "mojibake: OK (no double-encoded sequences)"
