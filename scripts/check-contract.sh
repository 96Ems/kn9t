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

PYTHON="${PYTHON:-}"
if [ -z "$PYTHON" ]; then
  for candidate in python3 python py; do
    if command -v "$candidate" >/dev/null 2>&1; then
      PYTHON="$candidate"
      break
    fi
  done
fi

if [ -z "$PYTHON" ]; then
  # Exit 2 ("cannot check"), never 1 ("invariant broken"), so a missing
  # interpreter is never mistaken for a contract violation.
  echo "contract: SKIPPED (no python interpreter found; set PYTHON=...)" >&2
  exit 2
fi

exec "$PYTHON" scripts/check_contract.py
