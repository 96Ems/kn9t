# ADR-0005: The API contract is schema-first; API.md becomes generated output

## Status

Accepted

## Date

2026-08-30

## Context

Three documents currently disagree about nearly every route:

| Aspect | API.md says | Server does | TUI's wire.rs says |
|--------|-------------|-------------|---------------------|
| `POST /session` body | `{model, title}` | `{cwd, model:{provider,id}, name}` | matches server |
| `GET /session` response | bare array `[...]` | `{"sessions":[...]}` | matches server |
| `POST /approve` body | `{id, approved:bool}` | `{id, decision:string, scope}` | sends `decision` |
| SSE event casing | PascalCase | snake_case | expects snake_case |
| Routes `/pref`, `/health`, `/stop`, `/attach` | omitted | exist | uses them |

**Root cause:** The server has **no typed request structs at all.** Routes use 10
hand-poked `body.get("field")` calls in `routes/session.rs`. There are zero `Deserialize`
structs in `routes/`, zero `#[serde(deny_unknown_fields)]` anywhere, so unknown or
mistyped fields are **silently ignored**.

Example bugs caused by this:

- TUI sends `decision: "always"` but server compares `decision == "allow"`, so "always"
  silently records as deny.
- `scope` field exists in spec but server never reads it.
- `created_at` is written as milliseconds, read as seconds → year 57668 in the TUI.

The server is authoritative (per R-TUI-012 and AGENTS.md §12), but its behavior is
undocumented and untestable. API.md is a lie, and the lie persists because nothing
checks it.

## Decision

Author **one language-neutral JSON Schema** as the single source of truth. From it,
generate:

1. **Server request/response types** — with `#[serde(deny_unknown_fields)]` so typos are
   caught at parse time.
2. **TUI's wire.rs** — matching structs.
3. **API.md** — human-readable docs.
4. **Go/Python client stubs** — for plugins like `kn9t-agents-md` (Go) and future Python
   tooling.

A **CI script** compares generated output against checked-in code and fails on drift.

### Why not `schemars`?

`schemars` generates schema **from** Rust types. We need schema **to** Rust types. The
existing Go and Python plugins could never consume a Rust-derived schema without a
language-neutral intermediate, and deriving from Rust types means the Rust code is still
the source of truth — exactly the problem.

### Precedent

TRACKING.md records: "GI-1 was violated for an unknown period because nothing checked it.
Lesson: prefer a script over an assertion." The same principle applies here — API.md
said one thing, the server did another, and nobody knew.

## Consequences

- **Zero new runtime dependencies.** Code generation runs at build time. DESIGN §15's
  dependency budget is unaffected.
- **API.md must never be hand-edited again.** It becomes generated output, checked in for
  human readability but regenerated on schema change.
- **Server routes get typed request structs.** `POST /approve` becomes
  `#[derive(Deserialize)] struct ApproveReq { id: u64, decision: String, scope: Option<Scope> }`
  with `#[serde(deny_unknown_fields)]`.
- **CI catches drift.** A route change without a schema change fails; a schema change
  without regenerating API.md fails.
- **Existing clients break loudly, not silently.** The TUI sending `approved: true` instead
  of `decision: "allow"` is a 400, not a silent deny.
