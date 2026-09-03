#!/bin/bash
# GI-1 enforcement: no crate except kn9t-server may have >1 workspace dependency.
# Run this before committing to catch violations early.

set -e
cd "$(dirname "$0")/.."

FAILED=0

# GI-1 constrains [dependencies] - what a crate links at build time. Test-only
# siblings in [dev-dependencies] are OUT of scope: they cannot create a runtime
# coupling or a dependency cycle in the shipped artifact.
#
# That scope was previously implicit, so three crates named two siblings each
# while the gate passed green and nobody could tell whether it was an accepted
# exception or an escape hatch (96E-32). The script now reports every dev-dep it
# skips, so the exception is visible instead of invisible.
echo "GI-1 scope: [dependencies] only; [dev-dependencies] reported but not enforced."
echo ""

# External plugins (plugins/*) are standalone crates outside the workspace, but
# GI-1 still applies: they may depend on kn9t-plugin-sdk and nothing else kn9t-*.
for toml in crates/*/Cargo.toml crates/internal-plugins/*/Cargo.toml plugins/*/Cargo.toml; do
    [ -f "$toml" ] || continue
    
    crate=$(basename "$(dirname "$toml")")
    
    # kn9t-server is the documented exception (DESIGN §2, spec/06-server.md)
    [[ "$crate" == "kn9t-server" ]] && continue
    
    # Count workspace deps in [dependencies] section only (not [dev-dependencies]).
    # NOTE: the range form /^\[dependencies\]/,/^\[/ is wrong — it terminates on the
    # very line it starts, yielding an empty section and a vacuously passing check.
    deps_section=$(awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f' "$toml" 2>/dev/null || true)
    count=$(echo "$deps_section" | grep -cE 'path = "(\.\./)+(crates/)?kn9t-' || true)
    count=${count:-0}
    
    if [ "$count" -gt 1 ]; then
        echo "GI-1 VIOLATION: $crate has $count workspace deps (max 1)"
        echo "  File: $toml"
        echo "$deps_section" | grep -E 'path = "(\.\./)+(crates/)?kn9t-' | sed 's/^/    /'
        FAILED=1
    fi

    # Report (do not fail on) test-only siblings, so the documented exception is
    # auditable from the gate's own output.
    dev_section=$(awk '/^\[dev-dependencies\]/{f=1;next} /^\[/{f=0} f' "$toml" 2>/dev/null || true)
    dev_list=$(echo "$dev_section" | grep -oE '^[[:space:]]*kn9t-[a-z-]+' | tr -d ' ' | tr '\n' ' ' || true)
    if [ -n "$dev_list" ]; then
        echo "  note: $crate dev-deps (not enforced): $dev_list"
    fi
done

if [ "$FAILED" -eq 0 ]; then
    echo "GI-1: OK (all crates have ≤1 workspace dependency)"
    exit 0
else
    echo ""
    echo "Fix: Use re-exports from the parent crate instead of direct dependencies."
    echo "See kn9t-provider-core/src/lib.rs for the re-export pattern."
    exit 1
fi
