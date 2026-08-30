# ADR-0002: Plugins declare argument EFFECTS; the server decides risk

## Status

Accepted

## Date

2026-08-30

## Context

With tools moved to external subprocess plugins (stage 08b), the server can no longer
introspect tool arguments directly — it sees an opaque JSON blob like
`{"tool":"x","args":{...}}`. The `Policy::check` trait takes `&ToolCall` (name + args),
but without knowing which argument is a shell command vs. a file path vs. a network URL,
the server cannot run the appropriate checker.

Two non-solutions:

1. **The plugin self-reports "needs_approval".** A careless plugin marks everything safe.
   A malicious plugin marks `rm -rf /` as safe. Self-approval is never acceptable.

2. **The server dry-runs the tool.** See ADR-0003 — this is rejected as a safety input
   because determining blast radius is equivalent to executing, and TOCTOU means the
   dry-run result is still a claim by the plugin.

The server needs structured metadata about what *kind* of side-effect each argument
represents, so it can dispatch to the correct checker without trusting the plugin's
judgment.

## Decision

**ToolSpec gains an `effects` field:** a list of `{field, kind}` where:

- `field` is a JSON pointer into `args` (e.g., `/command`, `/path`).
- `kind` is one of: `Shell`, `FsWrite`, `FsRead`, `Network`.

The server maps each kind to a checker:

| Kind | Checker |
|------|---------|
| `Shell` | The restored bash classifier (R-TOOL-080/090) |
| `FsWrite` | Path allowlist rules from `[policy.paths]` |
| `FsRead` | Path allowlist rules (less strict than write) |
| `Network` | Host/URL allowlist rules |

A tool declaring **no** effects falls to the strictest `[policy] mode` default:

- `mode = "ask_on_mutation"` → ask on every call.
- `mode = "deny_all"` → hard deny.

This makes lying **unprofitable**. A plugin can only *widen* what the server inspects,
never narrow it. Omitting effects triggers the strictest path, so a plugin that wants to
run unattended must declare its effects truthfully.

**Protocol change:** This is a breaking change to the plugin handshake (`hello` message).
We do it now while:

- `proto == 1` (no versioning burden).
- There are no external plugin authors (only internal-plugins in the repo).

## Consequences

- `ToolSpec` gains `effects: Vec<Effect>` (optional, defaults to empty = strict).
- The plugin SDK's `ToolBuilder` gains `.effect(field, kind)`.
- `kn9t-tools` must declare effects on `bash` (Shell), `edit`/`write` (FsWrite), `read`
  (FsRead).
- The server's `Policy` impl reads effects from `ToolSpec`, extracts the relevant args,
  and runs each through its checker.
- DESIGN §10 already says `write` and `edit` are "gated exactly as hard as bash — a model
  rewriting `~/.ssh/authorized_keys` needs no shell." Effects make that gateable.
- No new runtime dependencies (DESIGN §15 budget unaffected).
