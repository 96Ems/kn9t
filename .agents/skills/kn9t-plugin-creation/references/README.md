<!-- GENERATED FILE — do not edit by hand. -->
<!-- Regenerate with: cargo run -p xtask -- generate -->

# references

Generated snapshots of the contract. Do not edit anything in this directory: an edit is
caught by `xtask --check` (pre-commit + CI) and overwritten by the next `generate`.

| file | source of truth |
|---|---|
| `api.md` | `schema/http.json` + `schema/plugin.json`, rendered by `xtask` |
| `sdk/` | `crates/kn9t-plugin-sdk` — the Rust SDK, byte-for-byte |
| `sdk/kn9t-macros/` | `crates/kn9t-macros`, bundled so `sdk/` builds standalone |

`sdk/` is self-contained: point a `[dependencies]` entry at `sdk/` and it compiles with
no checkout of the kn9t repo. Workspace-inherited keys are resolved and the
`kn9t-macros` path dependency is rewritten to the bundled copy.

`SKILL.md` is hand-written and sits next to this directory.
