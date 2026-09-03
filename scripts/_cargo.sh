#!/bin/bash
# Shared cargo resolution for the guard scripts. Source it, then use "$CARGO".
#
# AGENTS.md 8.1: on Windows the toolchain lives on the Windows side, and these
# scripts are typically run from WSL or git-bash where `cargo` is not on PATH
# (WSL has its own filesystem, so ~/.cargo does not exist there). Without this,
# check-schema.sh and check-sse-race.sh fail with "cargo: command not found" -
# which reads as a gate failure rather than a missing tool (96E-29).
#
# Resolution order:
#   1. $CARGO if the caller already set it
#   2. cargo on PATH (native Linux/macOS, or git-bash with cargo installed)
#   3. cargo.exe on PATH (git-bash / WSL with the Windows toolchain exposed)
#   4. the usual Windows install location under the WSL /mnt mount
# Exits 2 (tool missing) rather than 1 (gate failed) so callers can tell the
# difference between "the invariant is broken" and "I cannot check it".

if [ -n "$CARGO" ] && command -v "$CARGO" >/dev/null 2>&1; then
  :
elif command -v cargo >/dev/null 2>&1; then
  CARGO=cargo
elif command -v cargo.exe >/dev/null 2>&1; then
  CARGO=cargo.exe
else
  CARGO=""
  for base in /mnt/c/Users /c/Users; do
    [ -d "$base" ] || continue
    for candidate in "$base"/*/.cargo/bin/cargo.exe; do
      if [ -x "$candidate" ]; then
        CARGO="$candidate"
        break 2
      fi
    done
  done
  if [ -z "$CARGO" ]; then
    echo "SKIP: cargo not found (PATH, cargo.exe, or */.cargo/bin/cargo.exe)."
    echo "      Set CARGO=/path/to/cargo, or run this gate where the toolchain lives."
    exit 2
  fi
fi

export CARGO
