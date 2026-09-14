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
- **GI-1** no crate except `kn9t-server` has >1 workspace dependency in `[dependencies]`
  (`[dev-dependencies]` are exempt and reported by `scripts/check-gi1.sh`).
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

- OS: Windows (win32), shell PowerShell 5.1. The `bash` tool runs the host shell; risk
  judgement lives in a policy plugin, not in kn9t (ADR-0008 deleted the in-tree pwsh/POSIX
  classifiers) — see `spec/03-react-tools.md` R-TOOL-080/090.
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

### 8.2 NEVER edit source files with PowerShell — it corrupts UTF-8

**CRITICAL RULE: do not use PowerShell to write, rewrite or patch a source file.** Use the
`edit`/`write` tools, or a Python script. This is not a style preference; PowerShell 5.1
silently corrupts every non-ASCII character in the file.

`Set-Content -Encoding UTF8` (and `Out-File`, `>`, `>>`) does two damaging things:

1. **prepends a UTF-8 BOM** (`EF BB BF`), which shows up as `\ufeff` before `//!` and
   breaks the first doc-comment line, and
2. **re-encodes text that is already UTF-8** — the bytes are decoded as cp1252 and
   re-encoded as UTF-8, so every non-ASCII character is mangled ("mojibake").

The repo is full of `—`, `§` and `──` in comments, so the blast radius is every file
touched. Worked example — em-dash `—` is `E2 80 94`; read as cp1252 those bytes are the
three characters (mojibake); re-encoded as UTF-8 they become `C3 A2 E2 82 AC E2 80 9D`, eight
bytes where there were three:

| character | correct bytes | after one PowerShell write |
|---|---|---|
| `—` U+2014 | `E2 80 94` | `C3 A2 E2 82 AC E2 80 9D` |
| `§` U+00A7 | `C2 A7` | `C3 82 C2 A7` |
| `─` U+2500 | `E2 94 80` | `C3 A2 E2 80 9D C2 80` |

`crates/kn9t-core/tests/mojibake.rs` (96E-15) catches the `§` and `—` forms, but **it does
not catch every variant** — box-drawing `──` passed it while the file was visibly broken.
A clean mojibake test is not proof the file is clean.

**PowerShell is fine for reading** (`Select-String`, `Get-Content`, `Get-ChildItem`) and for
running commands. The prohibition is on *writing source files*.

**If a file is already corrupted**, do not hand-patch bytes — the round-trip is
mechanical and a partial fix (fixing only the patterns the test checks) leaves hundreds of
broken sequences and silently degrades prose (`DESIGN §12` → `DESIGN 12`). Run:

```bash
python scripts/fix_mojibake.py --check <paths>   # report only, exit 1 if repair needed
python scripts/fix_mojibake.py <paths>           # repair in place
```

It strips the BOM and reverses the cp1252→UTF-8 round-trip per run of characters, applying
the fix only where the round-trip succeeds — so correct text (`—`, `§`, `──`, `café`) is
provably left untouched and the script is idempotent.

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

## 9.1 NEVER discard uncommitted changes without explicit permission

**CRITICAL RULE:** Do NOT run `git checkout <path>`, `git restore <path>`, `git reset --hard`,
or any command that discards uncommitted modifications without **explicitly asking the user
first** and receiving confirmation.

Uncommitted changes may contain hours of work. Discarding them is **irreversible data loss**.

Before cleaning the worktree:
1. Run `git status` to see what's modified
2. **Ask the user:** "These files have uncommitted changes: X, Y, Z. Can I discard them?"
3. Only proceed after explicit "yes"

This applies even when you think the changes are "cleanup" or "unrelated". The user may have
been working on them separately.

---

## 9.2 Clippy lint suppression — per-call, not per-file

**RULE:** Never use `#![allow(clippy::...)]` at file level to suppress safety lints like
`unwrap_used` or `expect_used`. File-level allows disable protection for the entire module,
hiding new unsafe additions from review.

**Granularity order (best to worst):**

1. **Per-call** (best) — inline `#[allow]` on the exact expression:
   ```rust
   #[allow(clippy::unwrap_used)] // poisoned mutex = fatal, no recovery
   let guard = self.inner.lock().unwrap();
   ```

2. **Per-function** (acceptable) — when a function has multiple related unwraps:
   ```rust
   #[allow(clippy::expect_used)] // all expects here are mutex poisoned checks
   fn update_state(&self) { ... }
   ```

3. **Per-file** (forbidden for safety lints) — loses all protection for new code.

**Why traits/macros don't work:** A wrapper trait like `SafeUnwrap` requires importing it
everywhere, creates cross-crate dependencies (GI-1/GI-6 violations), and the `#[allow]`
inside the trait impl doesn't suppress the lint at the call site in all clippy versions.

**Acceptable patterns for `unwrap`/`expect`:**
- Mutex/RwLock `.lock().unwrap()` — poisoned = another thread panicked, unrecoverable
- `"127.0.0.1:{port}".parse().unwrap()` — static format, cannot fail
- `lua.create_table().unwrap()` — only fails on OOM, which panics anyway

**Each allow must have a comment explaining why the panic is acceptable.**

---

## 9.3 Assume the developer rebuilds and restarts

**RULE:** Never ask "did you rebuild?" or "did you restart the server?" — the answer is always
yes. The developer is not an idiot. Asking wastes time and is insulting.

When debugging a fix that doesn't seem to work:
1. **Assume** the rebuild and restart happened.
2. **Investigate** the actual cause (logs, code paths, missed edge cases).
3. If truly stuck, add diagnostic logging and ask the developer to reproduce with logs.

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

### 11.1 The TUI is Lua-owned — Rust renders, Lua decides

The entire screen is defined in Lua. Rust provides native views (see
`widgets::NATIVE_VIEWS`: `transcript`, `input`, `status`, `diff`, `welcome`) and draws them
where Lua says; **Lua owns layout, content and styling**.

The goal is that a user can rice the TUI entirely from `~/.kn9t/tui.lua`, with **no
recompile**. When you add a rendering decision, ask where it belongs: *mechanism* (markdown
parsing, syntax highlighting, scroll maths, diff parsing, the render cache) is Rust;
*policy* (what is shown, where, in which colour, under which key) is Lua.

* **The default UI is embedded in the binary** — `crates/kn9t-tui/assets/default_tui.lua` via
  `include_str!` (`src/lua/default_config.rs`). The binary is self-contained: running it writes
  nothing to disk. `--print-config` / `--export-config [--force]` expose it on request.
* **Load order is built-in first, then `~/.kn9t/tui.lua` layered on top.** A user file only
  defines what it overrides; hot-reload re-runs the built-in first, so deleting a user function
  restores the default rather than leaving a stale definition.
* **There is no Rust fallback layout.** `UiOutcome` is `Ok | Failed | NotDefined`; a broken
  config renders `render_lua_error_shell` (red banner naming the error + transcript + input),
  never the old Rust chrome. Silently falling back to Rust chrome hid Lua errors — do not
  reintroduce it. When editing `default_tui.lua`, keep `assets/` and your `~/.kn9t/` copy in sync.
* **A documented Lua symbol must exist at runtime.** `tests/lua_api_contract.rs` asserts that
  every symbol named in the header of `default_tui.lua` is non-nil and callable in a booted
  runtime. This test exists because three documented APIs were dead at once: `render_status()`
  was never called, `kn9t.get_messages`/`kn9t.get_tools` were only installed by their own unit
  tests, and `kn9t.http` was an inert leaking no-op. **A unit test proving a function works in
  isolation says nothing about whether it is wired up.** Add to the contract test whenever you
  add to that header.
* **One colour parser.** `theme::parse_color` serves `[theme.colors]`, every widget colour, and
  `kn9t.theme`. A second copy is how `lightgreen` worked in config.toml and silently resolved to
  the theme default in Lua, killing the context gauge's warn/danger signal with no error.
* **No second mechanism for a job Lua already does.** Lua reaches the server via
  `kn9t.action(...)` ? Rust performs the I/O. The `kn9t.http` client was deleted for this:
  unused mechanisms rot (its whitelist named a nonexistent `/abort`), and a config file people
  copy from each other is the wrong place for an arbitrary HTTP client. Likewise prefer
  `{type="float"}` in `render_ui` over growing the parallel panel-placement registry.
* **An action name that parses must dispatch.** `parse_action` accepting a name with no arm in
  `execute_action` gives a binding that looks right and does nothing (`toggle_right` did this
  for months, with a test asserting the broken behaviour).
* **Geometry flows Lua ? Rust ? Lua.** Never derive a layout number from a Rust-side model of
  the layout. `input_height_for` used to subtract a hardcoded 24-column sidebar that no longer
  existed, so the row count published as `ctx.input_height` disagreed with the box Lua drew.
  Record what was actually rendered (`collect_natives`, `App::input_width`) and feed that back.
* **Approval and interaction overlays stay in Rust.** They are the `POST /approve` /
  `POST /ui-respond` contract paths; a Lua bug must not be able to swallow a denial or
  fabricate an approval. Lua may style them; it may not own the decision or the transport.
* **Publish state cheaply.** `StateSnapshot::collect` publishes bounded scalars eagerly and puts
  heavy data behind lazy accessors. Deep-copying the transcript into Lua every frame cost
  ~3.5 ms/frame; the snapshot is ~0.02 ms (178x). Never rebuild per-frame Lua tables from the
  full transcript.

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

### 13.1 Read `API.md` before writing code against any API

`API.md` is generated from the schema and **cannot drift from it** (§13). It is the fastest
correct answer to "what ops/endpoints/payloads exist". Consult it *before* writing a call or a
test — do not guess a signature and let the compiler correct you. That wastes cycles and
produces plausible-looking code built on invented APIs.

For Rust-internal signatures (constructors, trait methods, field names) the schema does not
cover, read the actual definition or an existing caller:

```bash
# an existing caller is the best template — it already compiles
Select-String -Path "crates\<crate>\tests\*.rs" -Pattern "<Type>::|\.<method>\("
git show HEAD:crates/<crate>/tests/<file>.rs   # if the test was replaced
```

Guessed-then-fixed APIs seen in practice: `ModelRef::new()` (real: struct literal
`ModelRef { provider, id }`), `store.create_session()` (real: free function
`kn9t_store::create_session(&store, &sess, cwd, &model)`), `Event::Live(LiveEvent::…)`
(real: `Event::UiDirective { … }` directly), `Panel::position` as an enum (real: `String`).

**Ops are documented in the schema description, not just in Rust.** Adding a host-API op means
editing `schema/plugin.json` (the `Request.description` op list) and running `generate` — the
Rust `match` arm alone leaves `API.md` silently wrong for plugin authors.

---

## 14. Plugin TUI display — plugins ship Lua, not widgets

A plugin that wants to draw in the TUI **sends Lua source**. There is no fixed placeholder
vocabulary and no plugin-specific Rust in the TUI.

| Op | Payload | Purpose |
|----|---------|---------|
| `ui_register_lua` | `{source}` | Lua defining `render(state)` → widget tree. Send once; 256 KB cap. |
| `ui_set_state` | `{state}` | Arbitrary JSON pushed to `render(state)`. Cheap; send per update. |
| `ui_clear` | `{}` | Drop the plugin's UI. |

Rules this mechanism exists to enforce:

1. **No per-plugin code in `kn9t-tui`.** If a plugin needs a new visual, it ships Lua — you do
   not add a `PlaceholderKind` variant or a `match` on plugin name. The old
   `declare_page`/`write_placeholder` API (fixed `text|number|bar|list` kinds) was removed for
   exactly this reason.
2. **The host does not interpret the Lua or the state.** `kn9t-server` validates the envelope
   and forwards a `UiDirective`; the widget vocabulary belongs to the TUI.
3. **Plugins propose, the user's config disposes.** A plugin returns a widget tree; it does not
   choose placement. `~/.kn9t/tui.lua` decides whether and where to draw it, so a plugin cannot
   seize screen space or hide the transcript.
4. **Each plugin's Lua runs in its own environment** (`set_environment`), so plugins cannot see
   or clobber each other — or the user's config. This is **collision avoidance, not a security
   boundary**: plugins are native executables (`Command::new`, `kn9t-plugin/src/host.rs`) and
   already hold full OS privileges, so restricting their Lua would protect nothing. Do not
   argue for sandboxing as a security measure here.
5. **A broken plugin degrades visibly, never silently.** Load errors, a missing `render`, and
   runtime errors are all captured and displayed in the plugin's own space; one broken plugin
   must not blank the frame or block others (`crates/kn9t-tui/src/lua/plugin_ui.rs`).

Implementation: `crates/kn9t-tui/src/lua/plugin_ui.rs` (registry + isolation),
`crates/kn9t-server/src/host_api.rs` (ops), `crates/kn9t-server/tests/plugin_lua_ui.rs`
(wire-level contract).

### 14.1 Placement is a layout decision, not a plugin's choice

Lua places a plugin with `{type="plugin", plugin="name"}`. The node carries *only* the name;
the plugin's `render(state)` supplies the subtree, and the enclosing layout supplies the rect.

The list of available views comes from `kn9t.state.plugin_views` (stable, sorted order), so
**nothing is hardcoded per plugin** — a newly registered plugin appears without editing
`tui.lua`. Placement belongs in `render_ui`, not buried in a helper: putting it inside
`build_sidebar` would silently force every plugin into one column.

A broken or absent plugin still gets its slot and the error is drawn there. Blanking the area
would look like a layout bug and hide the cause.

Note "sidebar" is now only a **Lua** concept (`build_sidebar` in `default_tui.lua`). Rust's
native views are `transcript`, `input`, `status` — there is no Rust sidebar.

### 14.2 Reference implementation: `kn9t-ask-user`

`plugins/kn9t-ask-user` is the worked example. It registers its Lua lazily (the session id only
arrives with the first tool call, not at handshake), then pushes state around each question.
UI failures are swallowed — the answer matters more than its presentation.

Its Lua is extracted verbatim into a test fixture so it cannot rot:

```bash
python scripts/extract_ask_user_lua.py   # -> crates/kn9t-tui/tests/ask_user_ui.lua
cargo test -p kn9t-tui --test plugin_lua_ui
```

Re-run the extractor after editing `UI_LUA` in the plugin; the suite then exercises the same
Lua the plugin actually ships, so a syntax error fails CI instead of a user's terminal.
