#!/bin/bash
# 96E-15/96E-20/96E-34 — encoding guard.
#
# Catches three distinct defects, all of which have actually occurred here:
#
#   1. Double-encoded UTF-8 - a file saved as UTF-8 then re-decoded as Latin-1
#      and saved again. Shows up as C2/C3 followed by a continuation byte.
#   2. Windows-1252 misreads of box-drawing characters. `Set-Content -Encoding
#      UTF8` in PowerShell 5.1 turns U+2500 into U+0393 U+00F6 U+00C7, which the
#      original C2/C3-only pattern did not match - that is exactly the corruption
#      found in plugins/kn9t-policy.py (fixed in 068a3b0).
#   3. UTF-16 and UTF-8-BOM files. git treats UTF-16 as binary, so grep never
#      sees inside them: kn9t-policy.py was UTF-16LE for an unknown period and
#      no guard could read it. A BOM also breaks shebangs and `#[cfg]` parsing.
#
# Scope is the whole tree, not just crates/ docs/ spec/. The previous version
# excluded root-level .md, so CHANGELOG.md, TRACKING.md and AGENTS.md were never
# checked (96E-34).

set -e
cd "$(dirname "$0")/.."

FAILED=0

# Files this script may inspect. Skip build output, vendored deps, and the raw
# provider fixtures (captured bytes, intentionally not valid UTF-8; R-RPLY-010).
list_files() {
  git ls-files -z -- \
      '*.rs' '*.md' '*.py' '*.sh' '*.toml' '*.json' '*.ts' '*.go' '*.hook' \
    | tr '\0' '\n' \
    | grep -v '^crates/kn9t-provider-replay/fixtures/' \
    | grep -v '^scripts/check-mojibake.sh$'
}

# ── 1 + 2. Byte-level corruption ──────────────────────────────────────────────
# Matched on bytes, via grep -P, so a mis-set locale cannot change the result.
#
# Care is needed: `\xc3\xa2` alone is a legitimate 'â' (French "tâche"), and
# `\xc3\xa9` is 'é'. Flagging those produced false positives on CHANGELOG.md,
# which is partly French. Genuine double-encoding is a mojibake *lead* byte
# followed by a second mojibake byte, i.e. the UTF-8 encoding of U+00C2/U+00C3
# immediately followed by another C2/C3/E2 sequence.
#   \xc3\x82        U+00C2 - only ever appears as double-encoding
#   \xc3\x83        U+00C3 - ditto
#   \xc3\xa2\xe2\x82\xac  'â€' - the U+2014 em-dash triple-encoded
#   \xce\x93\xc3\xb6\xc3\x87  U+0393 U+00F6 U+00C7 - U+2500 read as windows-1252
BYTE_PATTERN='\xc3\x82|\xc3\x83|\xc3\xa2\xe2\x82\xac|\xce\x93\xc3\xb6\xc3\x87'

while IFS= read -r f; do
  [ -f "$f" ] || continue
  if LC_ALL=C grep -qaP "$BYTE_PATTERN" "$f" 2>/dev/null; then
    echo "MOJIBAKE: $f"
    LC_ALL=C grep -naP "$BYTE_PATTERN" "$f" 2>/dev/null | head -n 3 | sed 's/^/    /'
    FAILED=1
  fi
done < <(list_files)

# ── 3. Byte-order marks and UTF-16 ────────────────────────────────────────────
while IFS= read -r f; do
  [ -f "$f" ] || continue
  bom=$(head -c 3 "$f" | od -An -tx1 | tr -d ' \n')
  case "$bom" in
    fffe*|feff*)
      echo "UTF-16 ENCODING: $f"
      echo "    git treats this as binary; no text guard can inspect it."
      FAILED=1
      ;;
    efbbbf)
      echo "UTF-8 BOM: $f"
      echo "    breaks shebang lines and byte-exact generated-file comparison."
      FAILED=1
      ;;
  esac
done < <(list_files)

if [ "$FAILED" -eq 0 ]; then
  echo "mojibake: OK (no double-encoded sequences, no BOM, no UTF-16)"
  exit 0
fi

echo ""
echo "Fix: re-save as UTF-8 without BOM and re-type the affected characters."
echo "In PowerShell use [IO.File]::WriteAllText(path, text, (New-Object Text.UTF8Encoding \$false))"
echo "  - 'Set-Content -Encoding UTF8' writes a BOM on PowerShell 5.1."
exit 1
