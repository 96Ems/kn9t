# FINDINGS — verified defects

**All entries verified by reading source at the cited `file:line` during the 2026-08-30 review.**
Do not re-derive these. Where verification failed or a claim was corrected, it is marked.

---

## F1 — The bash safety classifier was deleted, never reimplemented [CRITICAL]

**Evidence:**
```bash
git show 5b65819 --stat -- "crates/kn9t-tools/*"   # shows classify.rs | 323 ----
git show 5b65819^:crates/kn9t-tools/src/classify.rs # the lost file, intact
grep -rn "classify" --include=*.rs crates/          # returns nothing relevant
```
Commit `5b65819` message says *"Deleted kn9t-tools crate (migrated to internal-plugins)"*. The
323-line `classify.rs` implementing R-TOOL-080 (two shell grammars) and R-TOOL-090 (the 7-rule
`Ask`/`AllowReadOnly`/`HardDeny` pipeline) was deleted and **never** reimplemented.

**Consequence chain, each verified:**
- `crates/kn9t-server/src/state.rs:33-37` — `AllowPolicy::check()` returns `Decision::Allow`
  unconditionally. It is the **only** `Policy` impl in the workspace.
- Nothing anywhere emits `Event::ApprovalRequest`. Verified: `grep -rn "ApprovalRequest" crates/`
  shows only the enum def, serde tags, test constructors, and the TUI/CLI *handlers*.
- `crates/kn9t-tui/src/app.rs:1994` — the TUI approval overlay is **dead code**.
- `crates/kn9t-server/src/turn.rs:30` declares `static APPROVALS`; `turn.rs:72` inserts into it;
  **nothing ever reads it.** Verified: `grep -rn "APPROVALS\|approvals()" crates/` → 4 hits, all
  decl/accessor/insert.
- `spec/03-react-tools.md` R-TOOL-090 **rule 5** exists specifically to close the
  `sh -c 'rm -rf /'` bypass. **That bypass is currently open.**

**Status:** Phase 1 step 1.1 restored the module + tests (uncommitted). Steps 1.2–1.6 wire it up.

---

## F2 — False greens in TRACKING.md on a G1 gate requirement [HIGH]

`TRACKING.md:167-170` marked R-TOOL-070/080/090/095 as `☑` naming tests
`tool::classify_posix`, `tool::classify_pwsh`, `tool::classify_pipeline`. **Those tests did not
exist.** Per AGENTS.md §6 that is a false green on gate G1.

Also `TRACKING.md:246-253` listed 7 rows of `R-TUI-010..070` about wire types/leases, but
`spec/07-tui.md` defines **25 requirements R-TUI-010..R-TUI-240** on entirely different subjects.
The tracking IDs predate a spec rewrite and were meaningless.

**Status:** corrected in Phase 0 (`4fc0a78`). Stage 03 → `✗`, G1 → `✗`, Stage 07 table realigned
to the real spec IDs (23 of 27 have no test).

**Precedent worth citing:** `TRACKING.md:31-35` already records this exact failure mode for GI-1:
*"the invariant claim in this file was untrue for an unknown period because nothing checked it.
Prefer a script over an assertion."*

---

## F3 — `[policy]` config section is fully specified but NEVER PARSED [HIGH]

DESIGN §10.1 specifies the complete TOML shape (`mode`, `allow_read`, `always_ask`, `never`,
`allow_read_sub`). `crates/kn9t-server/src/config.rs` has **no policy section at all** — verified
`grep -n "policy" crates/kn9t-server/src/config.rs` returns only a doc comment on line 3.

This is the user-facing tuning surface for approvals. Designed, then skipped.

---

## F4 — `POST /approve` silently records "always" as DENY [HIGH]

- `crates/kn9t-tui/src/app.rs:1052` — TUI sends `decision: "always"` (also `app.rs:1057-1059`
  maps Enter on option 1 to `"always"`).
- `crates/kn9t-server/src/routes/session.rs:365` — server does
  `turn::record_approval(state, aid, decision == "allow")`.
- `"always" == "allow"` is **false** → recorded as deny.
- `spec/07-tui.md` R-TUI-130 **requires** an `[Always]` option.
- `scope` is in the doc + DESIGN §10 but the server never reads it
  (`routes/session.rs:361-369`).

DESIGN §10 specifies the full model: `scope=session` caches in a `ConfigPolicy` overlay,
`scope=always` writes to config. None of it exists.

---

## F5 — `created_at` millisecond/second bug → year 57668 [MEDIUM]

- `crates/kn9t-store/src/session.rs:10-12` — `now_ts()` returns `as_millis()`.
- `crates/kn9t-tui/src/wire.rs:188-203` — `visit_i64` does `let secs = v; let days = secs / 86400;`
  treating **milliseconds as seconds**.
- Also uses `days / 365` and `remaining_days / 30` — wrong calendar math independent of units.
- Consumed at `crates/kn9t-tui/src/ui/render.rs:1226` for the session-list date label.

Arithmetic check: a real ms timestamp `1756500000000` → `1970 + (1756500000000/86400)/365` ≈
**year 57668**.

The whole visitor (`wire.rs:164-228`, ~65 lines) is also an AGENTS.md §10 violation: it accepts
**both** i64 and string because the format was never pinned.

---

## F6 — Three-way API contract drift; server has NO typed request structs [HIGH]

**Root cause:** every route hand-pokes `body.get("field")`. Verified:
- 10 `body.get(` calls in `crates/kn9t-server/src/routes/session.rs`
- **zero** `Deserialize` structs in `crates/kn9t-server/src/routes/`
- **zero** `deny_unknown_fields` in the entire workspace

So an unknown or wrong-typed field is **silently ignored** rather than a 400. That is *why* F4
and F7 fail quietly.

**API.md is wrong about nearly every route.** Verified table (API.md ↔ actual server):

| route | API.md claims | server actually does |
|---|---|---|
| `POST /session` | `{model, title}` → `{id,model,title,created_at}` | `{cwd, model:{provider,id}, name}` → `{id,cwd,name,model:{}}` (`routes/session.rs:20-53`) |
| `GET /session` | bare array `[...]` | `{"sessions":[...]}` (`session.rs:72-82`) |
| `GET /session/{id}` | `{id,model,title,messages,seq}` | `{meta,head_seq,ctx_tokens,cost_usd,model,transcript}` (`session.rs:85-124`) |
| `POST /approve` | `{id, approved: bool}` | `{id, decision: string, scope}` (`session.rs:360-370`) |
| `POST /prompt` | `→ {}` | `→ {accepted, seq}` (`session.rs:295`) |
| `POST /session/{id}/fork` | body: none | `{origin_seq, reason}` (`session.rs:127-135`) |
| `GET /models` | bare array, `display_name` | `{models:[...], auth:{}}`, field is `api_id` (`routes/models.rs`) |
| `GET /cost` | `?session=&from=` | `?since=&group_by=` (`routes/cost.rs:14`) |
| SSE events | PascalCase `kind` | **snake_case** (`src/sse.rs:38-47`) |
| `/pref`, `/health`, `/stop`, `/attach` | **absent from API.md** | all exist (`router.rs:174-194`, `:80`) |

**On casing:** AGENTS.md §12 mandates snake_case, so here **API.md is wrong, server is right.**
`spec/07-tui.md` R-TUI-012 says "server is authoritative, update API.md" — nobody ever did.

---

## F7 — `CreateSessionReq.model` is the wrong type; silently ignored [MEDIUM]

- `crates/kn9t-tui/src/wire.rs:250-256` — `model: Option<String>`.
- `crates/kn9t-server/src/routes/session.rs:56-62` — server requires
  `model.get("provider")` + `model.get("id")`, i.e. an object.
- A string can never match → **always** silently falls through to the server default.
- Currently masked: the only caller passes `None` (`crates/kn9t-tui/src/session_manager.rs:82`).

`wire.rs:1-4` claims *"R-TUI-012: MUST match API.md wire format exactly"* — nothing enforces it.

---

## F8 — `app.rs` god object; the most important function is untested [HIGH]

- `crates/kn9t-tui/src/app.rs` — **2,814 lines, ~45 fields**.
- The TUI crate is **13,935 lines** across 34 files — larger than server (4,942) + store (2,518)
  + core (2,471) + react (2,360) combined.
- Other large files: `ui/render.rs` 1,930; `diff_viewer.rs` 1,310.
- `handle_sse` (`app.rs:1886-2019`) applies every SSE frame by mutating `self` directly. It is the
  single most important function in the crate and has **zero tests** — testable only by
  constructing the whole `App`.
- **Untested files entirely:** `app.rs`, `client.rs`, `config.rs`, `event.rs`, `keybind.rs`,
  `log.rs`, `main.rs`, `ui/layout.rs`, `ui/render.rs`, `wire.rs`.
- 58 unit tests exist but cover only leaf utilities. Only acceptance test is
  `tests/acceptance.rs` → `tui_no_kn9t_deps` (a GI-6 dependency check).

**§10 violations in the TUI:**
- Six `pending_*` fields (`app.rs:206-211`: `pending_approval_id`, `pending_welcome_action`,
  `pending_session_click`, `pending_new_session`, `pending_first_message`, `pending_images`) defer
  actions across loop iterations. §10 names "pending buffer" explicitly.
- `crates/kn9t-tui/src/message_handler.rs:394-412` — args parsed via
  `get("args_json").or_else(|| get("args")).or_else(|| get("input"))`. §10 names field-name
  fallbacks explicitly.
- `wire.rs:164-228` — dual-format timestamp visitor (see F5).
- `client.rs:296-330` — SSE reader **never retries**. `app.rs:636` prints
  `"Connection interrupted, reconnecting..."` and then does nothing. `spec/07-tui.md` R-TUI-230
  **requires** reconnect-from-last-seq.

**SSE frames received but ignored:** `ThinkingDelta`, `ModelChanged`, `Compacted` have no handler
arm in `handle_sse` (only their `seq` is recorded).

---

## F9 — Missing endpoints the TUI fakes or hardcodes [MEDIUM]

| TUI wants | current behavior | missing API |
|---|---|---|
| tools sidebar | **hardcodes 4 names** `bash/read/write/edit` (`app.rs:188-192`); toggling `enabled` mutates a field **nothing reads** (`app.rs:1782`) | `GET /tools` — registry exists in `ServerState.tools` but is never exposed |
| `/compact` | prints *"planned for a future release"* (`app.rs:2638`) | `POST /session/{id}/compact` — **engine already exists** at `crates/kn9t-react/src/exec.rs:139` (`run_compaction`), just unreachable |
| `/export` | prints *"planned"* (`app.rs:2643`) | `GET /session/{id}/export` |
| rename session | no UI at all | `POST /session/{id}/rename` — AGENTS.md §11's own worked example |
| `/diff` | **shells out to local `git`** (`app.rs:2767`) using `env::current_dir()`, **not** the session `cwd` | server-side diff, or accept as client-local |
| `/theme` | prints "planned" (`app.rs:2664`, `:2746` — the only 2 TODO markers in the TUI) | none needed |

`GET /tools` is the clearest miss: plugins can register tools the TUI will never display.

**Endpoints that exist but the TUI never calls:** `POST /session/{id}/steer`, `GET /cost`,
`GET /budget`, `POST /session/{id}/fork`, `GET /health`. Steering and fork/rewind are core to
DESIGN but unreachable from the UI.

---

## F10 — `POST /plugin/{name}/reload` specified but absent [LOW]

`spec/08b-plugin-redesign.md` R-PLUG2-100 specifies a 5-step reload sequence (cancel in-flight →
wait → shutdown → respawn → re-handshake) triggered by `POST /plugin/{name}/reload` or SIGHUP.
Verified absent: `grep -rn "reload" crates/kn9t-server/src/` → only an unrelated comment.

---

## F11 — SDK context structs were unconstructible outside the SDK [HIGH — FIXED in Phase 0]

Commit `81db0c7` added `pub kv: KvClient` to both `ToolCallCtx` and `ProviderCallCtx`
(`crates/kn9t-plugin-sdk/src/ctx.rs:378-398`). But `KvClient::new` is `pub(crate)`
(`ctx.rs:261`), while `CancelToken::new` and `ChunkSender::new` are `pub`.

→ After that commit **no code outside the SDK could construct either context struct.** Every
external Rust plugin's *test* target broke with `error[E0063]: missing field kv`.

**CORRECTION TO AN EARLIER CLAIM:** I initially said *"plugins/kn9t-custom-provider does not compile."*
**That was wrong.** `cargo build` succeeds and the binary works in production — only the test
target failed, because production never constructs a context (the SDK does).

**Fixed** in Phase 0 by adding `pub fn KvClient::for_test()` at the seam rather than patching each
plugin. `plugins/kn9t-custom-provider/src/client.rs:333` now uses it; its 26 tests pass.

Go (`plugins/kn9t-agents-md/main.go`) and Python (`plugins/kn9t-mcp/`) hand-roll the wire
protocol, so a Rust signature change could not reach them. **That is luck, not design** — and
exactly what ADR-0005 exists to prevent. Note for Phase 2: *a `pub` field whose type has no `pub`
constructor is a compile-time-invisible contract break.*

---

## F12 — 46 `eprintln!` in server hot paths [LOW]

`grep -rn "eprintln!" crates/kn9t-server/src | wc -l` → **46**. `prompt()`
(`routes/session.rs:203-293`) logs body keys, text length, per-image parse progress, and append
seq on **every request**. `crate::log!` already exists (`src/log.rs`). Debug scaffolding shipped
as behavior. Also `router.rs:155` logs every `POST /model`.

Related repo hygiene: `kn9t-tui.log` (726 KB) and `custom-provider.log` (29 MB) are committed/sitting in the
repo root; `plugins/kn9t-custom-provider/custom-provider.log` too.

---

## F13 — 3 react tests depend on build order [UNRESOLVED — needs an honest number]

In a **fresh git worktree** (no prior `target/`), these 3 fail:
`hook_posture`, `turn_sequence`, `parallel_order` — all panicking at
`crates/kn9t-react/tests/support/mod.rs:323` with
*"kn9t-tools binary not found at … Run `cargo build -p kn9t-tools-plugin` first."*

In the **main working tree** `cargo test -p kn9t-react` passes (12 passed) because the binary
already exists in `target/debug/`.

→ Not a source defect; a **test-harness fragility**. `support/mod.rs:300-320` locates the binary
by walking up to `kn9t/target/{debug,release}`. Phase 3 (auto-discovery) changes exactly this
lookup, so fix it there.

**TODO for next actor:** determine the honest workspace pass/fail count and correct
`TRACKING.md` (see `job/tracking.md` → "Test count").
