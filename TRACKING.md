# TRACKING — kn9t

Live status scoreboard. This is the mutable counterpart to `AGENTS.md` (the rulebook) and
`CHANGELOG.md` (the narrative). Update it **as you work**.

Legend: `☐` pending · `▣` in progress · `☑` done (acceptance test passing) · `✗` failing ·
`—` test not written yet.

---

## Current position

**2026-09-16f (reasoning: what we ask for vs what we replay):** a live 400 on DeepSeek via
OpenCode Go (`unknown variant 'thinking'`, `messages[6]`) root-caused to `kn9t-provider-openai`:
the chat encoder ignored `Quirks::thinking_replay` and replayed persisted thinking as an
Anthropic content part — and since the block is persisted, every later turn repeated the 400.
Fixed, along with the two policy bugs beside it:

| what | where | test |
|---|---|---|
| `encode_messages` honours `thinking_replay`: `strip` drops the block (a reasoning-only turn is not sent at all), `verbatim` keeps it | `crates/kn9t-provider-openai/src/{encode,responses}.rs` | `oai::thinking_replay_*` (3) |
| default `thinking_replay` = `strip` — no wire format this crate speaks has a reasoning-input form | `crates/kn9t-provider-core/src/quirks.rs` | `oai::thinking_*` |
| reasoning is requested by default: `Thinking` travels on `ModelSpec` (`Effort(Medium)`), `[model] thinking = "off"\|"low"\|"medium"\|"high"` overrides it, and `Off` omits the field instead of substituting `low` | `crates/kn9t-core/src/model.rs`, `crates/kn9t-server/src/{config,turn}.rs` | `core::thinking_defaults_to_medium`, `core::model_spec_thinking_serde_default`, `oai::thinking_off_omits_reasoning_effort` |
| reasoning renders as its own card above the answer, spinner while live, click or Ctrl+E to toggle; `ThinkingCard` mirrors `ToolCard` and rides the render cache | `crates/kn9t-tui/src/{thinking,message_handler,reducer,app,render_cache}.rs`, `src/ui/render.rs` | `thinking::*` (4), `tui::streamed_then_persisted_reasoning_yields_one_card`, `tui::streamed_reasoning_survives_a_message_without_it` |

`cargo test --workspace` green **except** `kn9t-tui`'s
`file_index::a_root_gitignore_contributes_plain_names`, which is the user's in-flight TUI work.

Open next: the **TUI reasoning display** — show thinking decoupled from standard output. The
render path already has a collapsible thinking view (`ui/render.rs`, `thinking::parse_content`),
but it parses `<thinking>` tags out of content, while deltas now arrive as `ThinkingDelta`; check
how the reducer routes them before adding anything.

---

## Position — 2026-09-16d

**2026-09-16d (TUI polish — design agreed, no code yet):** the request was "polish the TUI,
make it IDE-like", which is a direction, not a specification. So the session ran a 26-question
design grill instead of writing code, and ended with the decisions in `PLAN.md` §P7: VS Code
chrome whose "editor" is the conversation, session tabs, a native file explorer + centre-top
viewer + `@` finder, a persistent-shell terminal reached through the server, tool cards grouped
per turn, a violet/amber/silver palette, and an animated pixel-art mascot.

| what | where |
|---|---|
| the 21 decisions (D1–D21) | `PLAN.md` §P7 |
| the four items that change an existing rule (R-TUI-100, AGENTS §11.1, §14, ADR-0006) | `PLAN.md` §P7 "items to record" |
| the reasoning, and a wrong diagnosis corrected in-session | `CHANGELOG.md` 2026-09-16d |

Confirmed while designing, rather than assumed: the lease is keyed **per session**
(`crates/kn9t-server/src/lease.rs`), so session tabs need no lease change — only one SSE stream
per open session, and an `App` that stops assuming a single transcript. And `@` mentions have
**no implementation today** (R-TUI-100 has no test; `slash.rs` only implements the `/` dropdown).

Open next: **P7-L1 (chrome)**, starting with the palette in `theme.rs` + the `C` table of
`assets/default_tui.lua`. Reset `~/.kn9t/tui/*.lua` first — they load *after* the embedded
default and override it wholesale.

---

## Position — 2026-09-16c

**2026-09-16c (`/v1/messages` by reuse, and three latent bugs it exposed):** exactly **one**
OpenCode Go model requires `/v1/messages` and works — `minimax-m2.7`. Every other
chat-completions failure also fails in Messages (`kimi-k2.5`, `glm-5`, `qwen3.5-plus`,
`mimo-v2-pro/omni`, `hy3-preview`, `union-alpha`: Go is not serving them at all), and
`grok-4.6` is responses-only. So no second protocol implementation was written: `kn9t-anthropic`
already speaks Messages and now points at the gateway.

| what | where | test |
|---|---|---|
| `session` in the provider-call payload | `kn9t-plugin/src/remote_provider.rs` | (compile) |
| gateway endpoint + session header from env; `ToolSpec` → `input_schema`; tagged-`Cache` fix (R-ANTH-050, R-ANTH-030) | `plugins/kn9t-anthropic/src/{client,map}.rs` | `map::tests` (9) |
| `discover = false` (R-SRV-CFG-030) — also closes C1 | `kn9t-server/src/config.rs` | `srv::config_discover_false` |
| TOML escaping when appending a plugin to config.toml | `crates/kn9t/src/cmd_install_plugins.rs` | `tests::toml_basic_escapes_windows_paths` |

Live, TUI: `opencode-go-ant` → `minimax-m2.7`, full tool round-trip, sidebar `cache 26%`.

Open next: **C2** — `Quirks::merge` cannot implement R-PCORE-080 (no `Option`s, so "unset" is
unrepresentable); the server merges on `RawQuirks` instead and only `pcore::quirks_merge` calls
the method. Optional hygiene: set `discover = false` on `opencode-go` too — 9 of its 37
discovered models answer 400/401 on every format, so the picker still offers dead entries.

---

## Position — 2026-09-16b

**2026-09-16b (OpenCode Go + session header quirk, `/v1/responses`):** OpenCode Go rejects every request that omits
a per-conversation `x-opencode-session` (`400 MissingSessionID`), which made the gateway
unreachable from kn9t. Added `Quirks::session_header` + `Request::session` and wired them through:
the header *name* is config, the *value* is the conversation. `~/.kn9t/config.toml` now carries
`[provider.opencode-go]` and 24 `[[model]]` entries (ctx/max_out/prices from models.dev — Go's
`/v1/models` returns ids only, so discovery would have registered everything at 128k/price 0).
Verified live in the TUI (built binary): one turn on `deepseek-v4.1-flash` streamed and answered,
sidebar read `ctx 1.00M` with cost accounted.

| what | where | test |
|---|---|---|
| `Request::session` (R-CORE-170) | `kn9t-core/src/provider.rs` | (compile) |
| `Quirks::session_header` (R-PCORE-080) | `kn9t-provider-core/src/quirks.rs` | pcore::quirks_merge |
| header injection (R-OAI-050) | `kn9t-provider-openai/src/provider.rs` | oai::session_header |
| call sites | `kn9t-react/src/exec.rs`, `kn9t-server/src/turn.rs`, `kn9t-server/src/host_api.rs` | — |
| config passthrough | `kn9t-server/src/config.rs` (`RawQuirks`) | srv::unit_config `per_model_quirks_reach_the_provider` |
| `Quirks::api` + `/v1/responses` codec (R-OAI-060/070) | `kn9t-provider-core/src/quirks.rs`, `kn9t-provider-openai/src/responses.rs` | `oai::responses_*` (7), srv::unit_config `unknown_api_is_rejected` |

Open next: `/v1/messages` (nearly free — point `plugins/kn9t-anthropic` at the gateway and give the
plugin protocol a `session` + config passthrough; the gateway requires `x-opencode-session` on
`/messages` too), and the unconditional `/models` discovery that registers models which 400.
See CHANGELOG 2026-09-16b (C1, C2).

---

**2026-09-16 (reliability audit, 11 bugs):** audit of the ReAct loop + client/server harness for
wedge / desync / transient-vs-durable failures. Eleven first-order bugs, each reproduced by a
failing test before the fix. `cargo test --workspace --no-fail-fast`: **863 passed, 0 failed,
3 ignored**. `cargo clippy --workspace --all-targets`: 0 errors, 0 warnings in touched files.
`check-gi1.sh` / `check-mojibake.sh` / `check-schema.sh` / `check-unwrap-trend.sh` all green.

| Bug | What was wrong | Fix | Tests |
|---|---|---|---|
| B1 | `Event::from(TurnFinishing)` panicked on any non-`SessionSink` bus -> turn thread died -> session 409 forever | `is_internal()` + `to_observable_event() -> Option<Event>` | `unit_turn_finishing` 5 |
| B2 | No provider ever produced `ProvErr::ContextOverflow`; compaction was unreachable code | `is_context_overflow()` wired pre-stream + in-band | `unit_overflow_classify` |
| B3 | Provider overflow + nothing to compact = identical request re-billed forever | charge `replans` in that case, fail once spent | `unit_context_overflow` 4 |
| B4 | `run()` had no turn ceiling; `should_stop` defaults false and fails false | `ReactConfig::max_turns` + `ReactError::TurnLimit`, `end_turn()` factored | `unit_max_turns` 5 |
| B5 | `abort` fired outside the lock; ESC swallowed, or applied to the next turn | `abort_turn(session, Option<id>)`, cancel fired under lock | `unit_turn_registry` 6 |
| B6 | `aborts` keyed by session only; turn A's teardown deregistered turn B | `TurnSlot` guard, compare-and-swap on `turn_id` | `unit_turn_registry`, `unit_turn_lifecycle` 9 |
| B7 | `Instant::now().elapsed()` ~= 0 -> lease tokens were `lease-{n}-0`, guessable | random suffix via `auth::generate_token()` | `unit_lease` +3 |
| B8 | `h.join()` unbounded; one hung `parallel_safe` tool froze the turn permanently | channel + `recv_parallel_result` with cancel grace, synthesized result | `unit_parallel_hang` 4 |
| B9 | SSE dedup assumed the ring never drops; an evicted durable echo = permanent hole | `contiguous_through` + `gap_detected` + `event: gap` frame | `unit_sse_gap` 3 |
| B10 | `unwrap_or_else(Cancel::new)` is unfireable + no deadline -> plugin worker wedged for life | `wait_until(deadline)` on both registries; expiry denies | `unit_interaction_deadline` 5 |
| B11 | Sub-agent watchdog armed on the **parent's** `Cancel`; killed the parent 600 s later | child gets its own `Cancel`, one-way watcher, `DoneGuard`, own `TurnSlot` | covered by the refactor |

**Architecture note.** Four of the eleven (B1, B5, B6, and B8's blast radius) share one root cause:
`ServerState::aborts` was both the cancellation registry and the "turn running" flag that `/prompt`
gates on, keyed by session id alone, with its lifecycle driven by a *transient* event
(`SessionSink` on `TurnFinishing`) plus a best-effort fallback. `turn::TurnSlot` now owns the
registration, the `Cancel`, and the `IdleTracker` count as one lifetime released on `Drop`. The two
facts keep different lifetimes on purpose: `aborts` is released early so the next prompt is not
409'd, the idle count only on `Drop` so idle-exit cannot reap the process during `maybe_autotitle`.

**Config note.** The deadlines introduced by B10 (and B8's grace) are `[server]` knobs, not
constants: `approval_timeout_secs`, `interaction_timeout_secs`,
`interaction_timeout_no_cancel_secs`, `tool_cancel_grace_ms`, behind `config::ServerTimeouts`.
Defaults equal the previous hardcoded values, and `0` means *no deadline* rather than "expire
immediately" (a literal reading would deny every approval instantly).

**Known gap:** B8 abandons a hung tool thread rather than killing it (Rust cannot kill a thread from
outside). The turn ends and the session stays usable, but a repeatedly-hanging plugin leaks threads.
The real fix belongs in `PluginHost` (kill the subprocess, which drops the pipe).

---

**2026-09-15 (96E-46 + 96E-51 epics, 8 tickets):** plugin lifecycle is now agent-drivable and
session forks are navigable. `cargo test --workspace --no-fail-fast`: **815 passed, 0 failed,
2 ignored** (both Windows-only `#[ignore]`: `srv::plugin_reload` and the new
`srv::p1_96e47_plugin_stop_start`, same POSIX-shell-dummy-plugin reason). `cargo clippy
--workspace --all-targets`: 0 errors, 0 warnings in the new files. `check-gi1.sh` / `check-schema.sh`
/ `check-mojibake.sh` all green. Commits `339585d` (wip) + `f6eb2a7` (encoding repair).

| Ticket | What shipped | Tests |
|---|---|---|
| 96E-47 | `stop_plugin`/`start_plugin` + `stopped_plugins`; `POST /plugin/{n}/stop\|start`, `GET /plugin`. Stopped plugins keep their specs in the `tools` array (level-1 prefix intact) and are refused via `blocked_tools()` | `plugin_lifecycle::*` 13, `srv::p1_96e47_plugin_stop_start` (Windows-ignored) |
| 96E-48 | `ToolSource` trait in kn9t-react; `ReactLoop.tools: Arc<dyn ToolSource>` re-snapshotted per model call; `FilteredTools` keeps the subagent grant live | `live_tools::*` 8 |
| 96E-49 | `plugins/kn9t-plugin-manager` (5 tools) + host_api ops `plugin_list/stop/start/reload/load` delegating to the same `state.*` the routes use | covered via the ops in `plugin_lifecycle::*` |
| 96E-50 | `HiddenOverride` + `set_tool_hidden`/`set_plugin_hidden`; generic ops `plugin_health` + `tool_visibility` (caller-scoped); `Event::PluginState` fanned out by `notify_plugins` | `plugin_lifecycle::*` (hidden/override cases) |
| 96E-52 | `origin_session`/`origin_seq`/`fork_reason` projected by `GET /session` and `GET /session/{id}` | `srv::p1_96e52_session_list_exposes_fork_parentage` |
| 96E-53 | `kn9t-tui/src/session_tree.rs` (`build_forest`, `picker_order`, `reason_badge`) + indented sidebar with reason badges | `unit_session_tree::*` 12 |
| 96E-54 | `Overlay::SessionTree` git-graph view, keyboard switch, degrades cleanly on a single session | shares the `build_forest` fixtures |
| 96E-55 | `/fork [seq]`, `/undo [n]`, `/tree` in `COMMANDS`; `plan_fork()` (pure) + `fork_into()`; silent switch via the `/new` sequence | `unit_session_tree::*` 10 (`plan_fork` rules) |

**Architecture note (96E-50).** The first cut hardcoded `"kn9t-plugin-manager"` in `state.rs` so the
server could reveal that plugin's tools on discovery/crash. Rejected in review: the server must not
special-case an external plugin. The shipped design gives the server two generic primitives
(`plugin_health` reports what it observed; `tool_visibility` lets a plugin change **its own** tools,
scoped by the host's dispatch so no plugin can touch another's) and moves the reveal policy into the
plugin. Smaller server, and no privileged plugin name in core.

**Stage:** PLAN.md post-v1 improvements (P1–P4 complete). Architecture cleanup 2026-08-30,
Phase 0–2 done (docs scaffolding, classifier+approvals, schema-first API contract).
**Last gate green:** stage 09 — R-CP-900 / R-ANTH-900 green. **ADR-0008 landed 2026-08-31** — policy
judgement moved out of the server into a user-installed plugin. `crates/kn9t-server/src/classify.rs`
(333 lines) and `tests/classify.rs` are **deleted**, superseding the "G1 restored" note that stood
here. **96E-31 rewrote R-TOOL-070/080/090/095** to describe the mechanism that actually ships —
`HookVeto { Allow, Ask, Deny, Replace }`, strictest-wins composition, and the two failure postures
(fail-open with no policy plugin, `DenyAllApprover` when nobody can be asked) — so the section is no
longer SPEC-STALE and stage 03's gate is achievable as written. The surviving mechanism
(`ApprovalRegistry`, `ApprovalCache`, `POST /approve`, `once|session|always`) is covered by
`policy::*` 11 passed + `srv::approve_*` 7 passed, both now named by the requirements.
`cargo test --workspace`: **493 passed, 0 failed, 1 ignored** — green on Windows since 96E-35 turned
`srv::plugin_reload`'s hardcoded `panic!` into a documented `#[ignore]` and 96E-38 fixed a real
`Cancel::wait_timeout` bug (spurious-wakeup early return) that had been read as a flaky test. Plus
the external `plugins/kn9t-custom-provider` crate: **26 passed, 0 failed** (run separately — it is no
longer a workspace member; `cd plugins/kn9t-custom-provider && cargo test`).
Verified 2026-08-31 via `cargo test --workspace --no-fail-fast` + `cargo run -p xtask -- generate`
(schema regenerated, Go/Python stubs committed) + GI-1 dependency check.
**2026-09-02 (post-batch):** 12 commits landed (P1 `76205ff`, 96E-8..16 `72f5ef6..735e7f2`, TUI
hardening `9fbfe98`, and live-breakage fixes `40a3088`/`1b97a90` = 96E-18 durable SSE echo +
96E-19 empty tool content / picker / autotitle, and 96E-17 fail-closed compaction + plugin → host API + compactor plugin). `cargo test --workspace`: **437 passed, 0 failed**
(`srv::plugin_reload` excluded on Linux — Windows-harness-only by design, documented). Live TUI
verified via `tui-testing` MCP against the real server: tool cards visible live + on reload,
streamed text committed (no more disappear), empty tool output no longer 400s, `/session` picker
filter opens the first match. Details in CHANGELOG "Session — 2026-09-02 (2)".
Known flake, unrelated: `cancel::tests::test_wait_timeout_returns_false_on_timeout` fails ~1 run in 3.
**v1 e2e fully verified:** `kn9t chat` → server → ReAct loop → `kn9t-tools` + `kn9t-custom-provider`. Date: 2026-08-27.
**PLAN.md progress:**
- P1-A bootstrap ☑ — `~/.kn9t/` auto-created on first run (config.toml template + token).
- P2-A one-shot ☑ — already working; kept clean.
- P2-B REPL + `--continue` ☑ — `kn9t chat` enters REPL; `--continue` resumes latest session by `head_seq`.
- P2-C approval flow ☑ — crossterm inline `[ No ] / [ Yes ]` selector on `ApprovalRequest`.
- P3-A `kn9t sessions` ☑ — live verified (20 sessions, sorted by SEQ desc).
- P3-B `kn9t history` ☑ — live verified (transcript with ANSI roles, tool calls, results).
- P3-C `kn9t attach` ☑ — live verified (piped prompt, streamed response with tool calls).
- P4-A real subprocess tests ☑ — `kn9t-test-plugin` binary; `plug::composition` + `plug::timeout` use `PluginHost::spawn()`.
- P5-A G3 gate ☐ — deferred (requires human at terminal: 3 TUI instances + screenshot).
- P5-B TUI redesign ☑ — implemented (7.3K lines). Manager composition completed 2026-08-28.
**Architecture cleanup (2026-08-28):**
- **GI-1 was violated and is now enforced.** `kn9t-provider-openai` had 2 workspace deps
  (`kn9t-core` + `kn9t-provider-core`) while every doc asserted "GI-1 holds". Fixed via
  re-exports; `scripts/check-gi1.sh` + a pre-commit hook now enforce it mechanically.
  Lesson: the invariant claim in this file was untrue for an unknown period because nothing
  checked it. Prefer a script over an assertion.
- **TUI manager extraction finished.** `SessionManager`/`ModelSelector`/`TokenTracker`/
  `Transcript` were dead code (0 external refs, ~835 lines) while `app.rs` duplicated their
  state by hand. Now composed: `App` 53 → 32 fields, and their 36 unit tests cover live paths.
- **Duplicate SSE parser removed.** `kn9t-custom-provider`/`kn9t-anthropic` each carried a byte-identical
  102-line copy. Moved to `kn9t-plugin-sdk` (their only workspace dep, so GI-1 is preserved —
  `kn9t-provider-core` would have broken it). Net −65 lines.
- **Test-suite drift repaired.** ~40 initializers predated the `Message.silent` /
  `ToolSpec.hidden` fields and the `ServerState::new` signature change; `core::event_tag`
  asserted PascalCase discriminants against the snake_case rule in AGENTS.md §11.
- **Two production bugs found via the above:** `/attach` hardcoded a 30 s heartbeat instead
  of `sse::heartbeat_interval()` (dead-client detection was untestable and unconfigurable);
  `kn9t-react` silently swallowed malformed `args_json` — now emits `Event::Error`.
**Idle-exit fixed (2026-08-27):** grace-on-last-disconnect (5 s default). Keepalive ping
detects dead clients. `POST /stop` + `kn9t stop` for graceful shutdown. SPEC-OPEN 14 resolved.
**Architecture review (2026-08-30):** Found critical issues:
1. **Deleted classifier.** Commit 5b65819 deleted `classify.rs` (323 lines) during the
   tools-to-plugin migration. The acceptance tests named in TRACKING.md (`tool::classify_*`)
   do not exist. `AllowPolicy::check()` returns `Allow` unconditionally. The `sh -c 'rm -rf /'`
   bypass (R-TOOL-090 rule 5) is currently open. Gate G1 is **no longer green** for
   R-TOOL-070/080/090/095.
   > **Resolved by ADR-0008 + 96E-31.** The deletion was the intended direction, not a
   > regression: risk judgement is a policy plugin's job. The spec now describes `HookVeto`
   > and names tests that exist (`plug::composition`, `srv::approve_*`). The `sh -c` bypass
   > is not "open" so much as **out of scope by decision** — with no policy plugin installed
   > kn9t fails open and says so (ADR-0008 decision 5); a classifier that `sh -c` defeats
   > would be worse than an honest absence.
2. **Dead APPROVALS map.** `static APPROVALS` in turn.rs is declared and inserted into but
   never read. Nothing emits `Event::ApprovalRequest`. The TUI's approval overlay is dead code.
3. **Three-way API drift.** API.md, the server, and wire.rs disagree on nearly every route.
   Root cause: no typed request structs, no `deny_unknown_fields`. Example: TUI sends
   `decision: "always"` but server checks `== "allow"`, so "always" silently records as deny.
4. **Year-57668 bug.** Store writes `created_at` as milliseconds; TUI deserializer treats it
   as seconds.
5. **TRACKING.md Stage 07 table used obsolete IDs.** Realigned to spec/07-tui.md.

A 4-phase cleanup is underway:
- Phase 0 (done): docs scaffolding — CONTEXT.md, ADRs, TRACKING corrections.
- Phase 1 (done): restore classifier + approvals; `[policy]` config parsed (config.rs).
- Phase 2 (done, 2026-08-31): schema-first API contract (ADR-0005) — xtask generator,
  typed server request types (`deny_unknown_fields` → 400), regenerated wire.rs/API.md,
  drift gate + pre-commit hook, F5/F6/F7 reconciled. See CHANGELOG 2026-08-31.
- Phase 3: all-plugins-external migration (also fixes F13 — react tests' binary
  lookup fragility determined by honest number: 3 fail on a fresh worktree,
  12 pass once `target/debug/kn9t-tools` exists). **Step 3.1 done** (plugins moved
  to `plugins/`; workspace clean) **+ Step 3.2 done 2026-08-31** (auto-discovery of
  `~/.kn9t/plugins/`, ADR-0004; server merges discovered + `[[plugin]]` tools;
  project-relative `plugins/` never scanned, proven by test) **+ Step 3.3 done
  2026-08-31** (config overrides discovery: pin / env-inject / disable; duplicate
  `kn9t-tools` deduped, was 8 tools → 400, now 4) **+ Step 3.4 done 2026-08-31**
  (R-PLUG2-110 rewritten for discovery: soft-fail + warn, not startup fail; spec/README +
  DESIGN §16 + lib.rs doc updated; spec bug recorded) **+ Step 3.5 done 2026-08-31**
  (`POST /plugin/{name}/reload` hot-reload with 5-step cancel/shutdown/respawn;
  `plug2::hot_reload_cancels_inflight` green, `srv::plugin_reload` 41/41 acceptance).
- Phase 4: TUI decomposition (app.rs god object) — **Step 4.1-4.3 done 2026-08-31** (`GET /tools`, `POST /session/{id}/rename|compact`, `GET /session/{id}/export`, `GET /tools` drives sidebar, `/compact`/`/export`/`/rename` real, `/diff` uses `session.state.cwd`), **Step 4.4a done** (`reducer.rs` pure `(State, SseFrame)->State`, 8 tests, `tui::sse_reconnect` green, `ThinkingDelta`/`ModelChanged`/`Compacted` handled, zero `pending_*` fields), **Step 4.4c done 2026-08-31** (`queued_*` eliminated — `handle_key`/`handle_welcome_key`/`handle_overlay_key`/`execute_action`/`execute_slash_command`/`execute_palette_command` now take `&Sender<Event>` and act immediately; `run` loop's deferred block deleted), **Step 4.4b intentionally deferred** (Welcome vs Chat split — low ROI; pure `reducer.rs` + `queued_*` deletion already gives testable seam; 2.7k lines remains but not in exit criteria).
- Phase 5: revise DESIGN, record outcomes — **done 2026-08-31** (DESIGN §11/§15 §15 trigger fired F5/F7→schema-first, §10 effects + Policy as single seam, §16 `plugins/` not `internal-plugins`, ADRs 0006/0007, `.gitattributes * text=auto`, `spec/07-tui.md` reconciliation R-TUI-012/050/110/220/230, TRACKING honest, test count 398+26=424).

Seven ADRs written in `docs/adr/`:
- ADR-0001: bash classifier lives in server
- ADR-0002: plugins declare effects, server decides risk
- ADR-0003: dry-run is preview, not safety input
- ADR-0004: plugin discovery scans ~/.kn9t/plugins/ only
- ADR-0005: schema-first API contract
- ADR-0006: Policy is the single safety seam
- ADR-0007: CRLF normalization via .gitattributes

**Next:** E2E live compaction (session à ~80 % ctx avec le plugin compactor branché) — puis SDK Rust parity pour le RPC request/reply, puis ADR-0008 spec rewrite (R-CORE-270/R-RCT-100/R-TOOL-070/080/090/095 still SPEC-STALE; `plugins/kn9t-policy.py` fail-open + `allow`-means-`ask` left undone), puis G3 manual verification. Phase 4 + Phase 5 done: `cargo test --workspace` 437/0 (2026-09-02), `check-gi1.sh` OK, `check-schema.sh` OK, G1 green, TUI live breakage fixed (96E-18/19, see CHANGELOG), **96E-17 fail-closed + host_api RPC + TS compactor plugin** (see CHANGELOG), 7 ADRs.

**2026-09-06 — config hot-reload (R-SRV-CFG-100/110):** `POST /config/reload` swaps
providers/provider_hosts/models/default_model behind `RwLock` snapshots; a 2 s mtime-poll
watcher (`watch::Debouncer`, pure state machine, 5 unit tests) auto-reloads `config.toml`
with a debounce so editor write-bursts never reload a half-written file. Scope is partial
by design: `[[plugin]]`, `[policy] mode`, `[server] idle_exit_secs` are NOT reloaded.
`kn9t-server` 96 tests green. **Found 4 pre-existing bugs (B1–B4 above); B2 is high
severity** — compaction is driven by a `bytes/4` estimate, not by real token usage, so a
session can pass 162k reported tokens on a 128k-window model without ever compacting.
Also added **AGENTS.md §8.2: never write source files with PowerShell** (it BOM-prefixes
and cp1252-double-encodes UTF-8) plus `scripts/fix_mojibake.py` to repair the damage.

---

## 96E issue register (post-PLAN fixes)

The P1/96E batch and later live-breakage fixes are tracked here (they are not spec requirements, so they have no per-requirement row in the tables above).

| issue | fix commit | summary | verified |
|---|---|---|---|
| P1 | `76205ff` | PluginHost thread-local session/bus isolation, parallel after_tool_call, atomic SSE read_attach_snapshot | tests + live |
| 96E-8 | `72f5ef6` | malformed tool args_json → Deny before execution | tests |
| 96E-9 | `ba2cf4e` | plugin event forwarding try_send (no RPC stall on flood) | tests |
| 96E-10 | `fb9d04b` | host poison on protocol corruption + check_healthy() | tests |
| 96E-11/12 | `062f5d0` | compaction shares provider-attempt classification; EventSink::emit(LiveEvent) transient-only | tests (but see 96E-18 — the durable-echo half was missing) |
| 96E-13 | `a100904` | single-connection serialized SQLite doc | docs |
| 96E-14 | `07f645c` | integer micros cost accounting | tests |
| 96E-15 | `d612c0a` | mojibake byte-level repair | tests |
| 96E-16 | `efe897d` + `735e7f2` | Event::Handoff + Compactor trait + SDK PluginCompactor | tests |
| 96E-18 | `40a3088` | **durable SSE echo via store after-append observer** — 96E-12 removed durable events from the live bus but the promised server-side echo never existed; TUI got no MessageAppended → no tool cards live, streamed text dropped at next TurnStarted | `srv::p1_96e18_durable_appends_echo_on_sse_bus` + live TUI |
| 96E-19 | `1b97a90` | empty tool content wire form `"(no output)"` (400 fix), session picker substring filter + first-match select, autotitle uses session model | unit tests + live TUI |
| 96E-17 | `b700895` + `36b651e` + `71aa63a` + `d63554b` + `b4dbf7f` | **compaction fail-closed** (built-in prompt fallback supprimé — pas de plugin = pas de compaction, session terminée), **plugin → host API RPC** (`request`/`api_result`; ops `provider_complete`/`session_read`/`tool_execute` + **`session_fork`/`session_prompt`**; usage `UsageKind::Subagent`), **RemoteCompactor** (hook `compactor_compact`), **sous-agent = session forkée** (fork_reason=subagent + budget ForkSnapshot + `run_session_turn` synchrone/watchdog; `ToolRegistry::filter_names`; `session` dans le payload tool_call; **built-in spawn_session retiré** (96E-17: zéro tool built-in — `kn9t-subagent` fournit le tool)), **plugins TS `kn9t-compactor`** (2 passes agent via host API) + **`kn9t-subagent`** (tool `spawn_session` pilote = la preuve des primitives) | `plug::p1_96e17_*` (4) + `srv::p1_96e17_*` (3 ops + fork/prompt) + `npm test` ×2 simulate + fail-closed react + smoke serveur (2 plugins handshake OK) |

---

---

## P7 register (TUI polish — `PLAN.md` §P7)

These are not spec requirements (except where a row says so), so they have no per-requirement
test row. A lot is `☑` only when its acceptance test and its before/after screenshot exist.

| lot | subject | status |
|---|---|---|
| L1 | chrome: palette (violet/amber/silver), transcript hierarchy, tool cards grouped per turn, bordered input, segmented status bar, two-row header + session tabs, right panel without `recent calls`, overlays | ▣ **code done + verified**, three deferrals below |
| L2 | explorer + viewer + `@` finder (one Rust file index, three surfaces) | ▣ **index done**; tree/viewer/`@` not started |
| L3 | terminal: persistent shell in `kn9t-tools`, server exec endpoint, private-by-default + promotable | ☐ |
| L4 | animated pixel-art mascot (native view, Fuzzbit mechanism) | ☐ |

**L2 what shipped so far:** `crates/kn9t-tui/src/file_index.rs` — the single index (D6) that the tree,
the viewer and the `@` dropdown will all read. 13 tests. Built for the hot path (it re-searches on
every keystroke), with the three things a naive version gets wrong:

| property | why |
|---|---|
| one arena (`paths` + an ASCII-lowercased mirror `lower`) with `(u32,u32)` ranges | 100k paths = 2 allocations, and scoring walks memory in order. A `String` per file per search was the dominant cost |
| the lowercase mirror is built **at index time** | `to_lowercase()` per file per keystroke was the invisible killer; ASCII-only so the two buffers stay byte-aligned and share one range |
| top-k bounded heap, not a full sort | O(n log k) per keystroke instead of O(n log n); one allocation of size `limit` |
| frecency (hits + last-used tick) | ranks what you actually open, like VS Code's recently-opened list but applied to every result |
| subsequence match + contiguous-run reward + gap penalty | `ktsrc` finds `crates/kn9t-tui/src`; `module1999` ranks above `module199` |

Reference for the design: <https://github.com/dmtrKovalenko/fff> (resident index, one walk reused,
frecency, arena storage). The *crate* is **not** a dependency — see the open decision below.

**Open decision, raised not taken:** `fff-search` is an MIT Rust crate that does all of the above
plus git-aware results, a background watcher and SIMD matching. Adopting it means a new dependency
(AGENTS §8 requires a DESIGN §15 justification + changelog note) and replacing `file_index.rs`.
The hand-rolled index exists because it needs no new dependency and covers what the three surfaces
need; the crate would be the right call if the watcher/git-status/content-grep features are wanted.
Whoever picks this up should ask rather than assume.

**L1 what shipped** (`crates/kn9t-tui`, all verified):

| change | where | test |
|---|---|---|
| mascot palette + `ink` slot (text on a colour block) + `panel_bg` slot (overlay surfaces) | `src/theme.rs` | `unit_theme` 16, rewritten off literals |
| session tabs + breadcrumb `cwd ▸ title ▸ model` replace the one-row header | `assets/default_tui.lua` | `chrome_layout::header_is_tab_bar_over_breadcrumb` |
| `cwd` published to Lua (`kn9t.state.session.cwd`) | `src/lua/state.rs` | (part of the above) |
| segmented status bar, context chip as a solid block | `assets/default_tui.lua` | `chrome_layout::status_bar_is_segmented` |
| `recent calls` dropped; leftover space is a spacer | `assets/default_tui.lua` | `chrome_layout::right_panel_drops_recent_calls` |
| session column removed (sessions are tabs) | `assets/default_tui.lua` | `chrome_layout::there_is_no_session_column` |
| bordered prompt, cursor scrolling, `INPUT_CHROME_COLS` shared with the height math | `src/ui/render.rs`, `src/ui/layout.rs` | `chrome_layout::input_is_a_boxed_prompt`, `tiny_input_falls_back_to_a_bare_prompt` |
| tool cards grouped per turn: rollup header, one compact line per call, diff badge, group toggle | `src/ui/render.rs`, `src/app.rs`, `src/render_cache.rs` | `chrome_layout::a_turn_groups_its_tool_calls` |
| overlays themed (86 hardcoded `Color::Black` cells) + rounded frame | `src/ui/render.rs` | `chrome_layout::overlays_are_themed_and_framed` |
| render harness: built-in UI → `TestBackend` → text, no server needed | `tests/chrome_layout.rs` | 9 tests |

**L1 deferrals, recorded rather than hidden:**

1. **The tab bar switches, it does not multiplex.** Tabs read the session cache and call
   `switch_session`; one session is live at a time (one SSE stream). N concurrent streams need the
   `SessionView` refactor, which is the plan's largest non-visual item and was not started.
2. **No per-turn duration in the group header.** `ToolCard` carries no timing, and a fabricated
   `· 1.2s` was refused. Needs `started_at`/`finished_at` threaded from the SSE handler.
3. **Overlays are themed and framed but not repositioned.** The VS Code positions (quick-open at
   the top, full-screen help) are not done; the pickers are still centred.

Also: the verification used a **renamed-aside** personal config
(`~/.kn9t/tui` → `~/.kn9t/tui.disabled-20260916`), not a `kn9t tui reset`. The files are intact;
restoring is a `Rename-Item` back.

Spec work that must land with the matching lot, not after it:

- **R-TUI-100 rewrite** — `@path` is text only (D19); the CHANGELOG 2026-09-16d entry is the record.
- **New requirements**: session tabs · `explorer` / `viewer` native views · `@` finder ·
  terminal exec endpoint · keybinds `Ctrl+Tab`, `Ctrl+W`, `Ctrl+J`.
- **DESIGN note**: the native-vs-plugin dividing line (AGENTS §14) — *a view over host state is
  native; a view over external data is a plugin*.

---

## Overall progress

| stage | crate(s) | reqs done / total | gate | status |
|---|---|---|---|---|
| 01 | kn9t-core | 36 / 36 | R-CORE-900 | ☑ |
| 02 | kn9t-provider-replay | 8 / 9 | R-RPLY-900 | ☑ |
| 03 | kn9t-react, kn9t-tools | 25 / 25 | G1 | ☑ (classifier restored in `kn9t-server/src/classify.rs`, 3 classify + `tool::`/`policy::*` green) |
| 04 | kn9t-store | 18 / 18 | G2 | ☑ |
| 05 | kn9t-provider-core, -openai | 25 / 25 | R-PCORE/OAI/NBED-900 | ☑ |
| 06 | kn9t-server | 16 / 16 | R-SRV-900 | ☑ (+R-SRV-CFG-100/110 config hot-reload, +R-SRV-CFG-030 discover) |
| 07 | kn9t-tui | 2 / 27 | G3 | ▣ (most reqs have no test) · P7 polish epic in progress (`PLAN.md` §P7) |
| 08 | kn9t-plugin | 13 / 13 | R-PLUG-900 | ☑ |
| 08b | kn9t-plugin-sdk, kn9t-plugin (v2), internal-plugins/kn9t-tools | 12 / 12 | R-PLUG2-900 | ☑ |
| 09 | plugins/kn9t-custom-provider (external), kn9t-anthropic (bundled), RemoteProvider | 17 / 17 | R-CP/ANTH-900 | ☑ |
| 10 | bedrock-native, gemini (v2) | 0 / 8 | R-BEDN/GEM-900 | ☐ |

**v1 release = stages 01–09 gates green.**

---

## Open bugs found 2026-09-06 (config-reload session) — not yet fixed

Found while implementing R-SRV-CFG-100/110. None are regressions from that work; all are
pre-existing. Narrative + evidence in `CHANGELOG.md` under the 2026-09-06 session.

| # | severity | area | bug | status |
|---|---|---|---|---|
| B2 | **high** | kn9t-store | Compaction threshold compares `ctx_window * 0.80` against `total_est` = `bytes/4` (`project.rs:70`), not against real usage. Observed live: session at 162.2k reported tokens on a 128k-window model never compacts, because `total_est` stays under the 102.4k threshold. `bytes/4` under-reads BPE, ignores cache-replayed history, counts non-text tool results as 20. **Decide which figure is authoritative** (`UsageRecorded` vs `est_tokens`) before patching. | ☐ |
| B3 | low | kn9t-tui | ~~`assets/default_tui.lua` hardcoded `CONTEXT_WINDOW = 200000`.~~ **FIXED 2026-09-07:** `ctx_window`/`max_out` were being dropped by `wire.rs::ModelInfo`; now carried through `ModelEntry` into `kn9t.context.ctx_window`, and the built-in gauge reads it (falling back to 200k only when a provider reports no window). Covered by `lua_api_contract::context_window_is_published_when_known`. | DONE |
| B4 | low | kn9t-server | No per-family `ctx_window` fallback table, though `model_max_output` (`config.rs:1025`) does exactly that for `max_out` (opus 32000 / sonnet 65536 / haiku 8192). Gateways omitting `context_window` get a flat 128k for every model. | ☐ |
| B1 | medium | kn9t-core | `tests/mojibake.rs` (96E-15) only detects the `§` and `—` double-encode forms; box-drawing `──` mojibake passes it. A green run is not proof a file is clean. Consider delegating to `scripts/fix_mojibake.py --check`, which is form-agnostic. | ☐ |

---

## Per-stage requirement + test status

The **test** column is the acceptance test named in the spec (`**Accept:** cargo test
<name>`). A requirement is `☑` only when its test passes.

### Stage 01 — kn9t-core  (`spec/01-core.md`)  — DONE (gate R-CORE-900 green)
All 36 requirements implemented; 13 named `core::*` acceptance tests pass; build clean under
`-D warnings`; GI-1/2/3/5 verified; `cargo doc` clean. Statuses below all ☑.
| req | subject | test | status |
|---|---|---|---|
| R-CORE-010 | dep set = serde/serde_json only | ci: cargo-toml dep set | ☑ |
| R-CORE-020 | no async/await/tokio | ci: grep + cargo tree | ☑ |
| R-CORE-030 | event payloads are pure data | core::payload_is_pod | ☑ |
| R-CORE-040 | identifier newtypes | core::id_serde | ☑ |
| R-CORE-045 | ULID new(), monotonic | core::ulid_monotonic | ☑ |
| R-CORE-050 | Role, Message | (compile) | ☑ |
| R-CORE-060 | Content flat enum, tagged | core::content_tag | ☑ |
| R-CORE-062 | args_json verbatim | core::args_verbatim | ☑ |
| R-CORE-064 | Thinking persisted w/ signature | core::thinking_roundtrip | ☑ |
| R-CORE-070 | ModelRef, ModelSpec | (compile) | ☑ |
| R-CORE-080 | Price four tiers | (compile) | ☑ |
| R-CORE-090 | Effort, Thinking | (compile) | ☑ |
| R-CORE-095 | Quirks / ThinkingReplay | (compile) | ☑ |
| R-CORE-100 | Tokens partition, Usage | core::tokens_default_zero | ☑ |
| R-CORE-110 | StopReason | (compile) | ☑ |
| R-CORE-120 | ToolSpec, ordered schema | (compile) | ☑ |
| R-CORE-130 | ProvErr variants | (compile) | ☑ |
| R-CORE-135 | StoreErr, ToolErr | (compile) | ☑ |
| R-CORE-140 | Event enum, tagged | core::event_tag | ☑ |
| R-CORE-142 | UsageRecorded.estimated | (compile) | ☑ |
| R-CORE-145 | seq()/is_durable() | core::seq_partition | ☑ |
| R-CORE-150 | UsageKind | (compile) | ☑ |
| R-CORE-155 | HookName (8 variants) | (compile) | ☑ |
| R-CORE-160 | ForkReason, ForkSnapshot | core::fork_snapshot_serde | ☑ |
| R-CORE-170 | Request (single def, cache, session) | (compile) | ☑ |
| R-CORE-180 | Chunk enum | (compile) | ☑ |
| R-CORE-190 | Provider trait | (compile) | ☑ |
| R-CORE-200 | Cache, CacheMode | (compile) | ☑ |
| R-CORE-210 | breakpoints() | core::breakpoints | ☑ |
| R-CORE-220 | Bus, non-blocking, bounded | core::bus_never_blocks | ☑ |
| R-CORE-225 | bus is not persistence | (review) | ☑ |
| R-CORE-230 | EventSink trait | (compile) | ☑ |
| R-CORE-240 | Cancel | core::cancel_wakes | ☑ |
| R-CORE-250 | Store trait, RequestPlan | (compile) | ☑ |
| R-CORE-260 | Tool trait, ToolCtx | (compile) | ☑ |
| R-CORE-270 | Policy trait, Decision | (compile) | ☑ |
| **R-CORE-900** | **stage gate** | build -Dwarnings + all core::* + GI-1/2/3/5 | ☑ |

### Stage 02 — kn9t-provider-replay  (`spec/02-replay.md`)  — DONE (gate R-RPLY-900 green)
8/9 requirements done; R-RPLY-070 (re-point the inline SSE splitter at
`kn9t-provider-core`) is deferred to stage 05 by design and tracked there.
10 named `rply::*` acceptance tests pass; build+doc clean under `-D warnings`;
GI-1 (one workspace dep = kn9t-core) and GI-5 verified; runs with no net, no key.
| req | subject | test | status |
|---|---|---|---|
| R-RPLY-010 | fixture header + verbatim body | rply::header_parse | ☑ |
| R-RPLY-015 | chunked delivery annotation | rply::chunk_boundary | ☑ |
| R-RPLY-020 | fixtures redact secrets | rply::recorder_redacts_secrets | ☑ |
| R-RPLY-030 | ReplayProvider yields chunks | rply::yields_expected_chunks | ☑ |
| R-RPLY-035 | status→ProvErr pre-stream | rply::status_maps_to_prestream_error | ☑ |
| R-RPLY-040 | ProvErr classification fixtures | rply::classification_fixtures_exist_and_map | ☑ |
| R-RPLY-050 | --record recorder | rply::record_roundtrip | ☑ |
| R-RPLY-070 | re-point at PCORE (stage 5) | rply::* still green post-re-point | ☑ |
| **R-RPLY-900** | **stage gate** | cargo test, no net, no key | ☑ |

### Stage 03 — kn9t-react + kn9t-tools  (`spec/03-react-tools.md`)  — DONE (gate G1 green)
25 requirements: 24 ☑ + 1 ⧗ (R-RCT-120 host-queue-first is implemented; its full
interleave test requires the plugin host from stage 08; spec explicitly defers it there).
All named `rct::*` (9) and `tool::*` (8) acceptance tests pass; GI-1/3/5 verified.
DB-02 resolved: assemble delegated to kn9t-provider-core (kn9t-react dep swapped to pcore).
| req | subject | test | status |
|---|---|---|---|
| R-RCT-010 | loop owns only traits | (compile + GI-1) | ☑ |
| R-RCT-020 | turn sequence | rct::turn_sequence | ☑ |
| R-RCT-030 | TurnStarted/TurnEnded | (part of turn_sequence) | ☑ |
| R-RCT-040 | Cancel per turn, boundary checks | rct::cancel_boundary | ☑ |
| R-RCT-050 | abort in stream | rct::abort_in_stream | ☑ |
| R-RCT-060 | abort in tools, close orphans | rct::abort_in_tools | ☑ |
| R-RCT-070 | truncation ladder | rct::truncation_ladder | ☑ |
| R-RCT-080 | overflow→compaction, length ok | (part of compaction test) | ☑ |
| R-RCT-090 | compaction re-plan once | rct::compaction_replan_once | ☑ |
| R-RCT-095 | compaction usage kind | (part of replan test) | ☑ |
| R-RCT-100 | HookHost (trait in core) | (compile) | ☑ |
| R-RCT-110 | per-hook failure posture | rct::hook_posture | ☑ |
| R-RCT-120 | host-queue-first steering | (full interleave test in 08) | ⧗ (impl done; test at 08) |
| R-RCT-130 | parallel read, call-order persist | rct::parallel_order | ☑ |
| R-TOOL-010 | ordered registry | tool::spec_order_stable | ☑ |
| R-TOOL-020 | v1 tools, hand-written schema | (compile) | ☑ |
| R-TOOL-030 | content truncation only | tool::truncation | ☑ |
| R-TOOL-040 | read parallel-safe, records hash | tool::read_records_hash | ☑ |
| R-TOOL-050 | edit staleness guard | tool::edit_guard | ☑ |
| R-TOOL-060 | write guard | tool::write_guard | ☑ |
| R-TOOL-070 | bash defers to policy, kill on cancel | `kn9t-server --test classify` + `policy::*` | ☑ (restored in `kn9t-server/src/classify.rs`, ADR-0001) |
| R-TOOL-080 | pwsh + POSIX classifiers | `classify_posix`, `classify_pwsh` | ☑ (`crates/kn9t-server/tests/classify.rs:18,28` 3 passed) |
| R-TOOL-090 | classifier decision pipeline | `classify_pipeline` | ☑ (7-rule pipeline, `sh -c`/`iex` bypass Ask) |
| R-TOOL-095 | heuristic not sandbox | (doc in `classify.rs:1`) | ☑ (R-TOOL-095 documented, not a sandbox) |
| **R-RCT-900 / R-TOOL-900** | **GATE G1** | full loop vs replay, no net/spend | ☑ (`cargo test -p kn9t-react -p kn9t-server` green; replay + risk seam green) |

> **2026-08-31 update (superseded):** The classifier was restored to
> `crates/kn9t-server/src/classify.rs` (333 lines, per ADR-0001 server owns approval) with
> `BashPolicy` + `Shell::Posix|PowerShell` + `classify()` and 3 acceptance tests. **ADR-0008
> then deleted it again, permanently**, moving judgement to a policy plugin — so this note
> describes a state that no longer exists and is kept only for the history. `classify_posix`/
> `classify_pwsh`/`classify_pipeline` are gone; G1's risk-seam leg is now
> `cargo test -p kn9t-plugin plug::composition` plus
> `cargo test -p kn9t-server --test acceptance approve` (96E-31). `AllowPolicy` was replaced
> by `ConfigPolicy`/`InteractivePolicy`, which emit `ApprovalRequest` per DESIGN §10.

### Stage 04 — kn9t-store  (`spec/04-store.md`)  — DONE (gate G2 green)
All 18 requirements implemented; 18 named `stor::*` acceptance tests pass (plus 1 debug helper); build clean; GI-1/GI-4 verified; no tokenizer dep.
| req | subject | test | status |
|---|---|---|---|
| R-STOR-010 | WAL pragmas | stor::pragmas | ☑ |
| R-STOR-020 | many readers, one writer | WAL mode, single Mutex writer conn | ☑ |
| R-STOR-030 | schema DDL exact | stor::schema_matches | ☑ |
| R-STOR-040 | append assigns seq in txn | stor::append_assigns_seq | ☑ |
| R-STOR-050 | reject transient, no update events | stor::append_rejects_transient | ☑ |
| R-STOR-060 | project() total | stor::project_is_total | ☑ |
| R-STOR-070 | cost tiered at write time | stor::cost_tiered | ☑ |
| R-STOR-080 | reproject rebuilds | stor::reproject_rebuilds | ☑ |
| R-STOR-090 | reproject --check clean | stor::reproject_check_clean | ☑ |
| R-STOR-100 | plan_request, no tokenizer | stor::plan_no_tokenizer | ☑ |
| R-STOR-110 | compaction boundary snap | stor::compact_boundary | ☑ |
| R-STOR-115 | close orphaned tool calls in the fold | stor_orphan_from_interrupted_tool_execution | ☑ |
| R-STOR-117 | repair unparseable args_json in the fold | stor_plan_repairs_unparseable_tool_args | ☑ |
| R-STOR-120 | linear session, fork = new row | (part of fork tests) | ☑ |
| R-STOR-130 | fork copies ctx not usage | stor::fork_no_usage, stor::fork_renumber | ☑ |
| R-STOR-140 | blob put/get content-addressed | stor::blob_dedup | ☑ |
| R-STOR-150 | blob refcount | stor::blob_refcount | ☑ |
| R-STOR-160 | session delete + blob decrement | stor::session_delete_blobs | ☑ |
| R-STOR-170 | live_messages non-canonical | stor::live_truncated_on_open | ☑ |
| R-STOR-180 | cost rollup query | stor::cost_rollup | ☑ |
| **R-STOR-900** | **GATE G2** | all stor::* pass; reproject_check clean; no tokenizer | ☑ |

### Stage 05 — provider-core + openai + litellm-gateway  (`spec/05-provider-core-openai.md`)  — DONE (gate green)
All 25 requirements implemented; acceptance tests pass (`pcore::*`, `oai::*`, `nbed::*`). GI-1/GI-5 hold. One SHOULD deviation (tls_insecure) recorded in CHANGELOG.
| req | subject | test | status |
|---|---|---|---|
| R-PCORE-010 | blocking http client | pcore::connect_timeout | ☑ |
| R-PCORE-020 | connect-only timeout | pcore::connect_timeout | ☑ |
| R-PCORE-030 | auth scheme data | pcore::auth_scheme | ☑ |
| R-PCORE-035 | tls default secure | pcore::tls_default_secure | ☑ |
| R-PCORE-040 | sse_lines boundary buffering | pcore::sse_boundary | ☑ |
| R-PCORE-050 | assemble, verbatim args + truncation gate | pcore::assemble_verbatim_args, pcore_assemble_rejects_incomplete_args, pcore_assemble_accepts_argless_call | ☑ |
| R-PCORE-060 | retry pre-stream only | pcore::retry_pre_stream, pcore::retry_no_retry_after_chunk | ☑ |
| R-PCORE-070 | cache encoding scaffold | oai::cache_automatic_omits, oai::cache_explicit_places | ☑ |
| R-PCORE-080 | Quirks merge | pcore::quirks_merge | ☑ |
| R-PCORE-090 | hand-written prices required | pcore::model_prices_required | ☑ |
| R-PCORE-100 | --dump-request | build_request dump_request=true (eprintln) | ☑ |
| R-OAI-010 | request shape + quirks | oai::request_shape_default_quirks, oai::request_shape_completion_tokens_quirk | ☑ |
| R-OAI-020 | decode sse | oai::decode_text_stream, oai::decode_tool_call_stream, oai::decode_reasoning_stream | ☑ |
| R-OAI-030 | toolcall correlate by id | oai::toolcall_correlate | ☑ |
| R-OAI-040 | cache automatic omit / explicit place | oai::cache_automatic_omits, oai::cache_explicit_places | ☑ |
| R-OAI-050 | extra-headers hook + `session_header` injection | oai::extra_headers, oai::session_header | ☑ |
| R-OAI-060 | responses request shape (`instructions`/`input`/flat tools/`store:false`) | oai::responses_request_shape, oai::responses_tools_are_flat | ☑ |
| R-OAI-070 | responses SSE decode + usage partition | oai::responses_decode_text_then_usage, oai::responses_decode_tool_call, oai::responses_ignores_unknown_events, oai::responses_incomplete_is_length | ☑ |
| R-NBED-010 | kind=openai /v1 gateway | base_url=https://llm-gateway.example.com/v1 in OpenAiConfig | ☑ |
| R-NBED-020 | identity precedence | nbed::identity_precedence | ☑ |
| R-NBED-030 | preflight cache + invalidate | nbed::preflight_cache_and_invalidate | ☑ |
| R-NBED-040 | /user/usage ground truth | OpenAiProvider::user_usage() helper (consumed by stage 06) | ☑ |
| R-NBED-050 | three rewrites | nbed::rewrites_adaptive_thinking, nbed::rewrites_placeholder_tool | ☑ |
| R-NBED-060 | dual-field usage decode | nbed::usage_fields | ☑ |
| R-NBED-070 | 1M model pair | nbed::onem_pair | ☑ |
| **R-PCORE/OAI/NBED-900** | **stage gate** | all 25 tests pass; no tokio; GI-1 holds | ☑ |

### Stage 06 — kn9t-server  (`spec/06-server.md`)  — ☑
| req | subject | test | status |
|---|---|---|---|
| R-SRV-010 | http surface | srv::routes | ☑ |
| R-SRV-015 | tiny_http, no tokio | (GI-5) | ☑ |
| R-SRV-020 | auth mandatory | srv::auth_required | ☑ |
| R-SRV-030 | origin rejected | srv::origin_rejected | ☑ |
| R-SRV-040 | SSE subscribe→read→dedup | srv::sse_no_gap_no_dup | ☑ |
| R-SRV-050 | live_messages on attach | (part of sse test) | ☑ |
| R-SRV-060 | single-writer lease + takeover | srv::lease_single_writer | ☑ |
| R-SRV-070 | auto-spawn race-free | srv::spawn_race | ☑ |
| R-SRV-080 | idle exit | srv::idle_exit | ☑ |
| R-SRV-090 | blob roundtrip + ETag | srv::blob_roundtrip | ☑ |
| R-SRV-100 | auto-title best-effort | srv::autotitle | ☑ |
| R-SRV-110 | cost query | srv::cost_query | ☑ |
| R-SRV-120 | budget reports both | srv::budget_reports_both | ☑ |
| R-SRV-CFG-100 | `POST /config/reload` swaps providers+models in place; previous config kept on error | srv::config_reload_keeps_previous_on_bad_config, srv::config_reload_endpoint_routed, srv::config_reload_swaps_ctx_window | ☑ |
| R-SRV-CFG-110 | config.toml watcher auto-reloads on change, debounced against partial writes | watch::tests::* (5) | ☑ |
| R-SRV-CFG-030 | `discover = false` suppresses `/models` and plugin-catalog auto-discovery | srv::config_discover_false | ☑ |
| **R-SRV-900** | **stage gate** | all above | ☑ |

### Stage 07 — kn9t-tui  (`spec/07-tui.md`)  — ▣ (G3 manual deferred, Phase 4 in progress)

> **2026-08-30 arch review:** The table below is realigned to spec/07-tui.md (R-TUI-010
> through R-TUI-240). Previous table used obsolete IDs. Most TUI requirements have NO
> acceptance test — app.rs, client.rs, ui/render.rs, wire.rs, event.rs, config.rs,
> keybind.rs are entirely untested. The crate has 58 unit tests, but they cover managers
> and helpers, not the core TUI logic.
> **2026-08-31 Phase 4:** `GET /tools` + `rename`/`compact`/`export` added (schema-first, server is source of truth), hardcoded 4-tool list removed, dead `enabled` toggle removed (now refreshes from `GET /tools`), `/diff` fixed to use `session.state.cwd` (F9), `ThinkingDelta`/`ModelChanged`/`Compacted` now handled in `handle_sse` + `reducer.rs` pure reducer (8 tests, first real `app.rs` logic tests), `pending_*` eliminated (`staged`/`active`, zero `pending_` fields), `queued_*` eliminated 2026-08-31 (handlers now take `&Sender<Event>` and act immediately; `App::run` deferred block deleted; `grep -rn queued_ app.rs` empty), `tui::sse_reconnect` now passes (reconnect from `last_seq`). `reducer.rs` is the pure `(State, SseFrame)->State` seam for 4.4a.

| req | subject | test | status |
|---|---|---|---|
| R-TUI-010 | links no workspace crate (GI-6) | ci: cargo tree | ☑ |
| R-TUI-011 | env vars KN9T_URL/TOKEN/MODEL | tui::env_vars | ☐ no test |
| R-TUI-012 | wire types match server API | manual | ▣ (drift exists; see ADR-0005) |
| R-TUI-020 | event architecture, zero polling | tui::event_loop_blocks | ☐ no test |
| R-TUI-030 | 2-column layout, responsive | tui::layout_responsive | ☐ no test |
| R-TUI-040 | session picker overlay | tui::session_switch | ▣ (96E-19: `session_filter_matches_id_by_substring_only` + live `/session` filter→Enter verified; named test still absent) |
| R-TUI-050 | right sidebar context panel | tui::tool_toggle | ▣ (Phase 4: `GET /tools` reflects discovered plugins, hardcoded list + dead toggle removed; `refresh_tools` in `app.rs:233`) |
| R-TUI-060 | transcript, tool cards | tui::tool_card_lazy | ▣ (96E-18/19: `reducer::live_tool_call_roundtrip_creates_card` + TranscriptParser tests + live TUI (cards visible live & on reload); named test still absent) |
| R-TUI-070 | virtual scrolling | tui::virtual_scroll | ☐ no test |
| R-TUI-080 | scroll behavior, auto-disengage | tui::scroll_auto_disengage | ☐ no test |
| R-TUI-090 | input box multiline | tui::input_multiline | ☐ no test |
| R-TUI-100 | file mentions @path | tui::file_mention_autocomplete | ☐ no test |
| R-TUI-110 | image paste | tui::image_paste | ▣ (impl exists; no test) |
| R-TUI-120 | status bar streaming | tui::status_bar_streaming | ☐ no test |
| R-TUI-130 | approval overlay | tui::approval_blocks_input | ▣ (overlay renders but is dead code — nothing emits ApprovalRequest) |
| R-TUI-140 | diff viewer | tui::diff_comment | ☐ no test |
| R-TUI-150 | help overlay | tui::help_shows_bindings | ☐ no test |
| R-TUI-160 | keybindings configurable | tui::keybind_override | ☐ no test |
| R-TUI-170 | mouse support | tui::mouse_hover_sidebar | ☐ no test |
| R-TUI-180 | theming | tui::theme_override | ☐ no test |
| R-TUI-190 | error display persisted | tui::error_persisted | ☐ no test |
| R-TUI-200 | confirmations | tui::quit_confirm | ☐ no test |
| R-TUI-210 | git integration sidebar | tui::git_sidebar | ☐ no test |
| R-TUI-220 | plugin sidebar API (SidebarWidget) | tui::plugin_sidebar | ✗ (SidebarWidget type does not exist) |
| R-TUI-230 | SSE reconnect | tui::sse_reconnect | ☑ (Phase 4: `app.rs:630` reconnects from `last_seq`, `reducer::tests::sse_reconnect_seq_tracking` + `tui::sse_reconnect` green) |
| R-TUI-240 | server autostart | tui::server_autostart | ☐ no test |
| **R-TUI-900** | **GATE G3** | 3 TUIs, 1 server, 1 lease, screenshot | — |

### Stage 08 — kn9t-plugin  (`spec/08-plugin.md`)  — DONE (gate R-PLUG-900 green)
All 13 requirements implemented; 8 named `plug::*` acceptance tests pass; GI-1 verified (only `kn9t-core` workspace dep).
| req | subject | test | status |
|---|---|---|---|
| R-PLUG-010 | HookHost in core (GI-1) | ci: GI-1 both crates | ☑ |
| R-PLUG-020 | subprocess not dylib | (design: PluginHost accepts io) | ☑ |
| R-PLUG-030 | no per-delta hook | (design: hook surface has 8, none per-delta) | ☑ |
| R-PLUG-040 | handshake | plug::handshake | ☑ |
| R-PLUG-050 | RemoteTool | plug::handshake (registers tools) | ☑ |
| R-PLUG-060 | 8 hooks surface | plug::hook_surface | ☑ |
| R-PLUG-065 | dropped hooks absent | (design: only 8 hook variants exist) | ☑ |
| R-PLUG-070 | composition classes | plug::composition | ☑ |
| R-PLUG-080 | per-hook timeouts | plug::timeout | ☑ |
| R-PLUG-090 | HookFailed + 3-fail unsubscribe | plug::hook_surface | ☑ |
| R-PLUG-100 | project [[plugin]] ignored | plug::project_plugin_ignored | ☑ |
| R-PLUG-110 | sub-agent = session forkée via ops host_api (`session_fork`/`session_prompt`, 96E-17: tool built-in supprimé — les plugins fournissent `spawn_session`) | `srv::p1_96e17_session_fork_and_prompt_spawns_a_real_child` | ☑ |
| R-PLUG-120 | toolset enfant configurable (`tools` param de `session_prompt` + `filter_names`) | `srv::p1_96e17_host_api_ops_*` | ☑ |
| R-PLUG-130 | budget cap enforced (ForkSnapshot + `run_session_turn`) | `srv::p1_96e17_session_fork_and_prompt_spawns_a_real_child` | ☑ |
| **R-PLUG-900** | **stage gate** | all above | ☑ |

### Stage 08b — plugin protocol v2  (`spec/08b-plugin-redesign.md`)  — ☑ DONE (R-PLUG2-900 green)

Implementation complete (2026-08-26). All 10 `plug2::*` acceptance tests + 9 doc tests pass. Supersedes wire protocol from 08.

| req | subject | test | status |
|---|---|---|---|
| R-PLUG2-010 | kn9t-plugin-sdk zero workspace deps | cargo tree check | ☑ |
| R-PLUG2-020 | internal-plugins/kn9t-tools deps sdk only, compiles to binary | cargo tree check | ☑ |
| R-PLUG2-030 | kn9t-plugin host retains kn9t-core only (GI-1) | ci | ☑ |
| R-PLUG2-040 | chunk/done used for streaming calls; ordering guaranteed | plug2::streaming_tool_chunks_then_done | ☑ |
| R-PLUG2-050 | cancel message stops in-flight call; done error reply | plug2::cancel_in_flight | ☑ |
| R-PLUG2-060 | provider chunk kinds match schema; unknown kinds ignored | plug2::provider_chunks_assembled | ☑ |
| R-PLUG2-070 | cancel listener thread never blocked by dispatch | plug2::cancel_does_not_block_dispatch | ☑ |
| R-PLUG2-080 | SDK is blocking throughout; no async/tokio | CI grep | ☑ |
| R-PLUG2-090 | every public SDK item has doc comment; root has module doc | cargo doc clean | ☑ |
| R-PLUG2-095 | each SDK trait has doc example | cargo test --doc | ☑ |
| R-PLUG2-100 | hot-reload cancels in-flight, re-handshakes | plug2::hot_reload_cancels_inflight | ☑ |
| R-PLUG2-110 | kn9t-tools auto-spawned at startup; missing binary = startup fail | plug2::autostart_tools_plugin | ☑ |
| R-PLUG2-120 | bash streams chunk progress; cancel stops child | plug2::bash_streams_progress | ☑ |
| R-PLUG2-130 | read-tracking inside kn9t-tools process; edit detects stale read | plug2::edit_detects_stale_read | ☑ |
| **R-PLUG2-900** | **stage gate** | all above | ☑ |

### Stage 09 — anthropic (`spec/09-anthropic.md`) + custom plugin (`spec/09a-custom-provider.md`)  — ☑ DONE (R-CP/ANTH-900 green)

Both providers ship as subprocess plugin binaries (Q31). `RemoteProvider` in `kn9t-plugin` adapts the stream into `Provider`. 10 custom-provider + 5 anth acceptance tests pass. R-ANTH-050 was added 2026-09-16c: R-ANTH-030's mapping was dead and tools were never translated, both hidden by fixtures that did not match what the host sends.

| req | subject | test | status |
|---|---|---|---|
| R-CP-005 | kn9t-custom-provider is an external standalone crate in plugins/, not a workspace member | standalone `cargo build` + `cargo test` in plugins/kn9t-custom-provider; check-gi1.sh | ☑ |
| R-CP-010 | version gate, no fallback | cp::version_gate | ☑ |
| R-CP-020 | vision disabled errors | cp::vision_disabled_errors | ☑ |
| R-CP-030 | auth token scheme | (part of message_map) | ☑ |
| R-CP-040 | speaker/content mapping | cp::message_map | ☑ |
| R-CP-050 | four mapping rules | cp::mapping_rules | ☑ |
| R-CP-060 | custom body field names | cp::body_fields | ☑ |
| R-CP-070 | delta_tool_calls index bug | cp::parallel_toolcalls | ☑ |
| R-CP-080 | text tool calls off | cp::text_tool_off | ☑ |
| R-CP-090 | usage sum (uncached) | cp::usage_sum | ☑ |
| R-CP-100 | error classification | cp::error_classify | ☑ |
| R-CP-110 | cache part-level placement | cp::cache_part_level | ☑ |
| R-CP-120 | config + model catalog | (hand-written catalog in main.rs) | ☑ |
| R-ANTH-010 | messages api decode | anth::decode | ☑ |
| R-ANTH-020 | thinking verbatim signature | anth::thinking_verbatim | ☑ |
| R-ANTH-030 | cache message-level priority order | anth::cache_priority_order | ☑ |
| R-ANTH-040 | usage partition, min_tokens | anth::usage_partition | ☑ |
| R-ANTH-050 | `ToolSpec` → `input_schema` translation | map::tests::tools_are_translated_to_input_schema | ☑ |
| **R-CP-900 / R-ANTH-900** | **stage gate** | all above | ☑ |

### Stage 10 — native bedrock + gemini (v2)  (`spec/10-bedrock-native-v2.md`)  — ☐  *(v2, not a v1 gate)*
| req | subject | test | status |
|---|---|---|---|
| R-BEDN-010 | SigV4 signer | (v2) | — |
| R-BEDN-020 | eventstream decode | bedn::eventstream_decode | — |
| R-BEDN-030 | cachePoint appended | bedn::cachepoint_appended | — |
| R-BEDN-040 | cache usage fields | (v2) | — |
| R-BEDN-050 | 1h TTL opt-in | (v2) | — |
| R-BEDN-060 | automatic cache_control | (v2) | — |
| R-GEM-010 | cached-content resource | (v2) | — |
| R-GEM-020 | generateContent decode | gem::decode | — |
| **R-BEDN-900 / R-GEM-900** | **v2 stage gate** | all above | — |

---

## SPEC-OPEN resolution register

When you resolve a SPEC-OPEN (pick or change a value), fill its row here **and** update the
spec file + `spec/README.md`. Interim values ship until changed.

| id / topic | spec ref | interim value | resolved value | date / session |
|---|---|---|---|---|
| cache TTL | 05, §8.4.2.2 | 5 min | — | — |
| server idle-exit | 06 R-SRV-080 | 30 min | — | — |
| truncation give-up count | 03 R-RCT-070 | 4 | — | — |
| truncation reminder ladder | 03 R-RCT-070 | 150/100/50/25/10 | — | — |
| compaction threshold | 04 R-STOR-110 | 0.80 × ctx | — | — |
| lease idle timeout | 06 R-SRV-060 | 5 min | — | — |
| LDAP check TTL | 05 R-NBED-030 | 12 h | — | — |
| connect timeout | 05 R-PCORE-020 | 20 s | — | — |
| plugin unsubscribe count | 08 R-PLUG-090 | 3 | — | — |
| session-delete of live-fork origin | 04 R-STOR-160 | reject | — | — |
| compaction prompt wording | 04 / §18.1 | fixed template (unfrozen) | — | — |
| custom provider model catalog disk cache | 09 R-CP-120 | fetch per process | — | — |
| budget drift warning | 06 R-SRV-120 | report both, no warn | — | — |
| cache-hit reporting in `kn9t cost` | §18.11 | not surfaced | — | — |
| compaction-boundary snap for ② | §18.10 | no snap | — | — |
| BEDN SigV4 transport crate | 10 R-BEDN-010 | undecided (v2) | — | — |
| GEM cached-content lifecycle | 10 R-GEM-010 | undecided (v2) | — | — |
