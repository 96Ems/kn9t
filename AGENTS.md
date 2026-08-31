# AGENTS.md — kn9t

Operating guide for any agent (or human) working on **kn9t**, a minimal, modular coding
agent written in Rust. Read this file first, every session.

---

## 1. What this project is

kn9t is a from-scratch coding agent. Design goals, in priority order:

1. **Minimal** — a dependency budget, not fewer features (DESIGN Principle 5).
2. **Modular & composable** — one vocabulary crate everyone depends on, no crate names a
   sibling (except the server).
3. **Rust, OS threads, no async** — no tokio, no `.await`, blocking I/O throughout.
4. **Events are the wire, the log, and the truth** — one `Event` enum is the SSE payload,
   the SQLite row, and the input to state reconstruction.

The project is currently **design + spec complete, zero code written.** Your job across
sessions is to implement it, stage by stage, following the spec exactly.

---

## 2. The documents (read in this order)

| doc | what it is | when to read |
|---|---|---|
| `AGENTS.md` (this) | repo guidelines — how to proceed, rules, invariants, gates | every session, first |
| `TRACKING.md` | live status — stage progress, per-requirement test status, SPEC-OPEN register | every session, second — it tells you where you are |
| `CHANGELOG.md` | session narrative + discovered spec/design bugs | every session; append as you work |
| `DESIGN.md` | the *why* — decisions, rejected alternatives, accepted costs (§1–18) | when a spec requirement is unclear; it is the rationale |
| `spec/README.md` | spec conventions — ID scheme, keywords, global invariants, SPEC-OPEN register | before touching any stage |
| `spec/NN-*.md` | the *what* and *how* — per-stage requirements with signatures, DDL, wire schemas, acceptance tests | when implementing that stage |

`AGENTS.md` is the standing rulebook — it changes rarely. `TRACKING.md` is the mutable
scoreboard — it changes every session. Keep the two separate; do not put status tables in
this file.

**Rule of precedence:** if the spec and the design disagree, the design's *decisions* win
and the spec is the bug — stop and flag it. If the design is silent, the spec is
authoritative.

Do **not** read the whole design or spec into context every time. Read `TRACKING.md` to
find the current stage, then read only that stage's spec file plus `spec/README.md`.

---

## 3. Build order — never deviate

The spec files are numbered by DESIGN §16 build order. Each stage's acceptance gate
depends on the previous stage existing. Build strictly 01 → 10.

```
01 kn9t-core ............ types, Event, bus, all traits, breakpoints()
02 kn9t-provider-replay . raw-byte fixtures through the real parser   [enables offline tests]
03 kn9t-react + tools ... loop, cancel/abort, read/write/edit/bash    [GATE G1]
04 kn9t-store ........... SQLite schema, projections, reproject       [GATE G2]
05 provider-core+openai . http/sse/assemble/retry, openai, litellm gateway
06 kn9t-server .......... http surface, SSE, leases, auth, spawn
07 kn9t-tui ............. ratatui client, links no workspace crate    [GATE G3]
08 kn9t-plugin .......... stdio host, 8 hooks, subagent spawn
09 kn9t-custom-provider + anthropic  external custom plugin (6 hazards), anthropic
10 bedrock-native + gemini  SigV4/eventstream, gemini               [v2 — not in v1 gates]
```

Gates G1/G2/G3 are the three §16 checkpoints; they are hard stops (§7 below).

---

## 4. How to implement a stage

For each stage `NN`:

1. **Open `TRACKING.md`** — confirm the previous stage's gate is green. If not, finish
   that first. Never start a stage on top of a red gate.
2. **Read `spec/NN-*.md` in full** plus `spec/README.md` (global invariants GI-1…GI-6 apply
   to every stage and are not restated per requirement).
3. **Create the crate(s)** for that stage in a Cargo workspace at the repo root
   (`kn9t/Cargo.toml` as `[workspace]`; crates under `kn9t/crates/`).
4. **Implement requirement by requirement, in ID order.** Every requirement is
   `R-<AREA>-<NNN>`. A requirement stated as a signature / DDL / wire schema is **MUST**:
   match it exactly.
5. **Write the acceptance test named in each requirement** (`**Accept:** cargo test
   <name>`). The test name in the spec is the test you write. A requirement with no passing
   acceptance test is **not done**.
6. **Run the stage gate** (`R-<AREA>-900`). It lists the exact conditions for "done".
7. **Update `TRACKING.md`** (§6 below) — flip requirement/test statuses, record any
   SPEC-OPEN resolution.
8. **Record the session** in `CHANGELOG.md` — narrative of what changed, plus any
   discovered spec/design bug.

### Keywords (from `spec/README.md` §3)
- **MUST / MUST NOT** — absolute; violation blocks the gate.
- **SHOULD** — strong default; deviation needs a recorded reason in `CHANGELOG.md`.
- **MAY** — optional.

### Global invariants — check these every stage (CI should enforce)
- **GI-1** no crate except `kn9t-server` has >1 workspace dependency.
- **GI-2** `kn9t-core` depends only on `serde`/`serde_json`; event payloads are pure data.
- **GI-3** no `HashMap` is ever serialized into a request/cached prefix; `preserve_order` off.
- **GI-4** `events` table is append-only; only `live_messages` is mutable-in-place.
- **GI-5** no `tokio`, no `async fn`, no `.await` anywhere.
- **GI-6** `kn9t-tui` does not depend on `kn9t-core` (HTTP + SSE only).

---

## 5. Where status lives

**All live status is in `TRACKING.md`** — the stage tracker, per-requirement test tables,
and the SPEC-OPEN resolution register. This file (`AGENTS.md`) holds no status and must not
grow any; it is the standing rulebook only.

**v1 release = stages 01–09 gates green.** Stage 10 is v2 and does not gate v1.

---

## 6. Bookkeeping discipline — do this every session

Two files carry across sessions; keep both current **as you work**, not at the end.

**`TRACKING.md` — the scoreboard.** At session start, read it to find the current stage and
last gate status. As you work:
- flip each requirement's test status (`—` → `▣`/`✗`/`☑`) as its acceptance test is written
  and run. A requirement is `☑` only when its named test passes.
- update the stage's row in the overall progress table (`reqs done / total`, gate status).
- when you resolve a **SPEC-OPEN**, fill its row in the register with the chosen value and
  date, **and** update the interim value in the spec file + `spec/README.md`'s SPEC-OPEN
  table.

**`CHANGELOG.md` — the narrative.** It is the memory of *why* things changed; it is not a
git log and not a status table. Under a dated session heading, append:
- what you implemented/changed this session, by stage and requirement ID.
- any **deviation** from a SHOULD, with reason.
- any **design/spec bug** found (spec contradicts design, or a MUST is unimplementable) in
  the "Discovered bugs" table. Do not silently work around it — record and flag it.
- a one-line **"next session starts here"** pointer at the top.

### Marking a spec requirement done
A requirement is done **only** when its acceptance test passes. "Implemented but untested"
is **not** done — record it as `▣ in progress` with the test `not-written` or `failing`.
Never mark a gate green unless every MUST in the stage has a passing acceptance test.

---

## 7. Gates are hard stops

`G1`, `G2`, `G3` (and every `R-*-900`) are checkpoints, not suggestions.

- **G1** (end of 03): the full ReAct loop runs end-to-end against the replay provider with
  **no network and no spend**, executing a tool call and a compaction re-plan.
- **G2** (end of 04): **kill -9 between turns**, reload, state reconstructs exactly; and
  **`reproject --check` reports zero diffs**.
- **G3** (end of 07): **3 TUIs, 1 server, 1 lease**, screenshot paste renders.

Do not begin stage N+1 while stage N's gate is red. If a gate cannot be met, that is the
most important thing to record in the changelog.

---

## 8. Environment

- OS: Windows (win32), shell PowerShell 5.1. The `bash` tool classifier (stage 03) is
  **cross-platform** (pwsh + POSIX grammars) precisely because development is here — see
  `spec/03-react-tools.md` R-TOOL-080.
- Deliverables live inside `C:\_ddm\projects\Agents\kn9t\`. Never write final artifacts to
  temp; scratch may use temp.
- Rust toolchain: pin an edition (2021) in the workspace; build with `-D warnings`.
- Do not add a dependency not already justified in DESIGN §15 without recording the reason
  in the changelog and checking it against the relevant GI.

### 8.1 Running cargo — ALWAYS via Windows `cmd`

This is a **Windows** project. The toolchain is the Windows `cargo.exe`; there is no Linux
cargo. An agent in WSL/bash will find `which cargo` returns nothing — **that does not mean
cargo is unavailable.** Never report "cargo not on PATH" and fall back to manual review.

**Rule: invoke cargo through `cmd.exe /c`. Do not call the `.exe` by its `/mnt/c/...` path.**

```bash
cmd.exe /c "cargo check -p <crate>"
cmd.exe /c "cargo test  -p <crate>"
cmd.exe /c "cargo test --workspace"
```

For a crate outside the workspace (external plugins), `cd` into it first and run the same way:

```bash
cd plugins/kn9t-custom-provider && cmd.exe /c "cargo test"
```

Rationale: `cmd.exe /c` runs cargo with a native Windows working directory. Calling
`/mnt/c/Users/<user>/.cargo/bin/cargo.exe` directly from bash works by accident but hands
cargo a UNC path (`\\wsl.localhost\...`) whenever the cwd is not under `/mnt/c`, which
cargo mishandles. Using one method consistently also avoids needless rebuilds.

Notes:
- Cargo output uses Windows separators (`crates\kn9t-tui\src\app.rs`); translate to
  `crates/kn9t-tui/src/app.rs` when editing.
- **A gate is not green until a real `cargo test` run says so.** Structural checks (grep,
  reading files) are necessary but never sufficient — §6 "implemented but untested is not
  done" applies to the verification method too.

---

## 9. When you are unsure

- **Requirement ambiguous?** Read the linked `DESIGN §` for rationale. The design almost
  always explains the intent and the rejected alternative.
- **Design silent?** The spec's SPEC-OPEN register (`spec/README.md` §7, §9) lists the
  known-open decisions and their interim values. Use the interim; do not invent a new
  interface.
- **Spec contradicts design, or a MUST is unimplementable?** Stop. Record it in the
  changelog as a spec bug and surface it. Do not work around it silently.

---

## 10. No patches, fix the architecture

When a bug reveals a design flaw, **fix the design** — do not patch around it. Patches
accumulate into unmaintainable code. If the SSE event order doesn't match the TUI's needs,
fix the event order or the TUI's expectations, not both with a buffer hack.

Signs you're patching instead of fixing:
- Adding a "pending" buffer to work around timing issues
- Adding fallback logic (`or_else`, `unwrap_or`) for mismatched field names
- Duplicating code to handle "old" and "new" formats

When you see these patterns, stop and redesign.

---

## 11. TUI as API proving ground

The TUI is not just a client — it is the **experiment** that proves the server API is complete.
When implementing a TUI feature:

1. **If the server API is missing an endpoint** — add it to the server, not a workaround in TUI.
2. **If a feature needs PATCH/PUT to update partial state** — redesign the data model so the
   natural operation is a full replacement or a dedicated action endpoint.
3. **If the TUI needs data the server doesn't expose** — extend the server response, don't
   cache/compute it client-side.

**No PATCH endpoints.** PATCH implies partial updates on complex objects, which:
- Requires merge semantics (what wins on conflict?)
- Breaks event sourcing (events are atomic facts, not diffs)
- Complicates caching and replication

Instead, use **action endpoints** (`POST /session/{id}/rename`, `POST /session/{id}/pin`) or
**full replacement** (`PUT /session/{id}/metadata`).

**The product is not released.** Every TUI limitation is feedback for API design. If the TUI
needs something awkward, fix the API — don't ship the awkwardness.

---

## 12. JSON serialization convention

**All JSON uses `snake_case` for field names and enum variants.** This applies to:

- SSE event payloads (`Event` enum: `text_delta`, `message_appended`, etc.)
- HTTP request/response bodies
- SQLite `payload` columns (JSON-serialized events)
- Plugin protocol messages

Enforce this with `#[serde(rename_all = "snake_case")]` on all enums that serialize to JSON.
Rust code uses `PascalCase` for enum variants internally; serde handles the conversion.

This is a **global invariant** — any mismatch between server and client casing breaks SSE parsing.

---

## 13. Schema-first generation — API contract is committed, not built

`schema/http.json` + `schema/plugin.json` are the single source of truth (ADR-0005, DESIGN §15).

Generated outputs are **committed**:

* `crates/kn9t-server/src/api.rs` — typed request structs (`deny_unknown_fields`)
* `crates/kn9t-tui/src/wire.rs` — GI-6-clean serde mirrors (no `kn9t-*` dep)
* `API.md` — human-readable contract
* `schema/generated/go_types.go` + `schema/generated/python_types.py` — polyglot plugin stubs

Generation is **manual, not at `cargo build`**:

```bash
cargo run -p xtask -- generate   # after any schema/*.json edit
```

Do **not** add a `build.rs` that regenerates on build — it would leak `preserve_order` (IndexMap) into every runtime crate via feature unification (`GI-3` `preserve_order off`, `xtask/Cargo.toml:8`), bloat the `DESIGN §15` budget, and hide API breaks from diff review. Drift is enforced at commit/CI, not at build:

* `scripts/check-schema.sh` — `xtask --check` byte-identical compare; fails on drift
* `.git/hooks/pre-commit` — runs `check-gi1.sh` + `check-schema.sh`; a drifted `wire.rs`/`api.rs` blocks the commit

`cargo build` passes even drifted; only the hook/CI blocks. If `check-schema.sh` fails, run `generate` and commit both schema and regenerated files together.
