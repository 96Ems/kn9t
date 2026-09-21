#!/bin/bash
# Contract guard: the schema and the code must describe the same primitives,
# in both directions.
#
# `scripts/check-schema.sh` only proves schema -> generated agreement. A
# primitive added straight to a `match` arm therefore leaves `API.md` silently
# incomplete while every gate stays green — which is how nine host-API ops, and
# the `GET /policy` and `POST /plugin/{name}/ui_event` routes, went undocumented.
#
# This is the reverse check: every op, route and hook declared in the schema must
# exist in the code, and every one in the code must be declared in the schema.
# Parsing only (no cargo), so it is cheap enough to run on every push.

set -e
cd "$(dirname "$0")/.."

# Find a Python that actually runs. `command -v python` is not enough on Windows:
# the Microsoft Store ships a `python` shim on PATH that exists, exits non-zero and
# prints "Python introuvable" — which read as a guard failure and blocked the push.
resolve_python() {
  for candidate in "${PYTHON:-}" python3 python py; do
    [ -n "$candidate" ] || continue
    if "$candidate" --version >/dev/null 2>&1; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  # Usual install locations, which are not always on bash's PATH even when the
  # PowerShell side finds them.
  for exe in "$HOME"/AppData/Local/Programs/Python/Python3*/python.exe \
             /c/Python3*/python.exe \
             "/c/Program Files/Python3"*/python.exe; do
    [ -x "$exe" ] || continue
    if "$exe" --version >/dev/null 2>&1; then
      printf '%s' "$exe"
      return 0
    fi
  done
  return 1
}

if ! PYTHON="$(resolve_python)"; then
  # Exit 2 ("cannot check"), never 1 ("invariant broken"), so a missing
  # interpreter is never mistaken for a contract violation.
  echo "contract: SKIPPED (no working python interpreter; set PYTHON=...)" >&2
  exit 2
fi

exec "$PYTHON" scripts/check_contract.py
