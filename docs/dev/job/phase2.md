# PHASE 2 — schema-first API contract

**Goal:** one machine-readable contract; generated types, docs, and client stubs; CI fails on
drift. No client can silently disagree with the server again.
**Depends on:** nothing (independent of Phase 1). **Blocks:** Phases 3 and 4 — every *new*
endpoint should be born schema-conformant.
**Read first:** `job/findings.md` F5, F6, F7, F11; `docs/adr/0005`; AGENTS.md §11 + §12.

---

## Root cause to keep in mind

The server has **no typed request structs at all**: 10 hand-poked `body.get()` calls in
`routes/session.rs`, zero `Deserialize` in `routes/`, zero `deny_unknown_fields` workspace-wide.
Unknown/mistyped fields are **silently ignored** — that is *why* F4 and F7 fail quietly rather
than erroring.

---

## Step 2.1 — Author the schema

One language-neutral JSON Schema in-repo (suggest `schema/` at repo root) covering **both**:

1. **The HTTP API** — every route in `crates/kn9t-server/src/router.rs:130-198` plus the two SSE
   endpoints. Full route list with actual current shapes is in `job/findings.md` F6.
2. **The plugin protocol** — `HostMsg` / `PluginMsg`, `ToolSpec`, `ProviderDecl`, `Usage`, the 8
   hooks, and the `#[serde(flatten)]` body quirk. This matters as much as HTTP:
   `plugins/kn9t-agents-md/main.go:44` carries a hand-written comment warning about the flattened
   body — a contract that should be generated, not commented.

**Server is authoritative** (R-TUI-012) **with two exceptions where the server is wrong:**
- **SSE casing** — server emits snake_case (`src/sse.rs:38-47`), API.md documents PascalCase.
  AGENTS.md §12 mandates snake_case → **API.md is wrong.**
- **`created_at`** — see step 2.4.

Pin down the `approve` shape decided in Phase 1 step 1.5 (`{id, decision, scope}`).

---

## Step 2.2 — Build the generator (`xtask`)

No new **runtime** deps (DESIGN §15 budget intact — dev/tool deps in an `xtask` crate are outside
the runtime budget, but say so explicitly in the changelog).

Generates:
| target | notes |
|---|---|
| server request/response types | **`#[serde(deny_unknown_fields)]`** → a mistyped field is a **400, not a silent ignore** |
| `crates/kn9t-tui/src/wire.rs` | **GI-6 must survive** — generated types must still have no `kn9t-*` dep. Emit a standalone file. |
| `API.md` | becomes generated output, never hand-edited again |
| Go stub | for `plugins/kn9t-agents-md` |
| Python stub | for `plugins/kn9t-mcp` |

Replace the `body.get()` poking in `routes/session.rs` with the generated types.

**Watch out:** there is no `build.rs` and no `xtask` in the repo today — you are establishing a
precedent. Keep it a plain workspace member invoked as `cargo run -p xtask -- generate`.

---

## Step 2.3 — CI drift gate

`scripts/check-schema.sh`, modeled on the existing `scripts/check-gi1.sh` (read it — it is the
precedent, and note its own embedded comment about an awk range bug that made an earlier check
vacuously pass; do not repeat that mistake).

- Regenerate into a temp dir, diff against committed output, fail on difference.
- Add to the pre-commit hook alongside `check-gi1.sh`.
- **Also assert GI-6 still holds after generation.**

Cite the recorded lesson (`TRACKING.md:31-35`): *"the invariant claim was untrue for an unknown
period because nothing checked it. Prefer a script over an assertion."*

---

## Step 2.4 — Reconcile the three-way drift

Fix the two live bugs:

**`created_at` (F5).** Store writes `as_millis()`
(`crates/kn9t-store/src/session.rs:10-12`); TUI's `visit_i64` treats it as **seconds**
(`crates/kn9t-tui/src/wire.rs:194`) → session dates render as **year 57668**. Consumed at
`ui/render.rs:1226`.
→ Pin **one** format in the schema. Server normalizes at the boundary
(`routes/session.rs:74,93` pass the raw INTEGER through today). Then **delete** the entire
65-line dual-format visitor (`wire.rs:164-228`), including its wrong `days/365` and
`remaining_days/30` calendar math.

**`CreateSessionReq.model` (F7).** TUI declares `Option<String>`; server requires
`{provider, id}`. Generated types make this impossible.

Rewrite API.md's wrong rows from the generated source — full list in `job/findings.md` F6.

---

## Step 2.5 — Delete the fallback chains (AGENTS.md §10 violations)

- `crates/kn9t-tui/src/message_handler.rs:394-412` —
  `get("args_json").or_else(|| get("args")).or_else(|| get("input"))`. With one generated format
  all three collapse to one field. §10 names field-name fallbacks explicitly.
- `message_handler.rs:370` — `block.get("type")…unwrap_or("")`. Server should validate instead.
- The timestamp visitor (step 2.4).

---

## Also worth fixing here (cheap, same area)

- **F12** — 46 `eprintln!` in `crates/kn9t-server/src`; `prompt()` logs on every request.
  `crate::log!` already exists (`src/log.rs`). Convert them.
- Repo hygiene: `kn9t-tui.log` (726 KB), `custom-provider.log` (29 MB), `plugins/kn9t-custom-provider/custom-provider.log` in the
  tree; check `.gitignore`.

---

## Note for the schema design (from F11)

A `pub` field whose type has no `pub` constructor is a **compile-time-invisible contract break** —
that is how `KvClient` broke every external plugin's tests. The schema/codegen work should make
this class of break visible, and the Go/Python stubs are the real test of that: they are the
consumers a Rust-reflection approach could never have served.

---

## Phase 2 exit criteria

- [ ] `cargo run -p xtask -- generate` then `git diff --exit-code` is clean
- [ ] `scripts/check-schema.sh` in the pre-commit hook, and it actually fails on a deliberate drift
- [ ] an unknown field on any POST returns **400**, verified by test
- [ ] `created_at` renders a correct date in the TUI session list
- [ ] `wire.rs` timestamp visitor and the `args_json/args/input` chain are **deleted**
- [ ] GI-6 still passes (`tui_no_kn9t_deps`)
- [ ] API.md regenerated and no longer contradicts the server on any route in F6
