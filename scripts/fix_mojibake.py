#!/usr/bin/env python3
"""Repair cp1252 -> UTF-8 double-encoded text (mojibake) and strip UTF-8 BOMs.

Why this exists
---------------
Writing a source file with PowerShell's `Set-Content -Encoding UTF8` (PS 5.1)
does two damaging things:

  1. it prepends a UTF-8 BOM, and
  2. it re-encodes text that was already UTF-8, so every non-ASCII character
     is decoded as cp1252 and re-encoded as UTF-8.

Example: em-dash U+2014 is `E2 80 94`. Read as cp1252 those bytes are the three
characters `â`, `€`, `"`. Re-encoded as UTF-8 that becomes
`C3 A2 E2 82 AC E2 80 9D` - eight bytes where there were three. The same
happens to `§` (`C2 A7` -> `C3 82 C2 A7`) and box-drawing `─`.

The transform is invertible: take a maximal run of characters that are all
cp1252-encodable as a single byte, encode it back to those bytes, and if the
result is valid UTF-8, that run was mojibake.

Safety
------
The round-trip is attempted per run and only applied when it succeeds, so
already-correct text is never touched:

  * `—` (U+2014) -> cp1252 byte 0x97 -> not valid UTF-8 -> left alone.
  * `§` (U+00A7) -> cp1252 byte 0xA7 -> not valid UTF-8 -> left alone.
  * `─` (U+2500) -> not cp1252-encodable at all -> not part of any run.

A file is only rewritten when something actually changed, and the result must
itself be valid UTF-8 or the file is left untouched.

Usage
-----
    python scripts/fix_mojibake.py [--check] <path> [<path> ...]

    --check   report only, exit 1 if any file needs repair (for CI/hooks)
"""

from __future__ import annotations

import sys

BOM = "\ufeff"


def _cp1252_single_byte(ch: str) -> bool:
    """True if `ch` is non-ASCII and encodes to exactly one cp1252 byte."""
    if ord(ch) < 0x80:
        return False
    try:
        return len(ch.encode("cp1252")) == 1
    except UnicodeEncodeError:
        return False


def repair(text: str) -> str:
    """Undo one round of cp1252 -> UTF-8 double-encoding, and drop a leading BOM."""
    if text.startswith(BOM):
        text = text[len(BOM) :]

    out: list[str] = []
    i = 0
    n = len(text)
    while i < n:
        if not _cp1252_single_byte(text[i]):
            out.append(text[i])
            i += 1
            continue

        # Maximal run of candidate mojibake characters.
        j = i
        while j < n and _cp1252_single_byte(text[j]):
            j += 1
        run = text[i:j]

        try:
            fixed = run.encode("cp1252").decode("utf-8")
        except (UnicodeEncodeError, UnicodeDecodeError):
            fixed = run  # not mojibake - leave exactly as-is
        out.append(fixed)
        i = j

    return "".join(out)


def process(path: str, check_only: bool) -> bool:
    """Repair one file. Returns True if the file needed (or got) a repair."""
    with open(path, "rb") as fh:
        raw = fh.read()

    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        print(f"{path}: SKIP - not valid UTF-8 ({exc})", file=sys.stderr)
        return False

    fixed = repair(text)
    if fixed == text:
        return False

    if check_only:
        print(f"{path}: needs repair")
        return True

    # Preserve the original line endings by writing with newline='' - `repair`
    # never touches \r or \n, so whatever the file had survives.
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(fixed)

    removed_bom = text.startswith(BOM)
    delta = len(text) - len(fixed) - (1 if removed_bom else 0)
    print(f"{path}: repaired (bom={removed_bom}, chars_recovered={delta})")
    return True


def main(argv: list[str]) -> int:
    args = [a for a in argv if a != "--check"]
    check_only = "--check" in argv

    if not args:
        print(__doc__, file=sys.stderr)
        return 2

    touched = 0
    for path in args:
        try:
            if process(path, check_only):
                touched += 1
        except OSError as exc:
            print(f"{path}: ERROR {exc}", file=sys.stderr)
            return 2

    if check_only and touched:
        print(f"\n{touched} file(s) need repair; run without --check to fix.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
