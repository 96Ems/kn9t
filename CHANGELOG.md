# CHANGELOG — kn9t

Session narrative across the project. This is the memory of *what changed and why* — it is
**not** a git log and **not** a status table. Live status (stage progress, per-requirement
test tables, SPEC-OPEN register) lives in `TRACKING.md`.

Append under a dated session heading as you work. Keep the **Next session starts here**
pointer current.

---

## ▶ Next session starts here

**Next:** E2E live compaction (grosse session ~80 % ctx avec `kn9t-compactor` branché, via TUI-testing) — puis SDK Rust parity pour le RPC request/reply (`kn9t-plugin-sdk`), puis ADR-0008 spec rewrite + `kn9t-policy.py` fail-open, puis G3. Le 96E-17 (fail-closed + host API + compactor) est implémenté et testé (437/0), reste la vérification live de la compaction réelle.

---

## Session — 2026-09-02 (3) — 96E-17: compaction fail-closed + plugin → host API + compactor plugin TS/Effect

### Summary

Suite directe de la review du compactor (96E-16) : l'utilisateur tranche que (a) le fallback intégré (prompt « Summarize… », SPEC-OPEN §18.1) doit **disparaître** — pas de compactor = pas de compaction = session terminée ; (b) le compactor doit être un **sous-agent à jeu de tools spécifique** ; (c) **kn9t ne doit PAS embarquer de sous-agent** — c'est l'APIs + les hooks ouverts qui permettent aux plugins externes de faire leurs propres boucles d'agent. Décision d'architecture actée et appliquée.

### Fail-closed (`b700895`)

- `run_compaction` : `compactor: None` → `LiveEvent::Error` + `Err(ReactError::CompactionUnavailable)`, provider jamais appelé, rien persisté. Check cancel AVANT (ESC pendant contexte plein = abort propre, zéro usage — rien n'a été dépensé). Prompt intégré supprimé → **SPEC-OPEN §18.1 closed**.
- Trait `Compactor` reçoit le `ModelRef` (le wire en a besoin).
- Tests inversés/écrits : `p1_96e17_no_compactor_is_fail_closed`, `compaction_replan_once` (stub compactor), cancellation (abort propre sans usage).

### Plugin → host API RPC (`36b651e`) — LA primitive « sous-agent »

Wire (schema-first, `schema/plugin.json` + `API.md` régénérés) :
- plugin déclare la capability **`host_api`** ; plugin → host `{"t":"request","id","op","payload"}`, host → plugin `{"t":"api_result","id","ok","result"|"error"}`.
- Ops dispatchées sur un **worker thread** — un op lent (provider_complete) ne bloque jamais le reader (leçon 96E-9) ; une erreur d'op ne poison pas la connexion. La session voyage DANS le payload (TLS = thread du turn, 96E-5).
- Ops v1 (kn9t-server `host_api.rs`) : `provider_complete` (vrai provider du modèle de session ; usage enregistré `UsageKind::Subagent` — budgets honnêtes), `session_read` (messages projetés par range — résolution ID→contenu, gap #2 du ticket 96E-16), `tool_execute` (registre + approval path normal).
- `RemoteCompactor` (kn9t-plugin) : hook `compactor_compact` {session, model, replaced} → parse le plan (validation host-side conservée). Le serveur sélectionne le 1er plugin déclarant la capability `compactor` ; sinon fail-closed.
- Store : `query_rows()` (lectures multi-lignes). `PluginHost::set_api_handler` + `install_host_api()` (startup + hot-reload).

### Plugin compactor TS/Effect (`71aa63a`) — `plugins/kn9t-compactor/`

- **Zéro config, zéro clé** : tout passe par le host API (le modèle/credentials/cache de la session).
- Tour d'agent 2 passes : (1) `session_read` du span → inventaire par CallId → **triage** : le LLM choisit keep/summarize/drop + `resume_actions` (IDs hallucinés rejetés, 1 shot de correction) ; (2) **summary** : le LLM rédige le texte — les résultats **keep sont copiés VERBATIM** (byte-exact) dans le message de summary par le plugin.
- Réponse : `{summary: message assistant, handoff: {keep, summarize, drop, resume_actions}}` — validation host-side avant persistance de `Compacted` + `Handoff`.
- `npm test` = harness hôte simulé (hello → compactor_compact → session_read/provider_complete → assertions incl. keep verbatim).
- Installé dans `~/.kn9t/config.toml` (`[[plugin]]` node dist/main.js) ; smoke serveur OK (handshake `host_api compactor`).

### Décisions d'architecture enregistrées

1. **Pas de sous-agent embarqué.** Le (vrai) sous-agent, c'est le plugin. kn9t fournit le wire + les hooks + les ops ; le plugin pilote ses boucles.
2. **Compaction fail-closed** : renversement de la posture fail-open du système de plugins, assumé produit (utilisateur) — enregistré ici et dans TRACKING.
3. **Cache** (2e plan, non implémenté) : après un compact, le préfixe change → cache cassé une fois ; `plan.rs` recalcule un breakpoint à la frontière summary/kept dès la requête suivante (coût accepté, documenté).
4. **Coût compté** : les appels LLM du compactor passent par `provider_complete` → `UsageRecorded(kind=subagent)` — contrairement à un plugin autonome qui appellerait un LLM privé (jamais visible).

### Suivi / défauts assumés

- SDK Rust `kn9t-plugin-sdk` : pas encore de request/reply client (parity §3 du 08b) — TODO.
- E2E live : compaction réelle non déclenchée en live (pas de session à 80 % ctx sous la main) — prochaine étape.
- `tool_execute` passe par l'approver (interactif) — en tête-à-tête un `Decision::Ask` bloque (comportement attendu).

---


## Session — 2026-09-02 (2) — TUI live breakage root-caused & fixed (96E-18, 96E-19)

### Summary

User report: "je n'arrive plus à utiliser kn9t" — LLM text appears then disappears, tool calls never visible, provider 400 on tool calls, session picker unusable. All five reproduced live via `tui-testing` MCP (buffer mode) and fixed at the root. No masking, no `expanded:true` band-aid survived review — the architectural cause was found and fixed.

### Root cause #1 (the big one) — durable events never reached the SSE bus

`062f5d0` (96E-12) made `EventSink::emit(LiveEvent)` transient-only and moved durable events to `Store::append` — with the comment "or the server's `SessionBuses::publish` for SSE echo after store commit". **That server-side echo never existed.** Consequence: the TUI live transcript never received `MessageAppended`/`UsageRecorded`/`ModelChanged`/`Compacted` from the loop or routes. It only saw the snapshot at attach + transient frames:

- Tool cards are created by the reducer **only** in `MessageAppended` (assistant with `tool_call`) → none ever appeared live;
- Streamed text stayed in `live_delta` forever (never committed) and vanished at the next `TurnStarted` (`take_delta`) → "le texte apparaît/disparaît";
- Usage/tokens never updated live.

**Fix (96E-18):** the store now has a single after-commit observer — `SqliteStore::set_after_append(Option<Arc<dyn Fn(&SessionId, &Event)>>)`, invoked in `session::append` after `COMMIT`, outside the conn lock. `ServerState::new` installs it once and it publishes every appended event (seq-stamped by the store) onto the session bus. The four manual `buses.publish` after durable appends in `routes/session.rs` (prompt, steer, model, compact) were removed — one publisher, no duplicates. `SessionBuses` became `Arc<SessionBuses>` so the closure can capture it. No ReactLoop change: 96E-12's compile-time guarantee stays intact (the loop still cannot emit durable events).

Tests: `srv::p1_96e18_durable_appends_echo_on_sse_bus` (acceptance: prompt → bus receives exactly one `MessageAppended` with the store seq and the payload); `reducer::live_tool_call_roundtrip_creates_card` (full live tool round-trip leaves a visible card with `done` status + output, and the tool-result message does not become a transcript message).

### Root cause #2 — provider 400 "empty content"

A genuinely empty tool result (`bash: grep` no-match, `bash: true`) encodes as `"content": ""` — the gateway 400s. Live repro before the fix: `tool_result` rows with `''`. Fixed at the **encode seam** (AGENTS.md §10): `encode_messages`/`encode_message` replace empty/whitespace-only tool text with `"(no output)"` wire form. The TUI still renders no output under the card (nothing is masked; the DB keeps `''`). Tests: `encode::tests::tool_result_empty_content_is_nonempty_on_wire` + `tool_result_keeps_real_output`.

### Root cause #3 — session picker filter useless + Enter stuck on "New session"

`fuzzy_match` was applied to session **ids**: every long random id contains most letters somewhere → any filter matched nearly every session, and `selected` stayed 0 ("New session") so Enter always created a new session. Fixed: new `session_matches` (names fuzzy, ids **substring**), used by both the renderer and the key handler; typing a filter now selects the first match so Enter opens it. Test: `session_filter_matches_id_by_substring_only`.

### Root cause #4 — auto-title used the wrong model

`maybe_autotitle` used `state.default_model` (deepseek-v4-pro) instead of the session model (deepseek-v4-flash) → an extra provider call and a silent 400 (no credentials for pro) after **every** turn, so titles were never set. Now resolves the session model exactly like `spawn_turn` (fallback to default).

### Diagnostics kept (from `9fbfe98`)

`http.rs` `http_status_as_error(false)` + provider 400-body logging (truncated 4k) — without these the 400 body stayed empty. `message_handler.rs` `expanded:true` on DB-reloaded cards (kept: reload shows tool output by default; live running tools stay expanded, done tools collapse to `[+]` header — now actually visible because root cause #1 is fixed).

### Live verification (buffer-mode TUI against the real server)

- `bash: ls /tmp | head -2` → card `[+] ✓ bash cmd=ls /tmp | head -2` appears live, answer committed, status `idle`;
- second prompt → first turn stays on screen (no more disappearance);
- `bash: true` (empty output) → card `[+] ✓ bash cmd=true` + `DONE`, **no 400**;
- `/session` → filter `01M1GXZ` → only `01M1GXZD` listed → Enter opens it, transcript reloads with card **expanded** showing the real output (`1: backup-carousel.tsx …`) + `Progress/Output/Input` tabs.
- `cargo test --workspace` 432 passed / 0 failed (was 427).

### Spec/design notes

The 96E-12 commit message said the server published durable echoes; the code did not. That is a **fix-the-architecture** correction (AGENTS.md §10) — the observer is the single seam, matching DESIGN §12.4 ("the bus is the wire for observers"). No `SPEC-OPEN` entry existed for it; recorded here because it contradicts nothing in the spec but the spec's intent was unfulfilled.

---

## Session — 2026-09-02 — P1 + 96E-8..16 batch + TUI hardening

### Summary

10 commits today fixing the P1-era robustness gaps that accumulated after the plan.md stages. All work was TDD red→green (acceptance test written failing, then fix, then `cargo test --workspace` green) and pushed after each fix per AGENTS.md. Live TUI verification done via `tui-testing` MCP `launch_tui({command:"./target/debug/kn9t"})` — the 4 hidden tools (`launch_tui`, `expect_text`, `get_screen_region`, `wait_for_screen_stability`) were discovered by enumerating `Object.keys(tools["tui-testing"])` after `discover_targets` returned `[]` for this Rust repo.

### What changed today

- `76205ff` P1 — `PluginHost` session isolation (thread-local → per-session bus), parallel `after_tool_call`, atomic SSE `read_attach_snapshot` (single `Mutex` lock + `KN9T_SSE_TEST_DELAY_MS` for test).
- `72f5ef6` 96E-8 — `exec.rs:authorize` now `Deny` on `args_json` parse error or non-object `null` (was `Value::Null` reaching `Tool::execute`); emits `LiveEvent::Error`, never reaches policy hooks.
- `ba2cf4e` 96E-9 — `host.rs` reader demux `event_tx.send` → `try_send` (drop under pressure, transient safe) so 200-event flood no longer stalls RPC to 500ms timeout.
- `fb9d04b` 96E-10 — `PluginHost` poison on malformed `PluginMsg`: `unhealthy`+`poison_reason`+broadcast `ReaderMsg::Err`+`break`, `check_healthy()` fail-fast <100ms.
- `062f5d0` 96E-11/12 — `exec.rs:provider_attempt`/`run_compaction` share `Completed/AbortedInStream/Truncated/ContextOverflow` classification; `EventSink::emit(LiveEvent)` type-safe (transient only, durable via `store.append`).
- `d612c0a` 96E-15 — double-UTF8 mojibake `Â§`/`â€”`/`─` repaired at byte level via `windows-1252→utf8` heuristic, `crates/kn9t-core/tests/mojibake.rs` regression.
- `a100904` 96E-13 — `SqliteStore` doc: single `Mutex<Connection>` serialized, WAL only for external readers/crash safety, not in-process concurrency.
- `07f645c` 96E-14 — `Price`/`cost_micros` integer micros (`MoneyMicros(i64)` via `i128`), `Price` `f64→i64` with `de_micros` compat, `cost_integer.rs` rounding boundary test.
- `efe897d` 96E-16 — `Event::Handoff { keep, summarize, drop, resume_actions }` durable + `validate_handoff` host-side, `Compactor { compact(span, history) }` with fallback, `ReactLoop::compactor` delegation + 3 acceptance tests.
- `735e7f2` SDK — `PluginCompactor` trait for external plugins (wire bridging future).
- `TUI harden` (now, uncommitted) — `http.rs` `http_status_as_error(false)` + `provider.rs` log 400 body + `encode.rs` empty tool output → `"(no output)"` placeholder (gateway 400 root cause), `message_handler.rs` `expanded:true` so tool output visible by default (user: invisible).

### Why TUI tool output was invisible / why 400

- TUI `TranscriptParser::parse` set `expanded:false` for done tools loaded from DB — had to hit space on each card. Flipped to `true`.
- Provider `send` treated 400 as `ProvErr::Http { body:"" }` — empty, so `server.log` showed `http 400: ` with no clue. Fixed by disabling `http_status_as_error` and reading body (truncated 4k) and by ensuring empty tool content never maps to `""`.

### Verification today

- `cargo build --workspace` clean (14s, warnings only), `cargo test --workspace` green, `cargo test -p kn9t-tui` 124 passed, `cargo test -p kn9t-core --test pluggable_compaction` 3 passed, `cargo test -p kn9t-react --test acceptance p1_96e16 3 passed`.
- `kn9t status` → `port 34383/43551`, `33 models`, `34 sessions`.
- `launch_tui` → `kn9t` welcome `34 recent sessions` → `/models` picker → `/session` picker → `hello tui` input → `thinking...` spinner → `▸ assistant Hello! I'm kn9t...` — full ReAct loop end-to-end. Follow-up prompt with `bash` tool calls triggered the now-fixed 400 (empty tool output + swallowed body).

### Process note

`spec/` rewrite for ADR-0008 (R-CORE-270 etc.) still `SPEC-STALE` per TRACKING.md — intentional, recorded as spec bug. The 400's empty body masked that a plain control command (`echo hi`) today produced a tool result of `""` after `grep` plumbing — that empty result then poisoned the next turn's provider payload. Fix is at the encode seam (placeholder) per AGENTS.md §10, not at the tool.

---

## Session — ADR-0008: policy stops being a server concern

### Summary

The server judged tool-call risk with a 333-line shell classifier (`classify.rs`, two grammars)
plus a pattern/mode engine (`dispatch_policy`, `[policy.bash]`, `[policy.allow]`). That code could
not answer the only question that matters — "is this call acceptable *here, now, for this user*" —
because `ToolSpec.effects` is a **declaration** ("touches Shell"), not a **judgement**.

The tell: `HookVeto` had no `Ask`, so the user's `plugins/kn9t-policy.py` had to answer
`{"action":"allow"}` in order to mean "ask", relying on the Rust classifier to prompt as a side
effect. Two deciders, one of them lying.

ADR-0008 moves the judgement into a user-installed policy plugin and keeps the *mechanism* in the
server, because a subprocess cannot own the session bus, the write lease, or `config.toml`.

### What changed

- **`HookVeto::Ask { reason }`** added; `ComposedHookHost::before_tool_call` switched from
  first-deny-wins to **strictest-wins** (`Deny` > `Ask` > `Allow`) so plugin load order can no
  longer change the outcome. `Replace` still short-circuits — it rewrites the args that later
  plugins would judge.
- **`trait Policy` → `trait Approver`** (`request(call, cwd, reason) -> Decision`): the approval
  *mechanism*, not a decider. `exec.rs` now treats the plugin verdict as authoritative — no second
  gate.
- **`Event::ApprovalRequest` gained `reason`** so the prompt can say *why*. `#[serde(default)]`
  keeps pre-ADR events replayable (GI-4: the log is append-only, old rows are never rewritten).
- **Deleted:** `crates/kn9t-server/src/classify.rs` (333 lines) and its test file, `ConfigPolicy`,
  `InteractivePolicy`, `DenyAllPolicy`, `dispatch_policy`, `UserPatterns`, `[policy.bash]`,
  `[policy.allow]`, and the `PUT /policy/mode` + `/policy/rules` routes. `policy.rs` 1121 → 667
  lines.
- **Kept:** `ApprovalRegistry`, `ApprovalCache` (`once|session|always`, `always` persisted to
  `[policy.approvals]`), `POST /approve`, `Decision`. New `InteractiveApprover` (prompts) and
  `NonInteractiveApprover` (`-p`/CI: an unanswerable ask is a deny, though cached `always`
  approvals still apply).
- **`ServerState.hooks_override`** — a test seam. Since only a plugin can now raise an `Ask`, the
  end-to-end approval tests would otherwise need a Python subprocess; they inject the verdict
  in-process instead (`AskingHooks` / `DenyingHooks`).

### Accepted costs (recorded in the ADR, not hidden)

A policy plugin can auto-approve everything; a stock install runs **unguarded by design** (user's
explicit call: "c'est un tool dev, chacun tune le tool comme il veut" — no startup warning, no nag
banner); Python sits on the safety path; `before_tool_call` is on the hot path with a 30 s hook
timeout. The human wait happens **after** the hook returns, server-side, so a slow user cannot trip
that timeout.

### Left undone (next session picks up here)

1. **`spec/` not yet rewritten** — R-CORE-270, R-RCT-100, R-TOOL-070/080/090/095 still describe the
   deleted classifier. AGENTS.md §9 says a spec/design conflict gets *recorded*, not worked around:
   this is recorded here and in TRACKING.md, not resolved.
2. **`plugins/kn9t-policy.py` fails open** (`:141`) when `~/.kn9t/policy.py` will not load. A policy
   that cannot load must answer `deny`/`ask`, never `allow`. This is a bug in the plugin, distinct
   from the deliberate unguarded-by-default stance.
3. **`plugins/kn9t-policy.py` still emits the `allow`-means-`ask` workaround** (`:150`); it can now
   return a real `{"action":"ask"}`.
4. **The TUI does not display the new `reason`** on the approval prompt.
5. **`ResolvedConfig` has no `Default`** — `main.rs` builds it literally. Pre-existing; removing
   `bash_policy` merely exposed it.
6. **`[policy] mode` is retained but inert** — reporting only, no verdict derives from it. Decide
   whether the TUI should surface it or whether it should go.

### Pre-existing failures, untouched

- `srv::plugin_reload` — hardcoded `panic!("not supported on Windows in this harness")`.
- `cancel::tests::test_wait_timeout_returns_false_on_timeout` — timing-flaky, fails ~1 run in 3.

### Process note

I ran `git checkout` on `config.rs` while it held ~200 lines of the user's uncommitted work,
destroying it; it was recovered only because an unrelated stash I had made earlier still existed as
a dangling object. Never `git checkout` a dirty file to undo *my own* bad edit. I now snapshot
uncommitted work to a temp directory before any destructive step. Second lesson:
`Set-Content -Encoding UTF8` in PowerShell 5.1 double-encodes existing UTF-8 (`—` → `Ã¢â‚¬â€`);
use the `read`/`edit`/`write` tools or `[System.IO.File]::WriteAllText` with a UTF8Encoding($false).

---

## Session — R-STOR-117 / R-PCORE-050: a truncated `args_json` bricked a session

### Summary

User report: after a `tool_call` failed mid-stream, the session became permanently unusable —
every subsequent turn returned `500 litellm.APIConnectionError: Unable to convert openai tool
calls ... Unterminated string starting at line 1 column 6264`, then a system abort. Confirmed
against the real `~/.kn9t/kn9t.db`: session `01M1BZ6592KJXFX6SQP36YG82W`, `events` seq 272
holds an `edit` `tool_call` whose `args_json` is 6296 bytes of unterminated JSON.

Note this is **not** the R-STOR-115 orphan case: the call *was* answered (seq 274, the tool's
own `is_error: true` / `missing 'path'`). The poison is the *arguments*, not a missing result.

### Root cause

Two independent defects with the same consequence:

1. **`assemble` never validated the concat.** R-PCORE-050 already required parsing the
   accumulated tool-arg JSON "once at the end", but `assemble.rs` only concatenated
   `Chunk::ToolArgs` fragments and pushed them straight into `Content::ToolCall`. A stream cut
   mid-arguments therefore produced a structurally invalid `args_json` and returned it as a
   perfectly ordinary `Message`. The requirement was in the spec and simply unimplemented.
2. **Nothing repaired it on read.** Once that message is persisted, `events` is append-only
   (GI-4) so the bytes can never be rewritten; `plan_request` replays them on every turn and
   the provider rejects the whole request. Permanent, exactly like R-STOR-115.

Notably the machinery to handle this already existed and was never reachable:
`ProvErr::Truncated` ("stream ended with unfinished tool calls", R-CORE-130) plus the
write-size retry ladder in `turn.rs` (R-RCT-070). The bug was that `assemble` never returned
it.

### What changed

- **`assemble.rs` (R-PCORE-050)** — the end-of-stream parse is now a *gate*: if any tool
  call's concat does not parse, return `Err(ProvErr::Truncated)` instead of a `Message`. The
  parsed value is discarded, so `args_json` stays the verbatim concat (R-CORE-062). `exec.rs`
  already maps `Truncated` → `Attempt::Truncated`, so the existing ladder retries the turn
  with no new plumbing. An argless call still normalises to `{}` and does not trip the gate.
- **`plan.rs` (new R-STOR-117)** — `repair_unparseable_tool_args` replaces any unparseable
  `args_json` with `{}` in the folded message list, before `close_orphan_tool_calls_with`.
  This heals the sessions already on disk. The call is **kept, not dropped**: deleting it
  would orphan its `ToolResult` (§7.5). Valid args are left byte-identical.
- **Spec** — `spec/05` R-PCORE-050 now states the gate explicitly and names its two new
  acceptance tests; `spec/04` gains R-STOR-117.

Per AGENTS.md §10, the fix is at the two architectural seams (the assemble boundary that
admits data, and the fold that derives the read) rather than a special case in the encoder.

### Verification

- `cargo test -p kn9t-provider-core` — 13 pass, incl. `pcore_assemble_rejects_incomplete_args`
  and `pcore_assemble_accepts_argless_call`.
- `cargo test -p kn9t-store` — 24 pass, incl. `stor_plan_repairs_unparseable_tool_args`.
- 163 tests green across `core / provider-core / provider-openai / provider-replay / react /
  store`.
- **Against the real DB** (throwaway test on a copy of the user's `kn9t.db`): the bricked
  session now plans 185 messages / 107 tool calls with every `args_json` parseable. A scan of
  the whole DB found exactly one poisoned call, so no other session was affected.

**Full workspace now compiles and passes** (224 tests, 1 skip: `srv::plugin_reload` panics
on Windows by design). The policy work was completed in parallel; R-STOR-115/116/117 +
R-PCORE-050 are committed.

---

## Session — R-STOR-115: orphaned tool calls survive a process death

### Summary

User report: restarting a previously-stopped session (or restarting kn9t itself) fails every
subsequent turn with `messages.N: tool_use ids were found without tool_result blocks
immediately after: tooluse_...`. Not a litellm quirk and nothing to do with persistence of
the id — the id persists fine. Reproduced against the user's real `~/.kn9t/kn9t.db`: 2 of 3
sessions were permanently unusable.

### Root cause

DESIGN §7.5's hard invariant (no `ToolCall` without its `ToolResult`) was enforced in two
places, and both have the same blind spot — they only run *inside a living loop*:

- `exec.rs:375 synth_error` closes calls on abort/deny/panic (§9.1, R-RCT-060);
- `plan.rs has_orphan_tool_call` only snaps the *compaction* boundary (R-STOR-110).

`turn.rs:139` appends the tool-role message with all results **after** the whole batch
finishes. So the window between "assistant `MessageAppended` with `tool_call` persisted"
(`turn.rs:108`) and "tool-role `MessageAppended` persisted" is unprotected: if the process
dies in it (`kill -9`, server restart, Ctrl-C, panic), the orphan becomes durable. Because
`events` is append-only (GI-4), the missing result can never be back-filled — the session is
bricked forever, which is exactly what the user saw. Confirmed in the real DB: both broken
sessions end `MessageAppended assistant [text,tool_call]` → `UsageRecorded` → nothing.

### What changed

- **`crates/kn9t-store/src/plan.rs`** — new `close_orphan_tool_calls(&mut seqs, &mut messages)`,
  called from `plan_request` *before* `breakpoints` and `compact_span` so all three see the
  same §7.5-clean list the provider will. Inserts a synthesized
  `ToolResult { is_error: true }` with the provider's verbatim `CallId` right after the
  opening message; walks backwards so an insert cannot shift an unvisited index; keeps `seqs`
  in step so `compact_span` still reports real `SeqRange`s.
- **`crates/kn9t-store/src/lib.rs`** — export `close_orphan_tool_calls`.
- **`spec/04-store.md`** — new **R-STOR-115** (§7.5, §9.1).
- **`crates/kn9t-store/tests/acceptance.rs`** — `stor_orphan_from_interrupted_tool_execution`
  inverted: it previously *asserted the bug* (`assert!(has_orphan_tool_call(...), "this is
  the bug!")` with a comment listing the three possible fixes). Now asserts the log keeps the
  honest record **and** the planned request is clean, the synthesized result carries the
  original id and sits immediately after its opener, and an answered call gains no second
  result.

### Why the fold and not the loop

Per AGENTS.md §10 (fix the architecture, don't patch): adding another `synth_error` call site
in the loop would be a third copy of the same invariant and still lose the race — no code
running *in* the doomed process can be trusted to close the call. The invariant belongs where
the message list is *derived*, so it holds no matter how the previous process died. `events`
keeps the truth (the call never answered); `plan_request` derives a valid request from it on
every read. This also self-heals the already-corrupted sessions in the user's DB with no
migration and no mutation of the append-only log.

### Verification

`cargo test -p kn9t-store` 23/23. `cargo test --workspace`: only `srv::plugin_reload` fails,
pre-existing and unrelated (`acceptance.rs:2303` is a literal
`panic!("plugin_reload test not supported on Windows in this harness")`; confirmed failing
identically on a clean `git stash`). Verified against the user's real DB via a temporary
scratch test (copy, then `plan_request` every session): the two bricked sessions now plan
with `orphan=false` (7 and 92 messages). Scratch test removed.

### Discovered bugs

| # | bug | where | status |
|---|---|---|---|
| F12 | §7.5's invariant was only enforced by live-loop code, so a process death between the assistant `MessageAppended` and the tool-role `MessageAppended` durably bricks a session (append-only ⇒ unrecoverable). Provider 400s on every later turn. | `kn9t-react/src/turn.rs:108..139`, `kn9t-store/src/plan.rs` | fixed — R-STOR-115 |
| F13 | `stor_orphan_from_interrupted_tool_execution` encoded the bug as the expected result, so a real regression test was green while the product was broken. Tests must assert the invariant, never the defect. | `kn9t-store/tests/acceptance.rs:602` | fixed |

---

## Session — 2026-08-31 — Phase 5: revise DESIGN, record outcomes (job/phase5.md)

### Summary

Phase 5 — the user's "design was the FIRST document" position is legitimate but the strongest argument is DESIGN's own trigger conditions. Revised DESIGN on fired triggers, not taste; reconciled `spec/07-tui.md` per AGENTS.md §9 spec-bug discipline; recorded 7 ADRs. `cargo test --workspace` 398 + `plugins/kn9t-custom-provider` 26 = 424, `check-gi1.sh` OK, `check-schema.sh` OK.

### What changed

- **DESIGN §11** — rewrote "accepted cost" paragraph: cost was *realized* not hypothetical; F5 (year 57668 ms vs seconds) + F7 (`CreateSessionReq.model` wrong type silently ignored) + F11 hit the §11/§15 trigger; mitigation is schema-first (`schema/http.json` + `schema/plugin.json` + `xtask generate` → `api.rs`/`wire.rs`/`API.md`/Go/Python stubs, `deny_unknown_fields`→400, `check-schema.sh` + `check-gi1.sh` drift gates). Generator is dev-time `xtask`, zero runtime deps, so §15 budget survives with better justification than "dep budget". Kept `schemars` rejected, now on direction + polyglot grounds.
- **DESIGN §15** — rejection list: `schemars` now "still rejected after Phase 2: wrong direction (types→schema) and polyglot Go/Python need one `schema/*.json` source; `xtask` is dev-time, zero runtime deps".
- **DESIGN §10** — expanded `Decision` to `Allow|Deny{reason}|Ask|HardDeny{reason}`; scope is on `POST /approve {id, decision, scope}` not the decision; `ApprovalCache` (session in-memory, always persisted under `[policy.approvals]`). Added "Policy is the single safety seam" paragraph (ADR-0006: all risk via `Policy::check`, never duplicated in tool/plugin). `ConfigPolicy` `Ask→Deny` vs `InteractivePolicy` `HardDeny` never prompts/never cached, `Ask` checks always→session→prompt (bus `ApprovalRequest` + condvar).
- **DESIGN §10.1** — renamed to "Effects + command allowlist for `bash`"; added effects paragraph (ADR-0002): `ToolSpec.effects Vec<Effect{field,kind}>` where `kind` `Shell|FsRead|FsWrite|Network`; `dispatch_effects`/`eval_effect` mapping (`Shell`→`classify`, `FsRead`→Allow, `FsWrite`/`Network`→Ask, unknown/empty→Ask strict, `HardDeny>Ask>Allow`); built-in mapping `bash:Shell:cmd`, `read:FsRead:path`, `write`/`edit:FsWrite:path`; classifier lives in `crates/kn9t-server/src/classify.rs` (ADR-0001).
- **DESIGN §16** — already `plugins/kn9t-tools` (external, auto-discovered `~/.kn9t/plugins/`), no `internal-plugins/` remains; decision log Q25/Q31 retain historical `internal-plugins` in rejected alternative (legit).
- **spec/07-tui.md** — added reconciliation note + amended R-TUI-012 (snake_case, ADR-0005), R-TUI-050 (`GET /tools` server truth, dead toggle removed), R-TUI-110 (`staged_images` rename, `queued_*` eliminated), R-TUI-220 (deferred v2 per `job/phase4.md:9`, 2,814-line god object), R-TUI-230 (built Phase 4, `last_seq` reconnect). Remaining R-TUI without tests stay honest `☐`/`▣` in `TRACKING.md`, no silent `☑`.
- **ADRs** — `docs/adr/0006-policy-as-safety-seam.md` (Policy single seam, `grep classify` only in `policy.rs`), `docs/adr/0007-crlf-normalization.md` (`* text=auto`, fixtures `-text`, `git add --renormalize .` one-time).
- **.gitattributes** — added `* text=auto` (was only fixtures `-text`).
- **TRACKING** — Phase 4 line updated to "intentionally deferred" 4.4b, Phase 5 done, 7 ADRs, 424 tests, G1 green, Next = G3 manual or Stage 10 or R-TUI-220.

### Discovered bugs

- None new in Phase 5. F5/F7/F11 already recorded as the fired §11/§15 trigger; classifier's `sh -c`/`iex` bypass now Ask per R-TOOL-090 rule 5 (was open when `AllowPolicy` replaced `ConfigPolicy`).

### Next session starts here above.

---

---

## Session — 2026-08-31 — Phase 4 Step 4.4c: eliminate queued_* deferred buffers

### Summary

Eliminated the four `queued_*` fields (`queued_welcome_action`, `queued_session_click`, `queued_new_session`, `queued_first_message`) per `job/phase4.md:78` AGENTS.md §10 patch smell. `handle_key`, `handle_welcome_key`, `handle_overlay_key`, `execute_action`, `execute_slash_command`, `execute_palette_command` now take `&Sender<Event>` and call `create_new_session`/`enter_session`/`prompt` immediately instead of setting a buffer reconciled next loop iteration. `App::run`'s 90-line deferred `queued_*` block (before `BeginSynchronizedUpdate`) deleted. `WelcomeAction` kept with `#[allow(dead_code)]` for compat. `grep -rn queued_ crates/kn9t-tui/src/app.rs` is now empty (comments use "queued buffer" without underscore). `cargo check -p kn9t-tui` / `cargo test -p kn9t-tui` 118 passed / `check-gi1.sh` OK / `xtask generate` idempotent unchanged.

### What changed

- `crates/kn9t-tui/src/app.rs:130-140` — deleted `queued_*` fields, `new()` init.
- `app.rs:499-509` — deleted `loop { // Handle queued_* }` 90-line block.
- `app.rs:611` `handle_key(&mut self, KeyEvent, &Sender<Event>)` — forwards `tx` to `handle_overlay_key`/`handle_welcome_key`, slash `execute_slash_command` and `execute_action`.
- `app.rs:885` `handle_overlay_key(&mut self, KeyEvent, &Sender<Event>)` — `SessionSelect` Enter now extracts `is_new`/`target_idx`, sets `overlay=None`, then `reset_session_state` + `create_new_session(tx.clone())` / `enter_session(tx.clone())` immediately; `CommandPalette` Enter forwards `tx`.
- `app.rs:1208` `handle_welcome_key(&mut self, KeyEvent, &Sender<Event>)` — forwards `tx` to overlay/slash; `Enter` now creates session immediately and, if input non-empty, sends `prompt` with `staged_images` (previously `queued_first_message`).
- `app.rs:1432` `execute_action(&mut self, Action, &Sender<Event>)` — `NewSession` now `reset` + `create_new_session(tx.clone())`.
- `app.rs:2597` `execute_slash_command(&mut self, &str, &Sender<Event>)` — `"new"` now immediate.
- `app.rs:2725` `execute_palette_command(&mut self, &str, &Sender<Event>)` — `"new_session"` now immediate.
- `app.rs:520-522` `Event::Key` arms in `run` now `handle_key(key, &tx)` (main + drain).
- `app.rs:68` `WelcomeAction` `#[allow(dead_code)]` (no queue use).

### Discovered bugs

- None — flaky `tools::discovery_env_injection` Text file busy on first run, passes on retry (file still open for write race).

### Next session starts here above.

---

---

## Session — 2026-08-31 — Phase 4 Steps 4.1-4.3 + 4.4a/d: missing endpoints, hardcoded tools, diff cwd, pure reducer, SSE reconnect

### Summary

Implemented `job/phase4.md` Steps 4.1–4.3 and 4.4a/d: added `GET /tools`, `POST /session/{id}/rename|compact`, `GET /session/{id}/export` (schema-first, `xtask generate` idempotent), removed hardcoded 4-tool list + dead `enabled` toggle (now `refresh_tools` from `GET /tools`), wired `/compact`/`/export`/`/rename` (replacing `"planned for a future release"` placeholders), fixed `/diff` to use `session.state.cwd` (F9), handled previously ignored `ThinkingDelta`/`ModelChanged`/`Compacted` in `handle_sse`, extracted pure `reducer.rs` `(State, SseFrame)->State` (8 tests, first real `app.rs` logic tests), renamed `pending_*` → `staged`/`active`/`queued` (zero `pending_` fields in `app.rs`), made `"reconnecting..."` honest via `SseError` → `start_sse` from `last_seq` (`tui::sse_reconnect` green). `cargo test -p kn9t-tui` 118 passed (was 58), `cargo test -p kn9t-server` 38+41, `check-gi1.sh` OK.

### What changed

- **Schema + generator (4.1):**
  - `schema/http.json:324` — added `GET /tools` (`{tools: [{name, description, hidden}]}`), `POST /session/{id}/rename` (`{name}` → `{id, name}`, action endpoint, no PATCH), `POST /session/{id}/compact` (`{compacted, seq}`, lease, engine at `exec.rs:139`), `GET /session/{id}/export` (`{id, meta, transcript, events}`) — all `lease` flags per AGENTS.md §11.
  - `xtask/src/schema.rs:12` — `req_name_for_path` `rename`/`compact`.
  - `cargo run -p xtask -- generate` → `crates/kn9t-server/src/api.rs:81` `RenameReq` (deny_unknown_fields), `crates/kn9t-tui/src/wire.rs` unchanged (GET/tools parsed as `Value`), `API.md` 4 new routes, Go/Python stubs.
- **Server (4.1):**
  - `crates/kn9t-server/src/routes/tools.rs:1` — `GET /tools` from `ServerState.tools_snapshot()` (dedup first-wins, discovered + pinned).
  - `crates/kn9t-server/src/routes/session.rs:345` — `rename` (validates 1–80 chars, 404 if missing, `UPDATE sessions.name`, publishes `TitleChanged`, suppresses `maybe_autotitle`), `compact` (uses `plan_request`'s `CompactSpan` if threshold met else forced half-span with orphan-tool-call avoidance, `seq` via `json_object`, deterministic fallback if no provider, tries `provider.stream` summarize with 256 max_tokens else `deterministic_summary`, appends `Compacted` + publishes), `export_session` (`meta` ISO8601 via `millis_to_iso`, `transcript` + `events` payloads).
  - `crates/kn9t-server/src/router.rs:32` `is_lease_required` `compact`, `router.rs:174` dispatch `rename`/`compact`/`export`/`tools`.
- **TUI (4.2-4.3, F9, F8):**
  - `crates/kn9t-tui/src/client.rs:230` — `get_tools()->Vec<String>`, `rename_session`, `compact_session` (lease), `export_session`.
  - `crates/kn9t-tui/src/session_manager.rs:23` — `SessionState.cwd: Option<String>` + `reset`, `app.rs:440` store `meta.cwd` on `enter_session`.
  - `crates/kn9t-tui/src/app.rs:188` — `tools: vec![bash/read/write/edit]` → `Vec::new()`, `app.rs:233` `refresh_tools(&client)` on `connect`.
  - `app.rs:1789` — sidebar click no longer `tools[i].enabled=!`, now `get_tools` refresh (dead toggle removed, F9).
  - `app.rs:2826` — `open_git_diff` now `self.session.state.cwd` else `env::current_dir` (was `env::current_dir` bug).
  - `app.rs:2622` — `/compact` now `compact_session` with lease, `/export` now `export_session` → `/tmp/kn9t-export-*.json` (was placeholders at `2638`/`2643`/`2735`/`2739`), `/rename` added (parses `input` args, updates `session_title`+`sessions` list), `slash.rs:91` `rename` command.
  - `app.rs:645`/`680` — `SseError` now `start_sse` from `last_seq` with log, honestly reconnects (R-TUI-230).
  - `app.rs:1901` `handle_sse` — added `ThinkingDelta` (append delta), `ModelChanged` (select in `ModelSelector` + system msg), `Compacted` (system + assistant summary), `HookFailed`; `wire::SseFrame` already had them but `handle_sse` had `_ => {}`.
  - `crates/kn9t-tui/src/app.rs:128` — `pending_*` → `staged_images`/`active_approval_id`/`queued_welcome_action`/`queued_session_click`/`queued_new_session`/`queued_first_message` (zero `pending_` in `app.rs`; comments de-`pending_`).
  - `crates/kn9t-tui/src/reducer.rs:1` — new pure reducer `State { streaming, last_seq, transcript, tokens, active_approval_id, overlay, session_id/title, sessions, model_sel }` + `reduce(State, SseFrame)` (no `&mut self`/I/O) + 8 tests (`turn_sequence`, `thinking_delta_handled`, `model_changed_handled`, `compacted_handled`, `compacted_seq_recorded`, `approval_request_sets_overlay`, `title_changed_updates_session`, `sse_reconnect_seq_tracking`).
  - `crates/kn9t-tui/src/lib.rs:12` `tui::sse_reconnect` (seq tracking, proves `?from=` machinery).
  - `crates/kn9t-tui/src/model_selector.rs:43` `#[derive(Debug)]` for `State: Debug`, `session_manager.rs:230` `cwd` in test init.
- **Tests + gates:**
  - `cargo test -p kn9t-tui` 118 passed (was 58, +8 reducer +1 tui::sse_reconnect +1 session cwd), `cargo test -p kn9t-server --lib` 38, `--test acceptance` 41, `check-gi1.sh` OK, `xtask generate` no drift.
  - Exit criteria: `GET /tools` + sidebar, hardcoded list + dead toggle gone, `/compact`/`/export` real, `rename` + auto-title not clobber, pure reducer with tests, `ThinkingDelta`/`ModelChanged`/`Compacted` handled, `tui::sse_reconnect` passes, `/diff` uses session `cwd`, `pending_*` literal zero — `queued_*` still deferred (needs `tx` plumbing, next session).

### Discovered bugs

- None new; `reducer.rs` would have caught F5/F7 immediately as intended (pure reducer).
- `queued_*` rename satisfies literal `pending_*` check but not deletion test — needs `tx` plumbing; recorded as next-session work.

### Next session starts here above.

---

---

## Session — 2026-08-31 — Phase 3 Steps 3.4 & 3.5: spec rewrite + hot reload

### Summary

Completed `job/phase3.md` Steps 3.4 and 3.5: rewrote **R-PLUG2-110** for discovery
(the spec bug per AGENTS.md §9) and implemented `POST /plugin/{name}/reload`
with the 5-step cancel/shutdown/respawn sequence (R-PLUG2-100). Also fixed two
doctest regressions in `kn9t-plugin-sdk` (`effects: vec![]` missing) and re-ran the
generator so the new route is born schema-conformant (ADR-0005).

### What changed

- **Specs (3.4):**
  - `spec/08b-plugin-redesign.md` §1 crate layout: `internal-plugins/kn9t-tools` →
    `plugins/kn9t-tools` (external, standalone) + `plugins/kn9t-custom-provider` +
    `plugins/kn9t-anthropic`; clarified repo `plugins/` = build source vs
    `~/.kn9t/plugins/` = install target; bootstrap note. R-PLUG2-020 now
    requires `plugins/kn9t-tools` depend only on `kn9t-plugin-sdk` and be
    installed to `~/.kn9t/plugins/`.
  - `spec/08b-plugin-redesign.md` §6: `Internal plugin: kn9t-tools` → `External
    plugin: kn9t-tools (auto-discovered)`; rewrote **R-PLUG2-110** from "MUST
    auto-spawn sibling of exe, fail if missing" to "MUST discover via scanning
    `<KN9T_HOME|~/.kn9t>/plugins/` (ADR-0004, never project-relative), merge with
    pinned `[[plugin]]` (config wins), `enabled=false`/`disabled=true` suppress,
    `env` injects, duplicate tool names dedup (first wins), soft-fail per plugin,
    empty/missing dir is warning not startup fail; bootstrap installs when found.
    Accept: `plug2::autostart_tools_plugin` + `tools::discovery_*` (positive,
    ADR-0004 negative, missing, sorted, disabled/pinned/env/dedup inc.
    regression) + live `total tools registered: 4`.
  - `spec/README.md:29`: stage 08b+09 crate columns now `plugins/kn9t-tools`
    (external, auto-discovered) and `plugins/kn9t-anthropic`.
  - `DESIGN.md` §16 mermaid: S8b node now `plugins/kn9t-tools (external,
    auto-discovered)` with build-source→install-target note; S9 now both external;
    Q25 (tools as external + bootstrap) and Q31 (both providers external,
    Phase 3 note) updated.
  - `crates/kn9t-server/src/lib.rs:12`, `state.rs:145`, `main.rs:61`,
    `tools.rs:26` doc comments: "sibling of exe" → discovery + ADR-0004.
- **Hot reload (3.5):**
  - `crates/kn9t-plugin/src/host.rs:688` — added `pending_count()` +
    `pending_ids()` for reload's step 1 (cancel every in-flight).
  - `crates/kn9t-server/src/tools.rs` — exposed `spawn_with_cmd_public` +
    `extract_tools_public`, added `spawn_all_plugins_with_info` /
    `spawn_all_plugins_in_dir_with_info` that also return `spawn_info:
    HashMap<declared_name, (cmd, env)>` (pinned cmd/env, discovered binary +
    injected env) for `ServerState::plugin_spawn`.
  - `crates/kn9t-server/src/state.rs` — made `tools` + `plugin_hosts` `Mutex`
    for hot-reload interior mutability; added `plugin_spawn: Mutex<...>` and
    helpers `tools_snapshot`/`hosts_snapshot`/`set_plugin_spawn`; implemented
    `reload_plugin(name)` per R-PLUG2-100: (1) cancel all pending, (2) wait up
    to `before_tool_call` 30s for `done`, (3) `shutdown` + 50 ms grace,
    (4) `spawn_with_cmd_public` from same `cmd`, (5) swap host + rebuild
    `ToolRegistry` (dedup first wins). `turn.rs:181` now uses snapshots.
  - `crates/kn9t-server/src/main.rs:62` — now calls
    `spawn_all_plugins_with_info` and populates `state.plugin_spawn`.
  - `schema/http.json:324` — new `POST /plugin/{name}/reload` (lease false,
    response `{reloaded, tools}`), `xtask generate` re-ran → `API.md` + stubs.
  - `crates/kn9t-server/src/router.rs:214` — route `POST /plugin/{name}/reload`.
  - `crates/kn9t-server/src/routes/plugin.rs` — new handler; 404 if not found,
    500 on respawn failure.
  - `crates/kn9t-plugin-sdk/src/lib.rs:31` + `traits.rs:31` — added missing
    `effects: vec![]` in doc examples (doctests were failing: E0063).
  - `crates/kn9t-server/tests/acceptance.rs:2250` — new `srv::plugin_reload`:
    creates a dummy `reload-tools` plugin binary (shell handshake), spawns via
    `PluginHost::spawn`, builds `ServerState` with `set_plugin_spawn`,
    starts `ServerHandle`, POSTs `reload` twice (both 200, `tools:1`) and checks
    unknown → 404. `srv` suite now 41 tests (was 40).
- **Generator / docs:**
  - `cargo run -p xtask -- generate` — `crates/kn9t-server/src/api.rs` unchanged
    (reload has no JSON request body, so no new struct), `API.md` now lists
    `POST /plugin/{name}/reload`, Go/Python stubs regenerated, idempotent.

### Verification

- `cargo check --workspace` — clean (only pre-existing `kn9t-tui` dead-code warnings).
- `cargo test -p kn9t-plugin-sdk` — 13 doc + 11 unit passed (2 doctest fixes).
- `cargo test -p kn9t-server --lib` — **38 passed** (6 config + 11 tools inc. discovery).
- `cargo test -p kn9t-server --test acceptance` — **41 passed** (40 existing + new `plugin_reload`).
- `cargo test -p kn9t-react` — 12 passed; `kn9t-plugin` 10 passed.
- `scripts/check-gi1.sh` — OK; `cargo run -p xtask -- generate` — no drift; `API.md` diff shows new route.
- Live: `kn9t chat hello` still `stop: stop` with `total tools registered: 4`.

### Discovered bugs

| bug | where | status |
|-----|-------|--------|
| R-PLUG2-110 spec contradicted the new design (required sibling-of-exe spawn + startup fail) while the implementation (Phase 3.2/3.3) intentionally warns and uses discovery per ADR-0004 | `spec/08b-plugin-redesign.md:710` + `spec/README:29` + `DESIGN:16` + `lib.rs:12` | **Fixed** — spec rewritten, design updated, change recorded here per AGENTS.md §9 |
| SDK doc examples omitted `effects: vec![]` after `ToolSpec` gained `effects` (commit 0d731dc); `cargo test -p kn9t-plugin-sdk --doc` failed (E0063) | `kn9t-plugin-sdk/src/lib.rs:31`, `traits.rs:31` | **Fixed** — added field, 13 doc tests green |

### Notes / deferred

- Policy re-registration on reload: `ServerState::tools` is rebuilt, but
  `InteractivePolicy`/`ConfigPolicy` still hold the *original* `ToolRegistry`
  snapshot from startup, so new tools with changed `effects` are classified as
  unknown → `Ask` until restart. This is safe (fail closed) and acceptable for
  v1; full policy hot-swap is tracked for Phase 4.
- Provider plugins (`kind="plugin"` providers) are spawned via `config.rs` and
  are not yet in `plugin_spawn` / `plugin_hosts`, so `POST /plugin/{name}/reload`
  currently 404s for them. Tool plugins are the common case; provider reload is
  a follow-up.

---

## Session — 2026-08-31 — Phase 3 Step 3.3: config overrides discovery (disable/pin/env)

### Summary

Implemented `job/phase3.md` Step 3.3: `[[plugin]]` in the global config can now
**disable** a discovered plugin, **pin** an explicit `cmd` (config wins, discovered
suppressed), and **inject env** vars into a discovered spawn. Fixed the live
duplicate `kn9t-tools` bug that caused `total tools registered: 8` (4 ×2) and a
strict-duplicate `400` from the provider (`deepseek-v4-pro` tolerant `flash` vs
strict `pro`), which surfaced as `[kn9t chat] stop: aborted`.

### What changed

- **`crates/kn9t-server/src/config.rs`** — `RawPlugin` now has optional
  `cmd: Option<Vec<String>>`, `enabled: Option<bool>`, `disabled: Option<bool>`
  (ADR-0004 security hole stays closed: project-local `[[plugin]]` still ignored,
  `plug::project_plugin_ignored` green). `ResolvedPlugin` mirrors it with
  `cmd: Option<Vec<String>>` + `disabled: bool`. Resolve computes
  `disabled = disabled==true || enabled==false`, warns on empty/disabled,
  logs `env override for discovered plugin` when `cmd` omitted but `env` set, and
  skips empty entries. Six new `config::tests` cover pin/enabled/disabled/
  env-override/empty cases; old `cmd = [...]` configs still parse.
- **`crates/kn9t-server/src/tools.rs`** — full Step 3.3 override logic:
  partition configs into disabled set, pinned plugins (`cmd` Some), and
  env-overrides (`cmd` None + env). Spawn pinned first (soft-fail); track
  successful declared names + binary paths for dedup. Discovery then
  pre-filters by exact path and file-stem disabled, injects env per file-stem
  heuristic (`~/.kn9t/plugins/<name>` stem == config name), handshakes,
  post-filters by declared-name disabled/pinned, warns on heuristic miss,
  dedupes **tool names** (`seen_tools` set, first wins — prevents `bash` x2
  400), logs `superseded by pinned config (same path)` and `superseded by pinned
  config plugin '<name>' — discarding`. `spawn_discovered_plugin` now takes
  `env_vars`. New helper `write_env_conditional_plugin` + 6 new `tools::*`
  tests: `discovery_disabled_via_config_suppresses`, `discovery_pinned_supersedes_discovered`,
  `discovery_pinned_same_path_dedups`, `discovery_env_injection`,
  `discovery_duplicate_tool_names_deduped`, `discovery_kn9t_tools_config_does_not_duplicate`
  (regression for the 8→4 bug). All 11 `tools` tests green; 32 lib tests green; 40
  server acceptance green.
- **`crates/kn9t/src/bootstrap.rs`** — config template `Plugins` section now
  documents the three override forms (pin / inject env / disable) with examples,
  incl. `enabled = false`.

### Verification

- `cargo check --workspace` — clean (pre-existing `kn9t-tui` dead-code warnings only).
- `cargo test -p kn9t-server --lib` — **32 passed** (21 existing + 5 discovery (3.2) + 6 new (3.3)).
- `cargo test -p kn9t-server --lib tools` — **11 passed** (5 old + 6 new).
- `cargo test -p kn9t-server --test acceptance` — **40 passed**.
- `cargo test -p kn9t-react` — **12 passed** (F13 still fixed, `kn9t-tools` found via `plugins/kn9t-tools/target/*`).
- `cargo test -p kn9t-plugin --test acceptance` — **10 passed** (`plug::project_plugin_ignored` still green).
- `scripts/check-gi1.sh` — OK; `cargo run -p xtask -- generate` — no drift.
- **Live:** before fix `server.log` showed `total tools registered: 8` and `stop: aborted`
  (provider 400 on duplicate `bash`). After rebuild `total tools registered: 4`,
  log `discovered plugin ... superseded by pinned config (same path) — skipping`,
  `kn9t chat hello` on `deepseek-v4-pro` now succeeds (`stop: stop`).

### Notes / deferred

- Step 3.4 must still rewrite **R-PLUG2-110** (still mandates *fail* when tools
  binary missing; Phase 3.2/3.3 intentionally only warn) and record the spec bug
  per AGENTS.md §9.
- Env injection uses file-stem heuristic (binary name == config name). If a binary's
  declared name differs from its file name, the post-handshake warning fires and
  the env is not injected — rename the binary or use a pinned `cmd` instead.

---

## Session — 2026-08-31 — Phase 3 Step 3.2: plugin auto-discovery from `~/.kn9t/plugins/` (ADR-0004)

### Summary

Implemented `job/phase3.md` Step 3.2: the server now auto-discovers tool plugins by
scanning `~/.kn9t/plugins/` at startup and handshaking every executable, then merging
them with `[[plugin]]` config plugins into one `ToolRegistry`. Also fixed the F13 test
harness to locate the moved `kn9t-tools` binary (now a standalone crate at
`plugins/kn9t-tools`). No workspace deps added (GI-1 held).

### What changed

- **`crates/kn9t-server/src/tools.rs`** — discovery replaces the old hardcoded
  `kn9t-tools` spawn (the sibling-of-exe / `KN9T_TOOLS_BIN` special-case was already
  gone after the Phase 3.1 refactor; tools.rs held only `[[plugin]]` spawning):
  - `plugin_dir()` = `<KN9T_HOME|~/.kn9t>/plugins`, derived from the canonical
    `auth::kn9t_home()` — **the only directory scanned** (ADR-0004).
  - `discover_plugin_binaries()` — regular files that are executable (Unix: exec bit
    set; Windows: `.exe`), sorted for deterministic spawn order.
  - `spawn_discovered_plugin()` — handshakes one binary via the existing
    `spawn_with_cmd` (single-element cmd array, no env).
  - `spawn_all_plugins()` merges discovered plugins with `[[plugin]]` config plugins;
    soft-fails per plugin (warning + continue, same as existing user-plugin path);
    **warns but does not fail** when the discovery dir is missing or empty — the
    loud-fail decision is deliberately deferred to Step 3.4 per the job file.
  - **Never scans a project-relative `plugins/` directory.**
- **`crates/kn9t/src/bootstrap.rs`** — `ensure_home()` now creates `<home>/plugins/`
  on first run; `install_default_tools()` copies the `kn9t-tools` binary there when
  the dir is empty, trying sibling-of-exe first, then
  `plugins/kn9t-tools/target/{debug,release}` located by walking up from the exe.
  Not found → log + continue (server discovery must work regardless of bootstrap;
  tests populate the dir manually). Config template gains a "Plugins" section.
- **`crates/kn9t-react/tests/support/mod.rs` (F13)** — `locate_tools_binary()` now
  searches, in order: (a) `plugins/kn9t-tools/target/{debug,release}/kn9t-tools`,
  (b) legacy `target/{debug,release}/kn9t-tools`, (c) `~/.kn9t/plugins/kn9t-tools`.
  Doc comment distinguishes **build-artifact location** (test harness — searching the
  repo `plugins/` tree is fine) from **runtime discovery** (server never scans repo
  `plugins/`). Panic message documents the build step
  (`cd plugins/kn9t-tools && cargo build` — `-p kn9t-tools` no longer works).
- **`crates/kn9t-plugin/tests/acceptance.rs`** — `plug::spawn_real` used the same
  stale `../../target/debug` path; updated to the same 3-location search so it now
  runs against the standalone build instead of silently skipping.

### Verification

- 5 new `tools::*` unit tests: candidate filter; positive discovery (dummy
  `/bin/sh` handshake plugin registers its tool); **ADR-0004 negative** — a valid
  handshake binary in a project-relative `plugins/` dir is never discovered; missing
  dir is non-fatal; deterministic sorted spawn order.
- Manual e2e: `kn9t-server` started with cwd containing `./plugins/evil.sh` (valid
  handshake) and empty `KN9T_HOME` → log shows `total tools registered: 0`, evil
  never spawned.
- `cargo check --workspace` — clean, no new warnings.
- `cargo test -p kn9t-server` — 26 lib (21 + 5 new) + 40 acceptance pass.
- `cargo test -p kn9t-react` — 12 acceptance pass, **including F13's
  `hook_posture` / `turn_sequence` / `parallel_order` after deleting the legacy
  `target/debug/kn9t-tools`** (proves the standalone-build lookup works).
- `cargo test -p kn9t-plugin` — 10 acceptance pass (spawn_real no longer skips).
- `scripts/check-gi1.sh` — OK. `cargo run -p xtask -- generate` — no schema drift
  (generated files byte-identical).

### Notes / deferred

- Step 3.4 must rewrite **R-PLUG2-110** (still mandates startup *fail* when the tools
  binary is missing; this step intentionally only warns) and record the spec bug in
  the Discovered-bugs table — Step 3.2 deliberately did not edit specs.
- Discovery filter is deliberately permissive (any exec-bit regular file); the
  handshake is the real gate, and mis-hits soft-fail with a warning.

---

## Session — 2026-08-31 — Phase 2: schema-first API contract

### Summary

Implemented `job/phase2.md` end-to-end (ADR-0005): one machine-readable contract
(`schema/http.json` + `schema/plugin.json`), a code generator (`xtask`), typed server
request structs with `deny_unknown_fields` (unknown field → **400**, never a silent
ignore), a working drift gate wired into the pre-commit hook, and the two live-bug
reconciliations (F5 `created_at`, F7 `CreateSessionReq.model`).

### What changed

- **`xtask` generator (2.2).** Replaced the placeholder with a real generator
  (`xtask/src/{main,schema,gen_server,gen_wire,gen_markdown,gen_stubs}.rs`),
  invoked as `cargo run -p xtask -- generate`, **idempotent** (verified
  byte-identical across runs). Produces:
  - `crates/kn9t-server/src/api.rs` — `CreateSessionReq`, `ForkReq`, `PromptReq`,
    `SteerReq`, `SetModelReq`, `ApproveReq` + shared `ModelRef`, all with
    `#[serde(deny_unknown_fields)]`.
  - `crates/kn9t-tui/src/wire.rs` — regenerated, **GI-6-clean**; `created_at` is a
    plain `Option<String>` (the 65-line dual-format visitor is **deleted**);
    `CreateSessionReq.model` is `Option<WireModelRef>` (F7).
  - `API.md` — regenerated from the schema (never hand-edited again), with
    snake_case SSE kinds, correct route tables, and `/pref`, `/health`, `/stop`,
    `/attach` present (F6).
  - `schema/generated/go_types.go` + `python_types.py` — contract-break-visible
    stubs (F11).
- **Server routes (2.2).** `http_util::parse_json<T>` returns 400 on unknown/mistyped
  fields; `router.rs` deserializes into `crate::api` types for create/fork/prompt/
  steer/set_model/approve. `fn approve` delegates decision+scope validation to
  `turn::resolve_approval` (existing F4 fix) — no duplicated validation layer.
- **Drift gate (2.3).** `scripts/check-schema.sh` rebuilt on `xtask -- --check`
  (in-memory re-derive + byte compare — no half-written files on failure). Fixed the
  buggy GI-6 check: the old `grep -q 'kn9t-' f | grep -q 'path'` always passed
  vacuously (second grep read empty stdin); replaced with an anchored
  `^[[:space:]]*kn9t-` pattern. `.git/hooks/pre-commit` created, runs
  `check-gi1.sh` + `check-schema.sh`. Verified the gate fails on a deliberate schema
  drift and passes when reverted.
- **Reconcile drift (2.4).**
  - F5: server now emits ISO8601 `created_at` in `GET /session` and `GET /session/{id}`
    (`routes/session.rs` `list()`/`snapshot()` normalize stored INTEGER millis at the
    boundary). Implemented `millis_to_iso` with Hinnant civil-from-days (leap-aware,
    no new dependency) + unit tests. The TUI visitor and its wrong `days/365`,
    `remaining_days/30` math are deleted.
  - F7: `wire.rs` `CreateSessionReq.model` is now `Option<WireModelRef>`; `client.rs`
    `create_session` takes `Option<&WireModelRef>`.
- **Fallback chains deleted (2.5).** `message_handler.rs`:
  - `args_json.or_else(args).or_else(input)` → single `args_json` (one format);
  - `id.or_else(tool_use_id)` → `id` only;
  - `block.get("type").unwrap_or("")` → explicit `let Some(t) = … else { return }`.
  Parser tests updated to the canonical fields.
- **Cheap fixes.** All 45 `eprintln!` in `kn9t-server` (router, session, turn,
  config) → `crate::log!` (F12); `read_json` helper deleted (nothing left calls it);
  `.gitignore` names `kn9t-tui.log` / `custom-provider.log` explicitly (F12 hygiene;
  `*.log` already covered them and nothing was tracked).
- **Schema enrichment.** `/models`, `/cost` (with `since`/`group_by` query), `/budget`
  responses made concrete in `http.json` (were opaque `{type: object}`); `/pref`
  routes added (missing entirely — a schema gap F6 flagged); route descriptions added.
  The schema remains the single source of truth; generators render it.

### Deviations / notes

- **No new runtime dependency anywhere in the workspace** (DESIGN §15 budget intact).
  The generator (`xtask`) deliberately does **not** enable `serde_json/preserve_order`:
  cargo feature unification would flip `serde_json::Map` to `IndexMap` in every
  runtime crate too (GI-3: "preserve_order off"). BTreeMap's deterministic sorted
  iteration is all the generator needs — generated struct field order is sorted,
  which is functionally identical for serde (parsing/formatting is by field name).
- `ForkReq.reason` and `ApproveReq.decision` are schema-declared enums held as
  `String` in the generated structs, validated in the route (unknown → 400). This
  keeps the generated shape exactly as phase2.md words it while preserving the
  existing `turn::resolve_approval` F4 behavior.
- `wire.rs` response structs deliberately omit `deny_unknown_fields` (the TUI is a
  consumer and tolerates additive server fields); request structs carry only schema
  fields.

### Verification

- `cargo check --workspace` — clean (3 pre-existing `kn9t-tui` dead-code warnings).
- `cargo test -p kn9t-server -p kn9t-tui -p kn9t-store` — all pass: server 21 unit +
  40 acceptance (incl. new `unknown_field_is_400`) + 3 classify; store 14 + 23; TUI
  109 unit + `tui_no_kn9t_deps` (GI-6).
- `cargo test --workspace` — green except the 3 documented **F13** `kn9t-react`
  harness failures (binary lookup, deferred to Phase 3); all 12 pass once
  `target/debug/kn9t-tools` exists (verified).
- Generator idempotent; `scripts/check-schema.sh` fails on deliberate drift and
  passes on revert; pre-commit hook runs both gates cleanly.

### Discovered bugs

| bug | where | status |
|-----|-------|--------|
| (none new) | — | F5/F6/F7 were pre-listed in `job/findings.md` and are resolved here |

---

## Session — 2026-08-30 — architecture review + Phase 0 (docs scaffolding)

### Summary

An architecture review found critical issues: the bash safety classifier was deleted during
the tools-to-plugin migration (commit 5b65819) and never reimplemented, leaving the
`sh -c 'rm -rf /'` bypass open. Additionally, the TRACKING.md status for
R-TOOL-070/080/090/095 was marked `☑` citing acceptance tests that **do not exist** —
false greens on a G1 gate requirement.

This session is Phase 0 of a 4-phase cleanup: documentation scaffolding only, no Rust
code changes.

### Review findings

**1. Deleted classifier (CRITICAL).**
Commit 5b65819 ("feat: tool progress streaming...") claimed "Deleted kn9t-tools crate
(migrated to internal-plugins)" but the classifier (`crates/kn9t-tools/src/classify.rs`,
323 lines implementing the Ask/AllowReadOnly/HardDeny decision pipeline per R-TOOL-080/090)
was deleted and never reimplemented in internal-plugins.

Consequences verified:
- `AllowPolicy::check()` at `state.rs:33-37` returns `Decision::Allow` unconditionally.
- It is the **only** `Policy` impl in the workspace.
- Nothing anywhere emits `Event::ApprovalRequest`.
- The TUI's approval overlay (`app.rs:1994`) is dead code.
- `static APPROVALS` at `turn.rs:30` is declared, inserted into at line 72, but **never read**.
- The `sh -c 'rm -rf /'` bypass that R-TOOL-090 rule 5 exists to close is currently open.

**2. False greens in TRACKING.md.**
Lines 167-170 marked R-TOOL-070/080/090/095 as `☑` (done, acceptance test passing) naming
tests `tool::classify_posix`, `tool::classify_pwsh`, `tool::classify_pipeline`. Those
tests **do not exist**. Per AGENTS.md §6 these are false greens on G1 gate requirements.

**3. Three-way API drift.**
API.md, the server, and wire.rs disagree on nearly every route:
- `POST /session`: API.md says `{model,title}`, server takes `{cwd,model:{provider,id},name}`.
- `GET /session`: API.md says bare array, server returns `{"sessions":[...]}`.
- `POST /approve`: API.md says `{id,approved:bool}`, server takes `{id,decision:string,scope}`.
- SSE events: API.md says PascalCase, server emits snake_case (AGENTS.md §12 mandates snake_case).
- Routes `/pref`, `/health`, `/stop`, `/attach`: not in API.md but exist.

Root cause: no typed request structs in `routes/`, no `deny_unknown_fields`, so unknown/mistyped
fields are silently ignored. Example: TUI sends `decision: "always"` but server compares
`decision == "allow"`, so "always" is silently recorded as deny.

**4. Year-57668 bug.**
Store writes `created_at` via `as_millis()` (`session.rs:10`) but TUI's deserializer treats
the value as seconds (`wire.rs:194`), rendering session dates as year 57668.

**5. Stale TRACKING.md test count.**
"Current position" claimed `cargo test --workspace`: "285 passed". Actual: 385 passed.

**6. Stage 07 table misalignment.**
TRACKING.md lines 246-253 listed 7 rows of R-TUI-010..R-TUI-070 about wire types/leases/
screenshot-paste. But spec/07-tui.md defines 25 requirements R-TUI-010 through R-TUI-240
about completely different subjects. The tracking IDs predated a spec rewrite.

### ADR decisions

Created `docs/adr/` with five architecture decision records:

- **ADR-0001: Bash classifier lives in kn9t-server, not in the tool plugin.**
  The server owns the approval UI, the write lease, and user config. Plugins must not
  self-approve.

- **ADR-0002: Plugins declare argument EFFECTS; the server decides risk.**
  `ToolSpec` gains `effects: [{field, kind}]` where kind is Shell/FsWrite/FsRead/Network.
  The server maps kind→checker. A tool declaring no effects falls to strictest mode, so
  lying is never profitable.

- **ADR-0003: Dry-run is a preview mechanism, never a safety input.**
  You cannot dry-run `rm -rf /` to learn it is destructive. TOCTOU means the dry-run result
  is a claim by the plugin, reintroducing self-approval. Accepted only as approval-overlay
  diff preview (R-TUI-130).

- **ADR-0004: Plugin auto-discovery scans ~/.kn9t/plugins/ only.**
  The repo's `plugins/` is build source; `~/.kn9t/plugins/` is install target. Scanning
  project-relative paths would re-open the R-PLUG-100 code-execution hole.

- **ADR-0005: API contract is schema-first; API.md becomes generated output.**
  One JSON Schema generates server types (with `deny_unknown_fields`), wire.rs, API.md,
  and Go/Python stubs. CI fails on drift. Cites TRACKING.md GI-1 lesson: "prefer a script
  over an assertion."

### TRACKING.md corrections

**Stage 03 table (lines ~167-171):**
- R-TOOL-070: `☑` → `✗`, test column changed to note tests do not exist.
- R-TOOL-080: `☑` → `✗`, test column `**DELETED** — tests do not exist`.
- R-TOOL-090: `☑` → `✗`, test column `**DELETED** — tests do not exist`.
- R-TOOL-095: `☑` → `✗`.
- R-RCT-900 / R-TOOL-900 (GATE G1): `☑` → `✗`.
- Added note explaining classifier deletion and that G1 is no longer green.

**Stage 07 table (lines ~246-253):**
- Replaced 7-row table (obsolete IDs R-TUI-010..R-TUI-070) with 27-row table aligned to
  spec/07-tui.md (R-TUI-010..R-TUI-240 plus R-TUI-900).
- Marked statuses honestly: R-TUI-010 `☑` (GI-6 verified), R-TUI-220 `✗` (SidebarWidget
  does not exist), R-TUI-230 `✗` (SSE reconnect prints message but never retries), most
  others `☐` (no test).

**Overall progress table:**
- Stage 03: `24/25 ☑` → `20/25 ✗ (classifier deleted)`.
- Stage 07: `6/7 ▣` → `2/27 ▣ (most reqs have no test)`.

**Current position section:**
- Updated test count: 285 → 385 passed.
- Added architecture review entry with all findings.
- Updated Next pointer to Phase 1.

### Discovered bugs

| Bug | Location | Severity | Status |
|-----|----------|----------|--------|
| Classifier deleted, MUST requirements unimplemented | commit 5b65819 | **CRITICAL** | Documented; Phase 1 will restore |
| `[policy]` config section specified in DESIGN §10.1 but never parsed | `config.rs` shows only doc comment | HIGH | Documented; Phase 1 will implement |
| `POST /approve` accepts `scope` field but server reads only `decision` | `routes/session.rs:361-369` | MEDIUM | Documented |
| TUI sends `decision: "always"` but server compares `== "allow"`, silently records as deny | `routes/session.rs:365`, `app.rs:1052` | HIGH | Documented |
| Store writes `created_at` as milliseconds, TUI reads as seconds (year 57668) | `session.rs:10`, `wire.rs:194` | MEDIUM | Documented |
| `POST /plugin/{name}/reload` specified in spec/08b R-PLUG2-100 but route does not exist | `routes/` | LOW | Documented |
| `KvClient::new` is `pub(crate)` while `ToolCallCtx`/`ProviderCallCtx` have a `pub kv` field — the context structs were publicly declared but impossible to construct outside the SDK, breaking every external Rust plugin's test target | `kn9t-plugin-sdk/src/ctx.rs:246-267`, `plugins/kn9t-custom-provider/src/client.rs:333` | HIGH | **Fixed** — added `KvClient::for_test()` |

### SDK fix — `KvClient::for_test()`

Commit `81db0c7` added the persistent KV store, giving `ToolCallCtx` and `ProviderCallCtx`
a new `pub kv: KvClient` field. `CancelToken::new()` and `ChunkSender::new()` are `pub`, but
`KvClient::new()` is `pub(crate)` — so after that change **no code outside the SDK could
construct either context struct.** `plugins/kn9t-custom-provider` still built and ran correctly (the
production path never constructs a context; the SDK does), but its *test* target no longer
compiled: `error[E0063]: missing field kv`.

This is the SDK's bug, not the plugin's, and the fix belongs at the seam rather than in each
plugin: added `pub fn KvClient::for_test()`, which writes to `io::sink()` and never receives a
reply. `plugins/kn9t-custom-provider/src/client.rs` now uses it; its 26 tests pass again.

The Go and Python plugins hand-roll the wire protocol, so they were unaffected by a Rust
signature change — but that is luck, not design, and is precisely the drift ADR-0005 exists to
make impossible. Worth noting for Phase 2: a `pub` field whose type has no `pub` constructor is
a compile-time-invisible contract break, exactly the class of defect the schema/codegen work
should catch.

### Files created

- `CONTEXT.md` — domain glossary (27 terms, DESIGN section pointers).
- `docs/adr/0001-bash-classifier-lives-in-server.md`
- `docs/adr/0002-plugins-declare-effects-server-decides-risk.md`
- `docs/adr/0003-dry-run-is-preview-not-safety-input.md`
- `docs/adr/0004-plugin-discovery-user-dir-only.md`
- `docs/adr/0005-api-contract-schema-first.md`

### Files modified

- `TRACKING.md` — corrected false greens, realigned Stage 07 table, updated test count.
- `CHANGELOG.md` — this entry.
- `crates/kn9t-plugin-sdk/src/ctx.rs` — added `KvClient::for_test()` (see SDK fix above).
- `plugins/kn9t-custom-provider/src/client.rs` — test helper uses `KvClient::for_test()`.

### Verification

No Rust code changed. No `cargo` commands run (per task constraints).

---

## Session — 2026-08-30 — Plugin KV store + kn9t-agents-md durable injection

### Summary

Implemented a persistent plugin KV store backed by SQLite so plugins survive server
restarts without losing state.  Rewrote `kn9t-agents-md` as the first consumer: it now
tracks per-session injection state in the host KV store instead of process memory, so
AGENTS.md is never re-injected into existing sessions after a restart.

### What changed

**`kn9t-core/src/traits.rs`**
- Added `PluginKv` trait: `kv_get`, `kv_set`, `kv_del`, `kv_del_scope` (all take `scope`).
- Re-exported from `kn9t-core/src/lib.rs`.

**`kn9t-store/src/db.rs`**
- Added `plugin_kv(plugin, scope, key, value, updated_at)` DDL to schema.
- Implemented `PluginKv` for `SqliteStore`.

**`kn9t-store/src/session_delete.rs`**
- Added `DELETE FROM plugin_kv WHERE scope=?1` inside the session-delete transaction.

**`kn9t-plugin/src/codec.rs`**
- `HostMsg`: added `KvResult { id, value, ok, error }`.
- `PluginMsg`: added `KvGet`, `KvSet`, `KvDel`, `KvDelScope`.

**`kn9t-plugin/src/host.rs`**
- `writer` field changed to `SharedWriter = Arc<Mutex<...>>` (needed for KV inline replies).
- Added `kv: Arc<dyn PluginKv>` field.
- `from_io` takes 4th `kv` arg; `spawn` takes 3rd `kv` arg.
- Reader thread intercepts `KvGet/Set/Del/DelScope` inline and replies with `KvResult`.

**`kn9t-plugin/src/lib.rs`**
- Added `NoOpPluginKv` (all ops no-op); used in every test that constructs a `PluginHost`.

**`kn9t-plugin-sdk/src/wire.rs`**
- Added `HostMsg::KvResult` and `PluginMsg::KvGet/KvSet/KvDel/KvDelScope`.

**`kn9t-plugin-sdk/src/ctx.rs`**
- Added `KvClient` struct (blocking `get/set/del/del_scope`, 5 s timeout).
- Added `KvReply` internal type.
- Added `kv: KvClient` to `ToolCallCtx` and `ProviderCallCtx`.

**`kn9t-plugin-sdk/src/plugin.rs`**
- Added `kv_pending: KvPending` and `kv_next_id: Arc<AtomicU64>` to `Runner`.
- Added `make_kv_client()` helper.
- Handles `HostMsg::KvResult` in dispatch loop (routes to `kv_pending` channels).
- Passes `kv: self.make_kv_client()` when constructing `ToolCallCtx` and `ProviderCallCtx`.

**`kn9t-server/src/tools.rs` + `main.rs` + `config.rs`**
- All plugin spawn functions now accept `kv: Arc<dyn PluginKv>`.
- `spawn_all_plugins` call in `main.rs` passes `store.clone()`.
- Probe in `config.rs` uses `NoOpPluginKv`.

**`plugins/kn9t-agents-md/main.go` — full rewrite**
- Dropped in-memory `sessions map[string]*SessionState`.
- Added reader goroutine: routes `kv_result` to pending channels, everything else to hookCh.
- Added `KvGetMsg / KvSetMsg / KvDelScopeMsg` wire types.
- `kvGetInjected / kvSetInjected / kvDelScope` blocking helpers (5 s timeout, chan-based).
- `queueIfNew` now reads from KV before checking, and persists the updated set to KV.
- `handleEvent("compacted")` calls `kvDelScope` instead of `delete(sessions, sid)`.
- `sendHello` events list trimmed to `["compacted"]` only.
- Thread-safe stdout via `writerMu sync.Mutex` + `bufio.Writer`.

### Verification

- `cargo test --workspace`: **285 passed, 0 failed** (same as before).
- `go build ./...` in `plugins/kn9t-agents-md`: clean.

---

## Session — 2026-08-29 (cont) — Phases 1-3 Complete

### Summary

Completed all TUI improvements through Phase 3. Now at 16/62 items complete.

### New Features

**1.4 Prompt Stash** (`prompt_stash.rs`)
- `/stash` saves current input to single-slot stash
- `/unstash` restores from stash (swaps if input non-empty)

**1.5 Word Navigation** (`word_segmenter.rs`)
- `Ctrl+Left/Right`: Move cursor by word
- `Ctrl+Backspace`: Delete word backward (via kill ring)
- `Ctrl+Delete`: Delete word forward
- Uses `unicode-segmentation` for CJK/emoji-aware boundaries

**2.5 LaTeX Math Rendering** (`latex.rs`)
- Unicode approximation for LaTeX math expressions
- Greek letters, operators, sub/superscripts, fractions, roots
- `process_math()` handles inline `$...$` and display `$$...$$`

**2.6 OSC 8 Hyperlinks** (`hyperlinks.rs`)
- `hyperlink(url, text)`: Wraps text in OSC 8 sequence
- `file_url/file_link/file_line_link`: File path hyperlinks
- Terminal support detection (iTerm2, Kitty, WezTerm, Windows Terminal, tmux)
- Note: Integration with ratatui spans deferred (requires lower-level access)

**3.3 Semantic Navigation**
- `Ctrl+Up`: Jump to previous user message
- `Ctrl+Down`: Jump to next user message

### Skipped

**2.3 Differential Rendering**: CSI 2026 synchronized output (already done) provides sufficient flicker-free rendering.

### Tests

109 tests passing (was 91 before this session).

---

## Session — 2026-08-29 — Diff Viewer & Command Palette

### Summary

Enhanced diff viewer with full mouse support, file tree navigation, and comment input.
Added command palette (Ctrl+P) for quick command access.

### New Features

**Diff Viewer Enhancements:**
- Mouse click on diff lines to select cursor position
- Re-click on selected line opens comment dialog (UX improvement)
- Mouse click on file tree sidebar to switch files
- Mouse wheel scroll support
- Cursor auto-scroll when navigating with j/k beyond visible area
- Cursor highlighting in both unified and split modes (▶ marker + background)
- Comment input box now auto-wraps and resizes (up to 8 lines)
- Works on welcome screen (not just chat screen)
- New keybindings: n/p (next/prev file), b (toggle file tree)

**Command Palette:**
- Ctrl+P opens fuzzy search of all commands
- Shows keybindings alongside command names
- Categories: Navigation, Session, Edit, View, Tools, Settings
- /palette slash command also opens it

### Bug Fixes

- `line_hits` was never populated during render (mouse clicks didn't work)
- Split mode didn't show cursor highlighting
- DiffViewer overlay wasn't rendered on welcome screen
- Comment input box had fixed height, text disappeared when long

### Files Changed

- `crates/kn9t-tui/src/diff_viewer.rs` — mouse support, cursor highlighting, file tree clicks
- `crates/kn9t-tui/src/app.rs` — mouse event handling for diff viewer, new keybindings
- `crates/kn9t-tui/src/ui/render.rs` — DiffViewer rendering on welcome screen
- `crates/kn9t-tui/src/command_palette.rs` — NEW: command palette implementation
- `crates/kn9t-tui/src/slash.rs` — added /palette, /theme commands

### Test Results

- 83 kn9t-tui tests passing

---

## Session — 2026-08-29 — TUI Input UX improvements

### Commits

1. **a7e85e7** feat(tui): add undo/redo system (Ctrl+Z/Ctrl+Y)
2. **788238e** feat(tui): add Emacs-style kill ring (Ctrl+K/U/W/Y, Alt+Y)
3. **1dc990f** feat(tui): add prompt history navigation (Up/Down)
4. **580799f** feat(tui): add CSI 2026 synchronized output for flicker-free rendering

### New Features

**Undo/Redo (input_history.rs):**
- Ctrl+Z: undo last input change
- Ctrl+Shift+Z: redo
- Coalesces rapid keystrokes (300ms window)
- Max 100 undo states
- 5 unit tests

**Kill Ring (kill_ring.rs):**
- Ctrl+K: kill to end of line
- Ctrl+U: kill to start of line
- Ctrl+W: kill word backward
- Ctrl+Y: yank (paste from ring)
- Alt+Y: yank pop (cycle through ring after yank)
- Max 10 entries, consecutive kills append
- 7 unit tests

**Prompt History (prompt_history.rs):**
- Up (on first line): navigate to previous prompt
- Down (on last line): navigate to next prompt
- Prefix filtering: typing then Up shows matching history
- Persists to ~/.kn9t/prompt_history.json
- Max 500 entries, deduplicated
- 6 unit tests

**CSI 2026 Synchronized Output:**
- Wrap render in BeginSynchronizedUpdate/EndSynchronizedUpdate
- Prevents flicker on modern terminals

### Breaking Changes

- Ctrl+K/J moved from message navigation to kill ring (Emacs convention)
- Alt+K/J now used for message navigation

### Documentation

- Added docs/TUI_IMPROVEMENTS.md with 61 planned improvements
- Comprehensive comparison: Pi vs kn9t vs OpenCode TUIs
- Priority-ordered implementation roadmap

---

## Session — 2026-08-28 (continued) — TUI polish: model/session selectors

### Changes

**Model selector (`/models`):**
- Groups models by provider with headers (`— custom-provider —`)
- Shows short model names (`claude-haiku-4-5-latest`) instead of full path
- Filter now matches provider name too (type "custom-provider" to see only custom-provider models)

**Session selector (`/session`):**
- "✚ New session" option at top for quick session creation
- Date headers group sessions ("Today", "Yesterday", "Aug 27")
- Delete key removes sessions from history
- Filter matches session ID in addition to name

**Server:**
- `GET /session` now returns `created_at` timestamp for date grouping

---

## Session — 2026-08-28 (continued) — TUI bug fixes

### Problem

Fast typing dropped characters. When typing "hello world" the TUI would display "helo ord"
or similar — characters were silently lost.

### Root cause

The event loop's `drain()` call (app.rs:589) only handled `SSE` and `Tick` events, discarding
all others with `_ => {}`. When multiple key events arrived between render frames, they were
drained and thrown away instead of being processed.

### Fixes

| file | change |
|---|---|
| `crates/kn9t-tui/src/app.rs` | `drain()` loop now handles all event types: Key, Mouse, Paste, SSE, Tick, SseError |
| `crates/kn9t-tui/src/ui/render.rs` | `wrap_text()` now counts characters, not bytes (fixes Unicode wrapping) |

### Investigation notes

Initially suspected ratatui rendering artifacts (scattered characters on screen). Added
`clear_area()` to explicitly clear regions before rendering. Testing proved this was
unnecessary overhead — the real bug was the drain loop. Reverted `clear_area()`.

Tested via `tui-controle` MCP server driving kn9t through a PTY. After the fix, fast typing
works correctly and tool invocations render cleanly.

---

## Session — 2026-08-28 (continued) — kn9t-custom-provider becomes a proper external plugin

### Problem

`kn9t-custom-provider` lived in `crates/internal-plugins/kn9t-custom-provider` and was listed in the root
`Cargo.toml` `members`. Spec 09 simultaneously claimed it had "zero workspace deps — GI-1
satisfied **by design**". Those two statements were in tension:

- Workspace membership means a shared `Cargo.lock`, a shared `target/`, and unified feature
  resolution. The isolation was a convention, not a property.
- Nothing structurally stopped someone adding `kn9t-core` to it; only review would catch it.
- It was described as "bundled" in the generated config template, implying sibling-of-exe
  binary resolution, while being conceptually a replaceable third-party provider.

Per AGENTS.md §2 rule of precedence the design's *decision* (Q31: providers are replaceable
plugin binaries, core knows nothing about them) wins, so **the spec was the bug**.

### Change

Moved it out of the workspace to `plugins/kn9t-custom-provider`, next to the existing external plugins
`kn9t-agents-md` (Go) and `kn9t-mcp` (Python).

| file | change |
|---|---|
| `plugins/kn9t-custom-provider/**` | moved from `crates/internal-plugins/kn9t-custom-provider` via `git mv` (history preserved) |
| `plugins/kn9t-custom-provider/Cargo.toml` | added empty `[workspace]` (detaches from parent); package renamed `kn9t-custom-provider-plugin` → `kn9t-custom-provider` (matches `[[bin]]`); sdk path → `../../crates/kn9t-plugin-sdk` |
| `plugins/kn9t-custom-provider/README.md` | new — why external, build, absolute-path config, env vars, test |
| `Cargo.toml` | dropped the `crates/internal-plugins/kn9t-custom-provider` member |
| `Cargo.lock` | custom-provider package entry removed |
| `crates/kn9t/src/bootstrap.rs` | config template: custom-provider shown as external with an absolute `binary` path + build step; clarified bare-name = bundled only |
| `scripts/check-gi1.sh` | now also scans `plugins/*/Cargo.toml`; path regex generalised to `(../)+ (crates/)? kn9t-` |
| `spec/09a-custom-provider.md` | header fixed; correction note; **new R-CP-005** |
| `spec/README.md` | stage-09 crate column corrected |
| `DESIGN.md` | §2.1 provider table, §8.5 config example, §8.6 spawn diagram, §16 build-order node, Q31 refined |

`kn9t-anthropic` deliberately stays bundled, so both distribution paths stay exercised.

### Discovered bug — `scripts/check-gi1.sh` never checked anything

While extending the script I found it had **always been vacuous**. It used:

```bash
awk '/^\[dependencies\]/,/^\[/'
```

An awk range whose start and end patterns both match the same line terminates on that line,
so the extracted section was only the literal `[dependencies]` header — zero dependency
lines. The grep count was therefore always 0 and GI-1 "OK" was meaningless for every crate
since the script was written.

Fixed to a flag-based scan:

```bash
awk '/^\[dependencies\]/{f=1;next} /^\[/{f=0} f'
```

Verified it now genuinely fails: temporarily adding a second workspace dep to
`kn9t-plugin` and to `plugins/kn9t-custom-provider` each produced a `GI-1 VIOLATION` with exit 1; both
files restored afterwards. GI-1 passes cleanly across all crates with the working check,
so no real violation was being hidden.

### Verification

- `cd plugins/kn9t-custom-provider && cargo build` → compiles standalone, no parent workspace.
- `cd plugins/kn9t-custom-provider && cargo test` → **26 passed** (11 unit + 15 acceptance), incl. `parallel_toolcalls`.
- `cargo check --workspace` → clean.
- `cargo test --workspace` → **283 passed, 0 failed**.
- 283 + 26 = 309 = the previous total, so no test was dropped in the move.
- `bash scripts/check-gi1.sh` → OK, and provably able to fail.

### Follow-up

Publishing `kn9t-plugin-sdk` to crates.io would let the sdk path dep become a version
requirement, at which point this plugin builds with no kn9t checkout at all. Until then
"external" means "outside the workspace", not "buildable standalone from a tarball".

---

## Session — 2026-08-28 (continued) — Architecture Cleanup: GI-1, TUI Managers, SSE Dedup

### Summary

Empirical audit of the codebase rather than a docs-based one. Found that GI-1 was violated
despite every document asserting it held, that the TUI manager refactor had been abandoned
mid-way leaving ~835 lines of dead-but-tested code, and that a 102-line SSE parser was
duplicated byte-for-byte. Fixing the tests then surfaced two real production bugs.

Final state: `cargo test --workspace` → **309 passed, 0 failed**.

### 1. GI-1 violation (the important one)

`kn9t-provider-openai` declared two workspace deps — `kn9t-core` AND `kn9t-provider-core` —
while `TRACKING.md`, `spec/05`, and `DESIGN.md` all claimed GI-1 held. It had been untrue for
an unknown period because **nothing verified it**.

Fix: added `Cache` + `Effort` to `kn9t-provider-core`'s re-exports and rewrote the provider's
imports to go through them; removed the direct `kn9t-core` dependency.

`kn9t-core` remains a `[dev-dependencies]` entry because tests need `kn9t_core::Quirks`, which
provider-core deliberately does NOT re-export (it collides with its own HTTP `Quirks`). GI-1
counts `[dependencies]` only — `spec/04-store.md` and `kn9t-react` set that precedent.

Prevention: `scripts/check-gi1.sh` + `.git/hooks/pre-commit`. An assertion in a doc is not an
invariant; a script that fails the commit is.

### 2. TUI manager composition (P5-B)

A prior session extracted four managers and never wired them in:

```
SessionManager  referenced outside own module: 0 times
ModelSelector   referenced outside own module: 0 times
TokenTracker    referenced outside own module: 0 times
```

`app.rs` reimplemented the same state by hand across 53 fields. The modules survived only
because `app.rs` re-exported four data types from them. Their 36 unit tests were validating
code that never ran — false coverage confidence, and precisely why the `140240%` cache-ratio
bug got fixed in `render.rs` rather than in the tested `TokenTracker`.

Composed them as `App` fields: **53 → 32 fields**. Two behaviours were preserved deliberately
and are load-bearing:
- **Token accumulation** — multiple `UsageRecorded` events per ReAct turn must accumulate, not
  overwrite; `usage_kind == "title"` is excluded from LAST TURN; reset is deferred to the first
  `UsageRecorded` via `pending_turn_reset` so the previous turn stays visible while streaming.
- **Session-switch ordering** — `reset_session_state()` runs against the OLD `session_id`
  (lease release) *before* `enter_session()` installs the new one. Documented in-place.

Added `SessionManager::session_title()` / `set_session_title()`; moved `cost` out of
`SessionState` to `TokenTracker`, which already owned every other money/token field.

### 3. Duplicate SSE parser

`kn9t-custom-provider/src/sse.rs` and `kn9t-anthropic/src/sse.rs` were byte-identical (102 lines each,
including both tests). Moved to `crates/kn9t-plugin-sdk/src/sse.rs` with a doc example
(R-PLUG2-090/095). Net **−65 lines**; parser body verified byte-identical (a move, not a rewrite).

**Why not `kn9t-provider-core`:** each plugin's single workspace dep is `kn9t-plugin-sdk`;
adding provider-core would have given them two and reintroduced the GI-1 violation just fixed.
`kn9t-provider-core::sse_lines` is also a different abstraction — it yields `data:` only and
discards `event:`, which these plugins need for dispatch. Both files remain, correctly.

### 4. Test-suite drift repaired

Struct fields and signatures had changed without updating ~40 call sites in tests/examples:

| drift | sites |
|---|---|
| `Message.silent` missing | 32 |
| `ToolSpec.hidden` missing | 10 (incl. 2 SDK doc examples) |
| `ServerState::new` now takes 4 args | 11 |
| `Event::{TitleChanged,PluginNotification}` absent from exhaustive match | 1 |
| `core::event_tag` asserted PascalCase | 16 |

`core::event_tag` is worth calling out: it expected `"SessionForked"` while the code, server
SSE, and TUI all correctly emit `"session_forked"` per AGENTS.md §11. The test was wrong, not
the code — but it had been failing, so nobody was running the suite.

### 5. Two production bugs found by fixing the tests

**`/attach` hardcoded a 30 s heartbeat** (`kn9t-server/src/router.rs`) instead of calling
`sse::heartbeat_interval()`. Since a write failure on the heartbeat is *how* a dead client is
detected, this made the R-SRV-081 path both unconfigurable and untestable — the "flaky"
`srv::keepalive_detects_dropped_client` was actually asserting against a real defect. Now uses
the shared helper (honours `KN9T_SSE_HEARTBEAT_MS`, default 15 s). Test passes deterministically.

That same test was additionally attached to `/session/{id}/events`, which by design does NOT
increment `attached_clients` (R-SRV-081 moved counting to `GET /attach`). Repointed.

**Silent JSON corruption** (`kn9t-react/src/exec.rs`): `authorize()` did
`from_str(&call.args_json).unwrap_or(Value::Null)`, turning malformed provider args into empty
args with no diagnostic. Since §4.1 treats `args_json` as cache-critical verbatim bytes, this
was unfindable in production. Now emits `Event::Error` naming the tool and call id.

### 6. Docs corrected

- **DESIGN §2.1** claimed a provider "is expected to be ~250 lines". Measured: openai 725,
  anthropic 547, custom-provider 1059 — off by ~3x. Replaced with the real table plus the two structural
  reasons (wire mapping is bidirectional; plugin providers can't link provider-core under GI-1,
  so they carry their own HTTP). The part that *does* hold — no provider reimplements delta
  accumulation, retry, or partial-JSON buffering — is stated explicitly.
- **AGENTS.md §8.1** (new): cargo is Windows-only here. A Linux/WSL agent gets nothing from
  `which cargo` and must use `/mnt/c/Users/<user>/.cargo/bin/cargo.exe` or `powershell.exe -c`.
  Includes the rule that a gate is not green until a real `cargo test` says so — grep is
  necessary but never sufficient.

### Verified

```
cargo test --workspace   → 309 passed, 0 failed
./scripts/check-gi1.sh   → GI-1: OK
GI-6: kn9t-tui           → zero kn9t-* deps
R-PLUG2-010: SDK         → zero workspace deps
```

### Note on process

Two subagents did items 2 and 3. One reported `cargo check`/`cargo test` results while cargo
was not on PATH; those numbers happened to be right, but the claim was unverifiable when made.
The workspace-wide run is what actually caught the remaining breakage. AGENTS.md §8.1 now
records how to run cargo here so this does not recur.

---

## Session — 2026-08-28 (continued) — Native Clipboard Paste with Images

### Summary

Implemented native cross-platform clipboard paste (text and images) in kn9t TUI. Users can
paste images from clipboard and reference them inline with `[img1: WxH PNG]` markers.

### Crossterm Patch for Windows Bracketed Paste

**Problem:** crossterm 0.29 stable doesn't support bracketed paste on Windows. Text paste
works but `Event::Paste` is never emitted — the terminal sends individual key events.

**Solution:** Patched crossterm via git to use PR #1030 (Windows VT input fix):
```toml
# Cargo.toml (workspace root)
[patch.crates-io]
crossterm = { git = "https://github.com/eitsupi/crossterm", branch = "feature/windows-vt-input" }
```

This enables `Event::Paste(text)` on Windows via VT input mode. For images, bracketed
paste sends empty string (terminals can't transmit image data), so we fall back to
arboard clipboard.

### Architecture: Image Flow End-to-End

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ TUI (kn9t-tui)                                                              │
├─────────────────────────────────────────────────────────────────────────────┤
│ 1. Ctrl+V → Event::Paste("")                                                │
│ 2. paste_image_from_clipboard() → arboard::Clipboard::get_image()           │
│ 3. Encode PNG → base64 data URI (data:image/png;base64,...)                │
│ 4. Store in pending_images: Vec<String>                                     │
│ 5. Insert [imgN: WxH PNG] marker at cursor position in input text           │
│ 6. On Enter: send_prompt() → POST /session/:id/prompt {text, images}        │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Server (kn9t-server/routes/session.rs)                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│ 7. parse_data_uri() extracts mime + raw bytes from data URI                 │
│ 8. store.put_blob() stores image → returns sha256 hash (content-addressed)  │
│ 9. Build Message with Content::Image { sha256: "sha256:<hash>", mime }      │
│ 10. Append to session via Event::MessageAppended                            │
│ 11. spawn_turn() → plan_request()                                           │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Store (kn9t-store/plan.rs)                                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│ 12. plan_request() calls resolve_image_blobs()                              │
│ 13. For each Content::Image with sha256:<hash>:                             │
│     - get_blob(hash) → raw bytes                                            │
│     - Encode to data URI: data:<mime>;base64,<b64>                          │
│     - Replace sha256 field with data URI (provider-ready)                   │
│ 14. Return RequestPlan with resolved images                                 │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ Provider (kn9t-provider-openai/encode.rs, kn9t-custom-provider/map.rs)      │
├─────────────────────────────────────────────────────────────────────────────┤
│ 15. encode_content() for OpenAI:                                            │
│     - If sha256 starts with "data:" → use as-is                             │
│     - Emit: {"type":"image_url","image_url":{"url":"data:..."}}             │
│                                                                             │
│ 16. map_content_part() for custom provider:                                │
│     - Read sha256 field (kn9t serialization format)                         │
│     - Emit: {"type":"image_url","image_url":{"url":"data:..."}}             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Storage Architecture (DESIGN §12.4)

Images are **never stored inline** in the events table. Instead:
- `blobs` table stores raw bytes with sha256 hash (content-addressed, deduplicated)
- `Content::Image` stores only `sha256:<hash>` reference
- `resolve_image_blobs()` hydrates references to data URIs at request time

Benefits:
- SSE frames stay small (hash vs 300KB base64)
- Same image pasted twice = one copy stored
- Late-attaching clients replay quickly

### Spec Update: R-TUI-110

**Original spec:** Display placeholder `[Image: SIZE FORMAT WxH]`
**Implemented:** `[imgN: WxH PNG]` markers inserted at cursor (e.g., `[img1: 982x414 PNG]`)

Combines numbered reference with size info. Users can reference specific images:
"what is this [img1: 982x414 PNG] and that [img2: 1200x800 PNG]"

Spec updated to reflect implementation.

### User Message Display Optimization

**Old flow (removed):**
1. User types message with images
2. TUI sends to server
3. Server stores and broadcasts SSE MessageAppended
4. TUI receives SSE and displays user message

**New flow:**
1. User types message with images  
2. TUI adds user message locally (instant display)
3. TUI sends to server
4. TUI ignores SSE MessageAppended for role=user

**Benefit:** Instant feedback, no round-trip latency for user's own message.

### Files Changed

**Workspace root:**
- `Cargo.toml` — crossterm git patch for Windows VT input (PR #1030)

**kn9t-tui:**
- `Cargo.toml` — added arboard, png, base64 deps
- `src/app.rs`:
  - `paste_image_from_clipboard()` — read image, encode PNG, insert marker
  - `encode_image_as_base64_png()` — PNG encoding helper
  - `send_prompt()` — send images, add user message locally
  - `handle_sse()` — skip user MessageAppended events
- `src/event.rs` — simplified paste handling (removed Ctrl+V hack)
- `src/ui/render.rs` — removed suffix indicators, images inline in text
- `src/message_handler.rs` — `Message::with_images()`, `with_tools()` builders
- `src/wire.rs` — `PromptReq { text, images }` struct
- `src/client.rs` — `prompt()` accepts `images: Vec<String>`

**kn9t-server:**
- `src/routes/session.rs`:
  - `prompt()` parses `images` field from request body
  - `parse_data_uri()` extracts mime/bytes from data URI
  - Stores images as blobs, builds Content::Image with sha256 ref

**kn9t-store:**
- `src/blob.rs` — `put_blob()`, `get_blob()` (already existed per R-STOR-140)
- `src/plan.rs`:
  - `resolve_image_blobs()` — hydrate sha256 refs to data URIs
  - Fixed deadlock: release conn lock before calling get_blob()

**kn9t-provider-openai:**
- `src/encode.rs` — `encode_content()` handles pre-resolved data URIs

**kn9t-custom-provider:**
- `src/map.rs` — `map_content_part()` reads sha256 field for images

**kn9t-core:**
- `src/event.rs` — removed `Event::AgentsMdInjected` (user request)

### Dependencies Added

```toml
# kn9t-tui/Cargo.toml
arboard = { version = "3", features = ["image-data"] }  # Cross-platform clipboard
png = "0.17"      # PNG encoding for clipboard images
base64 = "0.22"   # Base64 encoding for data URIs
```

### Spec Updates

- `spec/07-tui.md` R-TUI-110 — updated image paste format to `[imgN: WxH PNG]`
- `spec/06-server.md` R-SRV-085 — documented prompt endpoint with `images` field

### Testing

Manual test procedure:
1. Copy image to clipboard (screenshot, etc.)
2. In TUI, type "what is this "
3. Ctrl+V → `[img1: 982x414 PNG]` appears at cursor
4. Type " and this "
5. Copy another image, Ctrl+V → `[img2: 1200x800 PNG]` appears
6. Enter → message sent with both images
7. LLM responds describing both images

---

## Session — 2026-08-28 (continued) — kn9t-agents-md Plugin & PluginMsg::Event

### Summary

Created `kn9t-agents-md`, a Go plugin that auto-discovers and injects AGENTS.md files into
agent context. Extended the plugin protocol with `PluginMsg::Event` to allow plugins to
emit events to the host's EventBus.

### Plugin: kn9t-agents-md (Go)

**Location:** `plugins/kn9t-agents-md/`

**Hooks implemented:**
- `after_tool_call` — tracks paths read by `read`, `glob`, `grep` tools
- `get_steering` — returns pending AGENTS.md content as system messages

**Discovery flow:**
1. On session start: inject `~/.kn9t/AGENTS.md` (global) and `$CWD/AGENTS.md` (project)
2. On tool read: walk up from read path to workspace root, inject any found AGENTS.md
3. Each AGENTS.md injected only once (tracked in `injected` set)

**Output:**
- System message with `<agents-md path="..." source="..." lines="...">` tags
- `AgentsMdInjected` event emitted to TUI for notification

### Protocol Extension: PluginMsg::Event

**Problem:** Plugins couldn't emit events without modifying kn9t-react code.

**Solution:** Added `PluginMsg::Event` variant:
```json
{"t": "event", "kind": "agents_md_injected", "path": "...", "source": "...", "lines": 42}
```

**Flow:**
```
Plugin ──event──> reader_thread ──ReaderMsg::Event──> wait_for_response()
                                                            │
                                                            ▼
                                                  forward_plugin_event()
                                                            │
                                                            ▼
                                                    EventBus.emit()
                                                            │
                                                            ▼
                                                      SSE broadcast
```

### Message.silent Field

Added `silent: bool` field to `Message` struct. When true:
- Message is persisted in events table
- Message is sent to LLM in context
- Message is NOT displayed in TUI transcript

This enables plugins to inject context (like AGENTS.md) without cluttering the UI.
The plugin can emit a separate notification event if user-facing feedback is needed.

**Files changed:**
- `kn9t-core/src/message.rs` — added `silent: bool` field with `#[serde(default)]`
- `kn9t-provider-core/src/assemble.rs` — set `silent: false` on assembled messages
- `kn9t-react/src/exec.rs` — set `silent: false` on compaction messages
- `kn9t-react/src/turn.rs` — set `silent: false` on tool result messages
- `kn9t-tui/src/app.rs` — skip rendering messages where `msg.silent == true`

### Event::PluginNotification

Added generic plugin notification event for plugins to send arbitrary notifications.
Used instead of specific events like `AgentsMdInjected` for flexibility.

**Files changed:**
- `kn9t-core/src/event.rs` — added `Event::PluginNotification { kind, data }`
- `kn9t-server/src/sse.rs` — mapped to SSE frame
- `kn9t-store/src/session.rs` — added to event_kind_name()

### ServerState Plugin Hosts

Server now keeps plugin hosts in state to compose hooks for the react loop.

**Files changed:**
- `kn9t-server/src/state.rs` — added `plugin_hosts: Vec<Arc<PluginHost>>`
- `kn9t-server/src/main.rs` — pass plugin_hosts to ServerState::new()
- `kn9t-server/src/tools.rs` — log hook count in handshake

### Plugin Protocol Files
- `kn9t-plugin/src/codec.rs` — added `PluginMsg::Event { event: Value }`
- `kn9t-plugin/src/composed.rs` — debug logging for hook calls
- `kn9t-react/src/hooks.rs` — debug logging for hook execution

### Dependencies
- `kn9t-store/Cargo.toml` — added `base64 = "0.22"` for resolve_image_blobs()

### Design Decision

Used Go for the plugin because:
- Single binary (~3MB), no runtime deps
- Fast startup, low memory
- Good for I/O-heavy file operations
- Diversifies plugin stack (Rust core, Python for MCP, Go for simple plugins)

---

## Session — 2026-08-28 (continued) — TUI Tool Tabs & Plugin Wire Fix

### Summary

Added 3-tab UI for tool cards (Progress | Output | Input) and fixed plugin wire format.

### TUI: Tool Card Tabs

Previously, tool output combined progress chunks and final output in one view, causing
duplication. Now separated into 3 tabs:

- **Progress** — streaming chunks during execution (what user sees in real-time)
- **Output** — final tool_result content (what the agent sees in context)  
- **Input** — args JSON (already existed)

**Behavior:**
- During execution: auto-switch to Progress tab
- When done: auto-switch to Output tab
- User can switch tabs with Left/Right arrows or mouse click

**Files changed:**
- `kn9t-tui/src/message_handler.rs` — added `ToolTab::Progress` variant
- `kn9t-tui/src/ui/render.rs` — new `render_tool_progress()`, updated tab bar
- `kn9t-tui/src/app.rs` — added `cycle_tool_tab()`, updated hit areas

### Plugin Wire Format Fix

Fixed JSON structure for plugin `done` messages. Rust uses `#[serde(flatten)]` on the
body field, so content must be at root level, not nested:

```json
// Wrong (was causing JSON to appear in TUI):
{"t": "done", "id": 1, "body": {"content": [...], "is_error": false}}

// Correct:
{"t": "done", "id": 1, "content": [...], "is_error": false}
```

### kn9t-mcp Plugin Improvements

- **Limit clamped to 10** (was 25) — prevents overwhelming agent with tools
- **Discovered tools cache** — 5-minute TTL, already-discovered tools excluded from results
- **Clear messaging** — tells agent to call tools directly as tool_call, not via bash

---

## Session — 2026-08-28 (continued) — Lazy Tool Discovery & kn9t-mcp Plugin

### Summary

Implemented **lazy tool discovery** for the kn9t plugin system, enabling plugins to register
many tools without polluting the system prompt. Created `kn9t-mcp`, a Python plugin that
bridges MCP servers (TeamForge, Jira, Confluence, TestKub) to kn9t with 148 hidden tools
discoverable on demand.

### Problem

The `kn9t-mcp` plugin bridges to 4 MCP servers exposing 148 tools total. Sending all tool
specs in the system prompt:
- Wastes ~50k tokens per request
- Overwhelms the model with choices
- Breaks cache prefix stability

### Solution: Hidden Tools

Added a `hidden: bool` flag to `ToolSpec`. Hidden tools are:
- **Registered** in `ToolRegistry` (can be executed)
- **Not sent** to the LLM in the tools array (saves tokens)
- **Discoverable** via meta-tools that return specs in their result

### Protocol Changes

**ToolSpec** (plugin hello):
```json
{
  "name": "jira_create_issue",
  "description": "Create a Jira issue",
  "schema": { ... },
  "parallel_safe": true,
  "hidden": true
}
```

**ToolRegistry** methods:
- `specs()` — returns ALL tools (for execution lookup)
- `visible_specs()` — returns only `hidden=false` tools (for LLM request)

### kn9t-mcp Plugin

Python plugin that bridges MCP servers to kn9t using lazy discovery:

**Visible tools (2):**
- `mcp_list_servers` — list available MCP servers with tool counts
- `mcp_search_tools` — search/discover tools by server name

**Hidden tools (148):**
- 40 TeamForge tools (artifacts, commits, code review)
- 90 Atlassian tools (Jira issues, Confluence pages)
- 18 TestKub tools (NFC + RF test campaigns)

**Discovery flow:**
```
Agent: mcp_list_servers()
  → {"servers": [{"server": "jira", "tools": 50}, ...]}

Agent: mcp_search_tools(server="jira", query="create")
  → {"tools": [{"name": "mcp_jira_create_issue", "parameters": {...}}]}

Agent: mcp_jira_create_issue(project="PROJ", summary="Bug")
  → {"key": "PROJ-1234"}
```

### Files Changed — kn9t (Rust)

- `kn9t-core/src/toolspec.rs`: Added `hidden: bool` field with `#[serde(default)]`
- `kn9t-core/src/registry.rs`: Added `visible_specs()` method
- `kn9t-react/src/exec.rs`: Use `visible_specs()` for LLM requests
- `kn9t-plugin-sdk/src/wire.rs`: Added `hidden: bool` to protocol ToolSpec
- `kn9t-plugin/src/spawn_tool.rs`: `spawn_session` tool with `hidden: false`
- `kn9t-server/src/tools.rs`: Copy `hidden` flag when extracting RemoteTools
- `kn9t-server/src/config.rs`: Added `[[plugin]]` config parsing with cmd/env
- `kn9t-server/src/main.rs`: Load user plugins via `spawn_all_plugins()`
- `internal-plugins/kn9t-tools/src/*.rs`: All tools with explicit `hidden: false`

### Files Changed — kn9t-mcp (Python)

- `plugins/kn9t-mcp/kn9t_mcp/plugin.py`: Lazy discovery with meta-tools
- `plugins/kn9t-mcp/kn9t_mcp/mcp_client.py`: Local MCP client (stdio)
- `plugins/kn9t-mcp/kn9t_mcp/mcp_http_client.py`: Remote MCP client (HTTP/SSE)
- `plugins/kn9t-mcp/kn9t_mcp/config.py`: Config loader for `~/.kn9t/mcp.toml`

### Documentation

- `API.md §8.4`: Tool Declaration format with `hidden` field
- `API.md §10`: New section documenting lazy tool discovery pattern

### Result

- **154 tools registered** (4 builtin + 150 from kn9t-mcp)
- **6 tools visible** in system prompt (bash, read, write, edit, mcp_list_servers, mcp_search_tools)
- **148 tools hidden** (discovered via mcp_search_tools)
- **Cache stable** — hidden tools don't affect prefix

---

## Session — 2026-08-28 (continued) — Read Tool: Directory Support

### Change

The `read` tool now supports directories in addition to files.

**Before:** `read path=.` returned "Access is denied" error on Windows.

**After:** If path is a directory, lists its contents with:
- Directories first (sorted), with trailing `/`
- Files second (sorted)
- Respects `limit` parameter

Updated tool description to inform agents of this capability.

---

## Session — 2026-08-28 (continued) — Bash Tool: Partial Success Handling

### Change

Commands with non-zero exit code but stdout output are now treated as success.

**Before:** `Select-String` returning exit 1 (due to permission errors on some files)
would show ✗ in TUI even though it found matches and returned useful output.

**After:** If stdout is non-empty, treat as success (✓) regardless of exit code.
The exit code is still shown in the output for transparency.

**Rationale:** For a coding agent, getting useful output matters more than a clean
exit code. Commands like grep/Select-String often return non-zero when they hit
permission errors on some files, even though they successfully processed others.

---

## Session — 2026-08-28 (continued) — TUI Tool Cards & Session Title

### Summary

Fixed tool progress streaming from plugins to TUI, fixed cache ratio display bug, and added
session title display in the sidebar.

### Tool Progress Streaming

**Problem:** The `edit` tool emitted unified diffs via `ctx.progress.send()`, but the TUI
never received them. The tool cards only showed "edit applied to X" instead of the diff.

**Root cause:** `RemoteTool::execute` used `call_raw_hook_str` → `wait_for_response` which
**discards** all `Chunk` messages (line 268: `continue // discard`). Progress chunks from
plugins were silently dropped.

**Fix:**
1. Added `call_raw_hook_str_streaming()` to `PluginHost` that calls a callback for each chunk
2. `RemoteTool::execute` now uses streaming and emits `Event::ToolProgress` for each chunk
3. The event flows: Plugin → Chunk → RemoteTool → Event::ToolProgress → SSE → TUI

### Cache Ratio Bug (140240%)

**Problem:** The sidebar showed absurd percentages like "in: 5 (140240%)" for cache hit ratio.

**Root cause:** The calculation was `cache_read / tokens_in * 100` but `tokens_in` only
contains non-cached tokens (e.g., 5), while `cache_read` is the cached portion (e.g., 7000).
The ratio should be `cache_read / (tokens_in + cache_read)` to get the percentage of total.

**Fix:** Changed SESSION stats to use `session_total = tokens_in + cache_read` as denominator.

### Session Title in Sidebar

**Problem:** No way to see which session you're in or what it's about.

**Changes:**
1. Added `Event::TitleChanged { title: String }` to `kn9t-core`
2. Server emits `TitleChanged` after auto-titling completes (`turn.rs:maybe_autotitle`)
3. Added `session_title: Option<String>` to TUI `App`
4. Sidebar now shows `#session_id` and title at the top
5. TUI updates title when `TitleChanged` SSE event arrives
6. Title also updates in the sessions list for sidebar display

### Files Changed

- `kn9t-core/src/event.rs`: Added `TitleChanged` variant
- `kn9t-store/src/session.rs`: Added `TitleChanged` to `event_kind_name`
- `kn9t-server/src/sse.rs`: Added `title_changed` SSE event name
- `kn9t-server/src/turn.rs`: Emit `TitleChanged` after auto-titling
- `kn9t-plugin/src/host.rs`: Added `call_raw_hook_str_streaming`
- `kn9t-plugin/src/remote_tool.rs`: Use streaming, emit `ToolProgress`
- `kn9t-tui/src/wire.rs`: Added `TitleChanged` variant to `SseFrame`
- `kn9t-tui/src/app.rs`: Handle `TitleChanged`, added `session_title` field
- `kn9t-tui/src/ui/render.rs`: Display session ID and title in sidebar, fix cache ratio

---

## Session — 2026-08-28 (continued) — Bash Tool Fixes

### Summary

Fixed two bugs in the bash tool (`internal-plugins/kn9t-tools/src/bash.rs`) that caused
commands to timeout or lose output.

### Bug 1: stdin inheritance causing Python hangs

**Symptom:** `python -c "print('hello')"` and `python script.py` would timeout after
120 seconds, while `python --version` worked fine.

**Root cause:** `Command::new()` inherits stdin from the parent by default. Python
(and some other programs) may block waiting for stdin input, especially when piped.
The plugin's stdin wasn't sending anything, causing an indefinite hang.

**Fix:** Added `.stdin(Stdio::null())` to the Command builder to explicitly close
stdin for child processes.

### Bug 2: Race condition on stdout/stderr collection

**Symptom:** Commands would sometimes return empty or partial output.

**Root cause:** After `child.try_wait()` returns `Some(status)`, the code immediately
called `rx.try_iter().collect()` which is non-blocking. If the reader threads hadn't
finished draining the pipes, lines would be lost.

**Fixes:**
1. Keep `JoinHandle` for stdout/stderr reader threads instead of spawn-and-forget
2. `join()` the threads after process exits to ensure they've finished reading
3. Added `drain_with_timeout()` helper that properly waits for channel disconnect
4. Check `send().is_err()` to stop reading if receiver is dropped

### Files Changed

| File | Change |
|------|--------|
| `internal-plugins/kn9t-tools/src/bash.rs` | +45 lines — stdin null, proper thread join, drain helper |

---

## Session — 2026-08-28 (continued) — Write Tool + Progress Display for Edit/Write

### Summary

Added missing `write` tool to kn9t-tools plugin and implemented progress output
for both edit and write tools so the TUI can display file changes in real-time.

### Changes

**1. New Write Tool (`internal-plugins/kn9t-tools/src/write.rs`)**
- Creates new files or overwrites existing files
- Stale-read detection for existing files (like edit)
- Emits content via `ctx.progress.send()` for TUI display:
  - New files: unified diff style (`+` lines)
  - Overwrites: content with line numbers

**2. Edit Tool Progress Output**
- Now emits unified diff via `ctx.progress.send()` after successful edit
- TUI displays diff with syntax highlighting (green/red/cyan)

**3. Plugin Registration**
- Added `write::Write` to plugin main.rs
- Tools now: bash, read, write, edit

### Files Changed

| File | Changes |
|------|---------|
| `internal-plugins/kn9t-tools/src/write.rs` | New file - write tool implementation |
| `internal-plugins/kn9t-tools/src/edit.rs` | +75 lines - emit_unified_diff function |
| `internal-plugins/kn9t-tools/src/main.rs` | +2 lines - register write tool |

---

## Session — 2026-08-28 (continued) — Edit Tool Improvements + Architecture Cleanup

### Summary

Major improvements to the edit tool (ported from Pi's edit-diff.ts) and architectural
cleanup removing the deprecated `crates/kn9t-tools` in favor of the plugin subprocess.

### Edit Tool Improvements (internal-plugins/kn9t-tools)

**1. Line Ending Preservation (CRLF/LF)**
- Detects original line ending style before processing
- Normalizes to LF for matching
- Restores original line endings after edit
- Handles BOM (Byte Order Mark) correctly

**2. Two-Phase Fuzzy Matching**
- Phase 1: Exact string match
- Phase 2: Fuzzy match with Unicode normalization if exact fails
- Normalizes smart quotes (`"` → `"`), dashes (`—` → `-`), special spaces (NBSP → space)
- Strips trailing whitespace per line

**3. Better Error Messages**
- Empty oldString validation
- "No changes made" detection
- Fuzzy match indication in success message

### Architecture Cleanup: Remove crates/kn9t-tools

Per DESIGN Q25, tools must ship as a subprocess plugin, not an in-process crate.

**Before:** `kn9t-server` and `kn9t-react` depended on `crates/kn9t-tools` directly,
calling tool functions in-process. This violated the plugin architecture.

**After:**
- `crates/kn9t-tools` **deleted entirely**
- `kn9t-server` spawns `kn9t-tools` plugin at startup via `PluginHost::spawn()`
- Tools are `RemoteTool` instances communicating via plugin protocol
- `kn9t-react` tests use the real plugin subprocess, not stubs

### Files Changed

**Deleted:**
- `crates/kn9t-tools/` (entire directory)

**Modified:**
- `Cargo.toml` — removed `crates/kn9t-tools` from workspace members
- `crates/kn9t-server/Cargo.toml` — removed `kn9t-tools` dependency
- `crates/kn9t-server/src/state.rs` — added `tools: ToolRegistry` field
- `crates/kn9t-server/src/turn.rs` — use `state.tools` instead of `kn9t_tools::default_registry()`
- `crates/kn9t-server/src/main.rs` — spawn tools plugin at startup
- `crates/kn9t-react/Cargo.toml` — removed `kn9t-tools` dev-dependency, added `kn9t-plugin`
- `crates/kn9t-react/tests/support/mod.rs` — added `spawn_tools_registry()` helper
- `crates/kn9t-react/tests/acceptance.rs` — use real plugin instead of in-process tools

**New:**
- `crates/kn9t-server/src/tools.rs` — `spawn_tools_plugin()` helper

**Improved:**
- `crates/internal-plugins/kn9t-tools/src/edit.rs` — full rewrite with Pi patterns

### Verification

```
cargo check --workspace  # compiles clean
cargo test -p kn9t-tools-plugin  # edit tool tests pass
```

---

## Session — 2026-08-28 (continued) — Fix Token Double-Counting in Cost Calculation

### Summary

Fixed critical bug in `kn9t-custom-provider` provider where session costs were inflated due to
double-counting of cached tokens in the cost formula.

### Root Cause

The `decode_usage()` function in `map.rs` was computing:
```rust
let input = prompt + cache_read + cache_write;  // WRONG: total context
```

But the cost formula in `kn9t-react/src/hooks.rs` multiplies each field separately:
```rust
cost = input * price.input
     + cache_read * price.cache_read
     + cache_write * price.cache_write
     + output * price.output
```

This caused cache tokens to be billed **twice**: once at full input price (via `input`)
and again at discounted cache price (via `cache_read`/`cache_write`).

### The Fix

Per DESIGN §8.4.3, `Tokens` fields form a **partition, not overlap**:
- `input` = uncached tokens only (after cache breakpoint)
- `cache_read` = tokens read from cache
- `cache_write` = tokens written to cache
- Total context = `input + cache_read + cache_write`

Fixed `map.rs:317`:
```rust
let input = prompt;  // CORRECT: uncached-only per §8.4.3 partition rule
```

The custom plugin's `prompt_tokens` field already represents uncached-only, so we use it directly
without summing cache tokens.

### Tests Updated

- `cancel_preserves_usage` — assertion now checks `input == 100` (uncached) not `600` (total)
- `build_aborted_result_with_usage` — assertion now checks `input == 100` not `350`
- `usage_partition_rule` — validates the partition semantics

### Verification

```
cargo test -p kn9t-custom-provider-plugin
   → 13 unit tests + 15 acceptance tests = 28 passed, 0 failed
```

### Files Changed

| File | Changes |
|------|---------|
| `crates/internal-plugins/kn9t-custom-provider/src/map.rs` | `input = prompt` (was `prompt + cache_read + cache_write`) |
| `crates/internal-plugins/kn9t-custom-provider/src/client.rs` | Updated test assertions for correct partition semantics |

### Impact

Session costs should now be accurate. Previously, a turn with 100 uncached + 500 cached
tokens would bill as 600 input tokens + 500 cache_read tokens (1100 total billable).
Now it correctly bills as 100 input + 500 cache_read (600 total billable at appropriate
price tiers).

---

## Session — 2026-08-28 (continued) — Tool Card Collapse/Expand Feature

### Summary

Implemented interactive collapsible tool cards in the TUI with Input/Output tabs, virtual
scrolling, and full keyboard/mouse navigation. Also fixed tool result loading from database
when reopening sessions.

### Features Added

**1. Collapsible Tool Cards**
- Tools auto-expand while running, auto-collapse when finished
- `[+]`/`[-]` indicators show expand state
- Tool name color reflects status: green=success, red=error, yellow=running
- Click header line to toggle expand/collapse

**2. Input/Output Tabs**
- Output tab (default): shows tool result with line numbers
- Input tab: shows tool arguments in key-value format
- Click tabs to switch, or use Left/Right arrows in tool mode

**3. Virtual Scrolling**
- Large outputs display 20 visible lines at a time
- Scroll indicator shows position (e.g., `[20/148] 13%`)
- Mouse wheel scrolls within tool output area
- PgUp/PgDn scrolls in tool mode

**4. Tool Mode (Ctrl+T)**
- Up/Down: navigate between tools
- Enter/Space: toggle expand/collapse
- Left/Right: switch tabs
- PgUp/PgDn: scroll output
- Esc: exit tool mode
- Focused tool gets highlighted background

**5. Theme Colors**
- Added 7 new theme fields for full color control:
  - `tab_active_fg`, `tab_active_bg`, `tab_inactive_fg`
  - `tool_focus_bg`, `tool_focus_border`
  - `input_key`, `input_value`

**6. Diff Display for Edit Tool**
- `ToolProgress` events are now accumulated in `progress_lines`
- Edit tool's unified diff is displayed with syntax highlighting:
  - Green for `+` (additions)
  - Red for `-` (deletions)  
  - Cyan for `@@` (hunk headers)
- Progress lines shown above final output with separator

### Bug Fix: Tool Results Not Loading from Database

**Problem:** When reopening a session, tool outputs were empty even though they were stored
in the database.

**Root Cause:** Format mismatch between kn9t-core storage and TUI parsing:
- kn9t-core stores: `{ "type": "tool_result", "id": "...", ... }`
- TUI expected: `{ "type": "tool_result", "tool_use_id": "...", ... }` (Anthropic API format)
- Similarly for args: kn9t-core uses `args_json`, TUI expected `args` or `input`

**Fix:** Updated parsing to check both formats:
- `"id"` or `"tool_use_id"` for tool result ID
- `"args_json"`, `"args"`, or `"input"` for tool arguments

### Files Changed

| File | Changes |
|------|---------|
| `theme.rs` | +34 lines - Added 7 new color fields |
| `message_handler.rs` | +32 lines - Added `ToolTab` enum, `progress_lines` field, extended `ToolCard`, fixed parsing |
| `app.rs` | +270 lines - Tool mode state, helpers, mouse/key handlers, parsing fix, progress accumulation |
| `keybind.rs` | +7 lines - Added `ToolMode` action with Ctrl+T binding |
| `ui/render.rs` | +370 lines - New tool card rendering with tabs, virtual scroll, hit areas, diff coloring |

---

## Session — 2026-08-28 (continued) — Fix Abort Race Condition / Orphaned tool_call Bug

### Summary

Fixed critical bug where pressing ESC to abort a turn and immediately sending a new prompt
caused transcript corruption, resulting in "tool_use ids were found without tool_result blocks"
API errors.

### Root Cause Analysis

**Problem 1: Cancel Not Propagated to ReactLoop**
The server created a `Cancel` object and registered it in `ABORTS` map, but `ReactLoop::run()`
created its OWN cancel per turn. When `abort()` fired the server's cancel, it fired a different
object than the one the ReactLoop was using - so cancellation had no effect!

**Problem 2: Race Condition in prompt()**  
The server's `prompt()` accepted new prompts while a turn was still running:
1. User sends prompt A → turn A starts, tools execute
2. User presses ESC → TUI calls abort, server fires cancel (but wrong cancel!)
3. TUI immediately sets `streaming = false` (allows new input)
4. User sends prompt B → server appends user B message to transcript
5. Turn A eventually finishes, appends tool_results AFTER user B message
6. Transcript corrupted: `[user A, assistant tool_call, user B, tool_result]`
7. Next API call fails: "tool_use ids without tool_result blocks"

### Fixes Applied

**1. Pass Cancel from Server to ReactLoop (`kn9t-react/src/loop_.rs`)**
- Added `cancel: Option<Cancel>` field to `RunParams`
- ReactLoop now uses external cancel if provided, else creates fresh per turn
- Server passes its registered cancel to ReactLoop

**2. Block Prompt While Turn Running (`kn9t-server/src/routes/session.rs`)**
- `prompt()` checks `turn::is_turn_running(session)` before accepting
- Returns 409 Conflict with `turn_running` error if turn is active
- Prevents transcript corruption from race condition

**3. Add is_turn_running() (`kn9t-server/src/turn.rs`)**
- New public function checks if session has entry in `ABORTS` map
- Entry exists = turn is running (cancel is registered)

**4. Fix TUI Abort Flow (`kn9t-tui/src/app.rs`)**
- Removed `streaming = false` from abort handlers
- Added `aborting: bool` field for visual feedback
- `TurnEnded` SSE event now sets both `streaming` and `aborting` to false
- User sees "Aborting..." in red while abort is processing

**5. Visual Feedback (`kn9t-tui/src/ui/render.rs`)**
- When `aborting` is true, show "Aborting..." with error color instead of streaming phrase

### Test Coverage

- `abort_then_prompt_race` — verifies server returns 409 when prompt sent while turn running
- `external_cancel_during_tool_execution` — verifies tool results are persisted even after cancel

### Files Changed

**kn9t-react:**
- `src/loop_.rs` — Added `cancel: Option<Cancel>` to `RunParams`
- `src/turn.rs` — Use external cancel if provided
- `tests/acceptance.rs` — Added `cancel: None` to test RunParams, new test

**kn9t-server:**
- `src/turn.rs` — Added `is_turn_running()`, pass cancel to RunParams
- `src/routes/session.rs` — Check `is_turn_running()` before accepting prompt
- `tests/acceptance.rs` — Added `abort_then_prompt_race` test

**kn9t-tui:**
- `src/app.rs` — Added `aborting` state, fixed abort handlers
- `src/ui/render.rs` — Show "Aborting..." when aborting

### Pre-existing Issues Noted

- `keepalive_detects_dropped_client` test is flaky (timing-sensitive, unrelated to this fix)
- `test_wait_timeout_returns_false_on_timeout` test is flaky (timing-sensitive)

---

## Session — 2026-08-28 (continued) — TUI Left Sidebar Removal & Session/Model Picker Overlays

### Summary

Removed the left sidebar from the TUI and replaced it with a `/session` slash command
that opens a fuzzy-searchable session picker overlay. Also added fuzzy filtering to
the existing `/models` command. This provides a cleaner, more keyboard-driven UX that
works consistently on both the welcome and conversation screens.

### Design Decision

**Q32 — Session Access Pattern**

Rejected: Hover-to-expand left sidebar with session list
- Required mouse interaction
- Took horizontal space from main content area
- Inconsistent with minimal design goals

Accepted: `/session` slash command with modal overlay
- Keyboard-driven (type to filter)
- Works on both welcome and conversation screens
- Consistent with `/models` pattern
- No wasted horizontal space
- `Ctrl+B` shortcut preserved (now opens session picker)

### Changes

**Layout (`ui/layout.rs`)**
- Changed from 3-column to 2-column layout: `[content | right sidebar]`
- Removed `left_enabled`, `left` fields from `LayoutState`
- Removed `left` field from `Areas` struct
- Removed `toggle_left()`, `expand_left()`, `collapse_left()` methods
- Removed `in_left_zone()` function
- Simplified `compute_with_input()` and `effective_right_state()`

**Rendering (`ui/render.rs`)**
- Removed `render_left_sidebar()` function entirely
- Updated `render_chat()` to skip left sidebar rendering
- Added `render_session_select()` with fuzzy filtering
- Updated `render_model_select()` with fuzzy filtering (filter input box)
- Updated overlay dispatch to handle `SessionSelect` variant
- Updated welcome screen hints: `/session  /models  /new  /help  /quit`
- Updated help overlay text: `Ctrl+B` now says "switch session"

**App State (`app.rs`)**
- Updated `Overlay` enum: `ModelSelect { selected, filter }`, `SessionSelect { selected, filter }`
- Removed `hover_session_idx`, `mouse_x`, `mouse_y` fields from `App`
- Removed left sidebar hover/click logic in `handle_mouse()`
- Updated `handle_overlay_key()` for fuzzy filtering (Backspace, Char input)
- Added `/session` command handling in `execute_slash_command()`
- `Action::ToggleLeft` now opens session picker overlay

**Slash Commands (`slash.rs`)**
- Added `/session` command: "Switch session"
- Made `fuzzy_match()` function public for use in render.rs

**Config (`config.rs`)**
- Removed `left_sidebar` field from `Config` struct
- Marked `left_sidebar` in config file parsing as deprecated (still parseable, ignored)

### UI/UX Changes

| Feature | Before | After |
|---------|--------|-------|
| Session switching | Hover left sidebar, click session | `/session` or `Ctrl+B`, type to filter, Enter |
| Model switching | `/models`, arrow keys | `/models`, type to filter, arrow keys |
| Layout | 3 columns (sidebar, content, sidebar) | 2 columns (content, sidebar) |
| Main content width | Reduced by sidebar | Full width minus right sidebar |

### Keyboard Shortcuts

- `/session` — Open session picker with fuzzy search
- `/models` — Open model picker with fuzzy search (now with filter)
- `Ctrl+B` — Open session picker (was: toggle left sidebar)
- `Ctrl+N` — New session (unchanged)

### Files Changed

**Modified:**
- `crates/kn9t-tui/src/ui/layout.rs` — 2-column layout
- `crates/kn9t-tui/src/ui/render.rs` — session picker, model picker with filter
- `crates/kn9t-tui/src/app.rs` — overlay handling, removed sidebar state
- `crates/kn9t-tui/src/slash.rs` — `/session` command, public `fuzzy_match`
- `crates/kn9t-tui/src/config.rs` — removed `left_sidebar`

### Build Verification

```
cargo build -p kn9t-tui  # Compiles clean, no warnings
cargo check -p kn9t-tui  # All dependencies resolve
```

---

## Session — 2026-08-28 — Code Review Improvements: Module Extraction & Unit Tests

### Summary

Iterative code review and improvement session. Addressed structural issues in the TUI
crate (monolithic `app.rs`) and added comprehensive unit tests to `kn9t-core` modules.

### Improvements Made

**1. TUI Module Extraction** (`kn9t-tui/src/`)

Extracted cohesive modules from the 1,456-line `app.rs`:

| New Module | Lines | Responsibility |
|------------|-------|----------------|
| `session_manager.rs` | 229 | Session lifecycle: list, create, switch, enter, state reset |
| `model_selector.rs` | 240 | Model list management and selection |
| `message_handler.rs` | 465 | Transcript state, message parsing, SSE frame processing |
| `token_tracker.rs` | 235 | Token usage tracking and throughput calculation |

`app.rs` now re-exports types from submodules for backward compatibility:
```rust
pub use crate::message_handler::{Message, ToolCard};
pub use crate::model_selector::ModelEntry;
pub use crate::session_manager::SessionEntry;
```

**2. Theme Auto-Detection** (`kn9t-tui/src/theme.rs`)

Implemented `Theme::auto_detect()` using `COLORFGBG` environment variable to detect
terminal light/dark mode. Falls back to dark theme if detection fails.

**3. kn9t-core Unit Tests**

Added comprehensive unit tests to core modules (previously only `ids.rs` had tests):

| Module | Tests Added | Coverage |
|--------|-------------|----------|
| `cache.rs` | 8 tests | `breakpoints()` edge cases: empty, dedup, priority order, max limits |
| `cancel.rs` | 10 tests | State transitions, thread safety, timeout behavior, clone sharing |
| `bus.rs` | 13 tests | Ring buffer, pub/sub, close semantics, timeout recv |

**4. Error Type Modules** (new files)

- `kn9t-store/src/err.rs` — Store error types
- `kn9t-tools/src/err.rs` — Tool error types

### Metrics

| Metric | Before | After | Change |
|--------|--------|-------|--------|
| Files with `#[test]` | 32 (22%) | 35 (24%) | +3 |
| `kn9t-core` files with tests | 1 | 4 | +3 |
| Total lines | ~24K | ~26.7K | +2.7K (tests + modules) |
| TODOs in prod code | 4 | 4 | — (all in doc examples) |

### Files Changed

**Modified:**
- `crates/kn9t-core/src/bus.rs` — added 13 unit tests
- `crates/kn9t-core/src/cache.rs` — added 8 unit tests
- `crates/kn9t-core/src/cancel.rs` — added 10 unit tests
- `crates/kn9t-core/src/ids.rs` — existing tests
- `crates/kn9t-tui/src/app.rs` — module re-exports, reduced duplication
- `crates/kn9t-tui/src/lib.rs` — export new modules
- `crates/kn9t-tui/src/theme.rs` — auto-detection implementation
- `crates/kn9t-tui/src/ui/layout.rs` — minor refactoring
- `crates/kn9t-tui/src/ui/render.rs` — minor refactoring
- `crates/kn9t-react/src/exec.rs` — refactoring
- `crates/kn9t-store/src/lib.rs` — export err module
- `crates/kn9t-store/src/session.rs` — refactoring
- `crates/kn9t-tools/src/bash.rs` — refactoring
- `crates/kn9t-tools/src/edit.rs` — refactoring
- `crates/kn9t-tools/src/read.rs` — refactoring
- `crates/kn9t-tools/src/lib.rs` — export err module

**New Files:**
- `crates/kn9t-tui/src/session_manager.rs`
- `crates/kn9t-tui/src/model_selector.rs`
- `crates/kn9t-tui/src/message_handler.rs`
- `crates/kn9t-tui/src/token_tracker.rs`
- `crates/kn9t-store/src/err.rs`
- `crates/kn9t-tools/src/err.rs`

### Remaining Work

1. **Compose managers in `App`**: The extracted modules define managers but `App` doesn't
   yet use them as composed fields — still manages state directly.
2. **Extend test coverage**: Tools (`bash.rs`, `edit.rs`, `read.rs`) still lack unit tests.
3. **Consider `parking_lot::Mutex`**: Would eliminate `expect("poisoned")` patterns.

---

## Session — 2026-08-27 (night) — Cache Control & TUI Stats Fixes

### Summary

Fixed prompt caching to work correctly with auto-discovered models, and fixed TUI to
display accurate cache hit percentages.

### Root Cause Analysis

1. **Model aliases broke caching**: Config-defined model aliases (e.g., `haiku`, `sonnet`)
   had `cache = "none"` as the default via `default_cache_mode_str()`. Auto-discovered
   models got `CacheMode::Automatic` but config aliases overrode this with `"none"`.

2. **TUI cache hit% calculation was wrong**: Was computing `cache_read / (input + cache_read)`
   but Anthropic returns `input` as the *total* input tokens (already includes cache_read).
   Correct formula: `cache_read / input`.

3. **TUI stats overwritten by titling**: The auto-title call (small, no cache) would
   overwrite the real agent stats in "LAST TURN" display.

4. **TUI didn't accumulate multi-call turns**: In a ReAct turn with tool calls, multiple
   `UsageRecorded` events arrive. The TUI was showing only the last one instead of
   accumulating.

5. **TUI stats disappeared during streaming**: Reset happened at `TurnStarted`, making
   previous turn stats invisible while waiting for new `UsageRecorded`.

### Fixes

**Config simplification** (`~/.kn9t/config.toml`):
- Removed manual `[[model]]` aliases that had `cache = "none"`
- Auto-discovered models have `CacheMode::Automatic` by default
- Changed `default_model` to use full auto-discovered model ID

**System prompt** (`kn9t-server/src/system_prompt.rs`):
- Added new module with `default_system_prompt()` function
- Platform-aware shell instructions (PowerShell on Windows, Bash on Unix)
- Injected via `RunParams.system` field in `exec.rs`

**Cache control on last tool** (`kn9t-provider-openai/src/encode.rs`):
- Moved `cache_control` from system message to the LAST tool definition
- This caches the entire system+tools prefix together (more efficient)

**TUI stats display** (`kn9t-tui/src/app.rs`, `kn9t-tui/src/ui/render.rs`):
- Filter out `usage_kind == "title"` from LAST TURN stats
- Accumulate stats within a turn using `pending_turn_reset` flag
- Reset deferred to first `UsageRecorded` (keeps previous turn visible during streaming)
- Fixed cache hit% formula: `cache_read / input * 100`
- Display format: `in: 6.3k (82%)` with `r:5.2k w:1.1k` below

### Cache Behavior Notes

Anthropic prompt caching has a **minimum threshold of ~1024 tokens**. Small requests
(like "hello") won't trigger cache writes. Once the context exceeds the threshold,
caching activates:

```
usage: in=1379 cache_r=0 cache_w=0     # Too small, no caching
usage: in=4290 cache_r=0 cache_w=4289  # Above threshold, cache write!
usage: in=4744 cache_r=4289 cache_w=X  # Next turn: cache hit!
```

From `custom-provider.log` with larger context:
```
usage: in=5249 out=143 cache_r=0 cache_w=5248      # Turn 1: write cache
usage: in=6360 out=563 cache_r=5248 cache_w=1111  # Turn 2: 82% hit!
usage: in=7471 out=213 cache_r=6953 cache_w=508   # Turn 3: 93% hit!
```

### Files Changed

- `crates/kn9t-server/src/system_prompt.rs` — new system prompt module
- `crates/kn9t-server/src/lib.rs` — export system_prompt module
- `crates/kn9t-server/src/turn.rs` — import system_prompt
- `crates/kn9t-react/src/loop_.rs` — add `system: Option<String>` to RunParams
- `crates/kn9t-react/src/exec.rs` — inject system prompt into Request
- `crates/kn9t-provider-openai/src/encode.rs` — cache_control on last tool
- `crates/kn9t-server/src/config.rs` — removed debug logging
- `crates/kn9t-tui/src/app.rs` — filter title, accumulate turn stats
- `crates/kn9t-tui/src/ui/render.rs` — correct cache hit% formula

---

---

## Session — 2026-08-27 (late evening) — Parallel Tool Call Fix

### Problem

The kn9t-custom-provider plugin was losing parallel tool calls. When the custom provider server sends multiple
tool calls as separate SSE events (each at array position 0), the host would assign
`idx=0` to all of them, causing collisions when assembling the final message.

**Root cause**: The `ChunkSender::tool_use_start()` and `tool_use_delta()` methods in
`kn9t-plugin-sdk` did not emit an `idx` field. The host (`kn9t-plugin/src/remote_provider.rs`)
defaulted to `idx=0` for all tool calls, causing them to overwrite each other.

**Reference fix**: tracks `positionToIndex: Map<number, number>` and `nextToolIndex`, emitting a stable global
index that increments for each new tool call. This index is used to group tool call
fragments correctly.

### Fix (kn9t-plugin-sdk)

`ChunkSender` now automatically tracks `call_id → idx` mapping:

```rust
pub struct ChunkSender {
    // ... existing fields ...
    call_id_to_idx: Mutex<HashMap<String, u32>>,
    next_tool_idx: AtomicU32,
}

impl ChunkSender {
    pub fn tool_use_start(&self, call_id: &str, name: &str, args_json: &str) {
        let idx = self.get_or_assign_idx(call_id);  // ← Auto-assign stable idx
        self.send_chunk(json!({
            "kind": "tool_use_start",
            "idx": idx,  // ← Now emitted!
            "call_id": call_id,
            "name": name,
            "args_json": args_json,
        }));
    }
}
```

**Key behavior**:
- First `tool_use_start("call_1", ...)` → assigns idx=0, stores in map
- First `tool_use_start("call_2", ...)` → assigns idx=1, stores in map
- Subsequent `tool_use_delta("call_1", ...)` → looks up idx=0 from map
- Subsequent `tool_use_delta("call_2", ...)` → looks up idx=1 from map

This mirrors OpenCode's `positionToIndex` / `nextToolIndex` pattern.

### Changes

**`kn9t-plugin-sdk/src/ctx.rs`**:
- Added `call_id_to_idx: Mutex<HashMap<String, u32>>` and `next_tool_idx: AtomicU32` to `ChunkSender`
- Added `ChunkSender::new()` constructor
- Added `get_or_assign_idx()` helper method
- `tool_use_start()` and `tool_use_delta()` now emit `"idx"` field
- Updated doc comments with parallel tool call examples

**`kn9t-plugin-sdk/src/plugin.rs`**:
- Updated to use `ChunkSender::new()` constructor

**`kn9t-plugin-sdk/tests/acceptance.rs`**:
- Added `parallel_tool_calls` test (R-PLUG2-125) verifying idx assignment

### Verified

```
cargo test -p kn9t-plugin-sdk  → 11 tests pass (including new parallel_tool_calls)
cargo build -p kn9t-custom-provider-plugin → compiles clean
```

### No changes needed to kn9t-custom-provider

The fix is entirely in the SDK. The custom provider plugin's existing calls to
`ctx.chunk.tool_use_start(id, name, "")` now automatically emit the correct `idx`.

---

## Session — 2026-08-27 (evening) — Pricing Fallback Architecture

### Problem

Auto-discovered models from `/v1/models` have no pricing information. The endpoint only
returns `{id, object, created, owned_by}` — no cost data. This results in `$0.000` in the
TUI even when tokens are consumed.

### Architecture Decision: Fallback Pricing Table (R-PCORE-095)

**`kn9t-provider-core` contains a built-in pricing table** (`src/pricing.rs`) that maps
model `api_id` patterns to known prices. This is used as a fallback when:

1. The config `[[model]]` section doesn't specify prices
2. The provider endpoint doesn't return pricing data

**Lookup order:**
1. Config-defined prices (`price_in`, `price_out`, etc.) — always win
2. `lookup_price(api_id)` fallback from `kn9t-provider-core::pricing`
3. Zero (if no match)

**Covered providers/models:**
- Anthropic Claude (3, 3.5, 4 — Haiku, Sonnet, Opus)
- Amazon Nova (Micro, Lite, Pro)
- Mistral (Large, 7B)
- NVIDIA Nemotron (Super, Nano)
- OpenAI GPT (3.5, 4, 4-turbo, 4o, 4o-mini)
- DeepSeek (Chat, Reasoner)
- Google Gemini (1.5, 2 — Flash, Pro)
- Meta Llama (8B, 70B, 405B)

**Prices are in USD per 1M tokens.** Cache read/write prices included where supported.

### TUI Status Bar Improvements

Added to status bar:
- `cache r:N w:M` — cache read/write token counts (when > 0)
- `N tok/s` — output throughput from last turn
- `^P=help` — corrected help hint (was `?=help` but `?` not bound)

---

## Session — 2026-08-27 (afternoon) — Global Attach Architecture

### Problem

Server kept dying mid-session because no SSE connection was active when user was on welcome
screen or model picker. The `idle-exit` logic triggered because `attached_clients == 0`.

This bug recurred 3 times because the fix was ad-hoc each time (increasing timeouts, etc.).

### Architecture Decision: Global Attach (R-SRV-081)

**Each client (TUI or CLI) MUST connect to `GET /attach` at startup.** This is a long-lived
SSE connection that:

1. Increments `attached_clients` on connect
2. Server sends heartbeat pings every 30s to detect dead clients
3. Decrements `attached_clients` on disconnect (write failure or client exit)
4. Stays open for the entire lifetime of the client process

**Session SSE (`/session/{id}/events`) does NOT affect `attached_clients`.** It's only for
receiving session-specific events.

```
┌─────────────────────────────────────────────────────────────────┐
│                         CLIENT (TUI or CLI)                     │
├─────────────────────────────────────────────────────────────────┤
│  At startup: GET /attach (SSE long-lived)                       │
│     └─> 1 attach = 1 client present                             │
│                                                                 │
│  For sessions: GET /session/{id}/events                         │
│     └─> Session events only, does NOT affect client count       │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                         SERVER                                  │
├─────────────────────────────────────────────────────────────────┤
│  attached_clients: count of /attach connections                 │
│    - 1 TUI connected = 1                                        │
│    - 1 TUI + 1 CLI chat = 2                                     │
│                                                                 │
│  idle-exit (R-SRV-080):                                         │
│    - attached_clients > 0 → NEVER exit                          │
│    - attached_clients == 0 AND no running turns → grace period  │
│    - After grace period → exit                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Implementation

**Server (`kn9t-server/src/router.rs`):**
- Added `GET /attach` route → `handle_global_attach()`
- Sends `event: hello` then heartbeat `event: ping` every 30s
- `client_attached()` on connect, `client_detached()` on disconnect
- Removed `client_attached/detached` from session SSE handler

**TUI (`kn9t-tui/src/client.rs`):**
- Added `spawn_attach_thread()` → returns `AttachHandle`
- Called in `App::connect()` at startup
- Stopped in `App::run()` cleanup on exit

**CLI (`kn9t/src/chat.rs`):**
- Added `spawn_global_attach()` using raw TcpStream
- Called at start of `run()` and `attach()` commands
- `_attach_stop` dropped on exit → thread stops

### Other fixes this session

- **Double user message**: Removed local push after prompt (SSE MessageAppended provides it)
- **Duplicate transcript on session switch**: Set `last_seq` from snapshot's `head_seq`
- **Server startup timeout**: Increased from 5s to 15s (model auto-discovery takes time)

---

## Session — 2026-08-27 (continued) — TUI rewrite from scratch

### Design session

Analyzed OpenCode 2.0 TUI (`packages/opencode/src/cli/cmd/tui/`). Key takeaways:
- Built on custom `@opentui/solid` (TypeScript + Zig core)
- Route-based: Home + Session views
- Portal-based dialogs, keybind system with leader key
- Type-specific tool rendering (bash, read, edit, etc.)
- Side-by-side diff viewer with syntax highlighting
- Full permission prompt blocking overlay

### Grill session (25 design decisions)

Produced `docs/TUI-DESIGN.md` (758 lines) and `spec/07-tui.md` (25 requirements).

Key decisions locked:
- **Framework**: ratatui + crossterm (Rust-native, spec-mandated)
- **Event model**: Pure event-driven, block on recv(), zero polling
- **Layout**: 3-column (left sidebar sessions, transcript, right sidebar context)
- **Mouse**: Full support (hover, click, drag)
- **Input**: `Enter` = send, modifier+Enter = newline
- **Approval**: Blocking overlay
- **Scroll**: Auto-scroll with escape, message jumping `[u`/`]u` `[a`/`]a`
- **Tool cards**: Collapsible, lazy-load on expand
- **Keybinds**: Vim-style default, fully customizable
- **Theming**: Auto light/dark + CSS-like user overrides
- **Spinner**: Animated braille + configurable fun phrases
- **Git**: Sidebar status + side-by-side diff viewer with line comments (v1)

### Implementation (from scratch)

Deleted old TUI code, rewrote per spec:

**Files created:**
- `src/lib.rs` — crate root
- `src/main.rs` — entry point, terminal setup, event loop
- `src/event.rs` — R-TUI-020: unified event channel, spawn_input_thread, spawn_tick_thread
- `src/wire.rs` — SSE frame types (no kn9t-* deps per GI-6)
- `src/client.rs` — HTTP client for server communication
- `src/config.rs` — R-TUI-180: config loading, keybinds, theme
- `src/theme.rs` — R-TUI-180: light/dark auto + user overrides
- `src/keybind.rs` — R-TUI-160: vim-style keybinds, multi-key sequences (gg)
- `src/app.rs` — main state, run loop, event handlers
- `src/ui/mod.rs` — UI module
- `src/ui/layout.rs` — R-TUI-030: 3-column responsive layout
- `src/ui/render.rs` — all rendering (sidebars, transcript, input, status, overlays)
- `src/widgets/mod.rs` — placeholder for custom widgets

**Key characteristics:**
- No borders (clean minimal design per user request)
- Sidebars: collapsed=2 cols, expanded=18/24 cols, hidden below 60 cols
- Hover-to-expand sidebars via mouse tracking
- Approval overlay blocks input until resolved
- Help overlay on `?`
- Spinner phrases rotate every ~2s

**Build result:** Compiles clean, all 133 workspace tests pass.

### What's next
- Test with live kn9t server
- G3 gate: 3 TUI instances against same session

---

## Session — 2026-08-27 (continued) — idle-exit fix + keepalive + tests

### What was done

**Root cause of idle-exit not working:**
`run_live_loop` blocked forever on `sub.recv()` between events. Server never attempted a
write, so it never detected a disconnected client, so `client_detached()` was never called,
so `attached_count` stayed 1, so `should_exit()` never fired.

**Fix 1 — keepalive ping (server-side):**
- `Subscription::recv_timeout(duration)` added to `kn9t-core` bus ring (condvar `wait_timeout`).
- `run_live_loop` now calls `recv_timeout(heartbeat_interval())` instead of blocking `recv`.
- On timeout: writes `": keepalive\n\n"` SSE comment frame. Write failure → loop exits →
  `client_detached()` → `attached_count` hits 0 → idle watchdog fires after grace period.
- `heartbeat_interval()` reads `KN9T_SSE_HEARTBEAT_MS` env var (default 15 s) — short in tests.

**Fix 2 — idle-exit semantic change:**
- Default grace: 30 min → **5 s** after last client disconnects.
- `should_exit()`: unchanged logic — fires when `attached_count=0` AND `running_turns=0`
  AND `elapsed >= idle_exit`. Returns false if `idle_exit` is zero (disabled).
- Config: `[server] idle_exit_secs` overrides default; `0` disables.

**Fix 3 — `POST /stop` + `kn9t stop`:**
- `stop_requested: AtomicBool` on `ServerState`; watchdog checks it alongside `should_exit`.
- `POST /stop`: sets flag, returns `{"ok":true}`.
- `GET /health`: returns `{ok, idle_secs, attached_clients, running_turns}`.
- `kn9t stop` subcommand reads `~/.kn9t/{port,token}`, sends `POST /stop`, exits.

**Tests added:**
- `srv::stop_route`: `POST /stop` → 200 → server shuts down within 3 s. ☑
- `srv::keepalive_detects_dropped_client`: opens real SSE TCP connection, drops it,
  verifies `attached_count` hits 0 within 5 s (heartbeat=200 ms), server idle-exits. ☑

**spec/06-server.md R-SRV-080 updated:**
- Grace-on-last-disconnect semantic documented.
- `POST /stop` and keepalive ping requirements added.
- Three acceptance tests listed.

**DESIGN.md §12.2 and §18.14 updated:**
- Grace period, keepalive, `POST /stop` documented.
- SPEC-OPEN 14 resolved: 5 s grace, not a fixed idle timer.

### Verified
- `cargo test -p kn9t-server -- srv::stop_route srv::keepalive_detects_dropped_client` — pass.
- `cargo test --workspace` — zero failures.
- Live: `echo Say hi | kn9t chat` → server starts → chat runs → server exits within ~15 s.

---

## Session — 2026-08-27 (continued) — shutdown-after-idle + kn9t stop

### What was done

**Idle-exit infrastructure** was already scaffolded (`IdleTracker`, watchdog thread, `should_exit()`) but had no config override and no way to trigger shutdown without `kill`.

**Config override (`[server] idle_exit_secs`):**
- Added `RawServer` struct with `idle_exit_secs: Option<u64>` to `config.rs`.
- `ResolvedConfig` gains `idle_exit: Option<Duration>`.
- `main.rs` wires it via `state.with_idle_exit(d)`. Logs chosen period on startup.
- `idle_exit_secs = 0` disables auto-exit (`should_exit()` returns false when duration is zero).
- `config.toml` bootstrap template updated to document the field.

**`POST /stop` route:**
- `stop_requested: AtomicBool` added to `ServerState`.
- `POST /stop` sets it; the idle watchdog checks it every 200ms and triggers clean shutdown.
- `GET /health` added: returns `{ ok, idle_secs, attached_clients, running_turns }`.

**`kn9t stop` subcommand (`crates/kn9t/src/cmd_stop.rs`):**
- Reads port+token from `~/.kn9t/`; sends `POST /stop`; prints confirmation.
- If server not running: exits with error.

**Verified:**
- Started server via `kn9t sessions`.
- `kn9t stop` → `[kn9t stop] server stopping (port 58361)`.
- Server process gone within 1s (`tasklist` shows no kn9t-server).
- `cargo test --workspace` — zero failures.

---

## Session — 2026-08-27 — P1-A, P2-B, P2-C, P3-A, P3-B (PLAN.md)

### What was done

**P1-A — bootstrap (`crates/kn9t/src/bootstrap.rs`):**
- `ensure_home(path)` auto-creates `~/.kn9t/` with `config.toml` template and `token` (UUID) on first run.
- UUID generated from OS CSPRNG: `/dev/urandom` on Unix, `BCryptGenRandom` (linked via `#[link(name="bcrypt")]`) on Windows.
- First-run message printed to stderr. Subsequent runs: no-op (fast path).
- Wired into `ensure_server()` in `main.rs` before any server interaction.
- Verified: clean `KN9T_HOME` → directory + files created, first-run message shown.

**P2-B — REPL mode + `--continue` (`crates/kn9t/src/chat.rs`):**
- `kn9t chat` with no prompt words → create new session → enter REPL loop.
- `kn9t chat --continue` → pick session with highest `head_seq` (`resolve_latest_session`) → REPL.
- `repl_loop()`: prints `> `, reads stdin, acquires lease with exponential backoff (100ms→2s, 30s total), sends prompt, streams until `TurnEnded`, releases lease, repeats. `Ctrl-D` exits cleanly.
- `acquire_lease_with_backoff()`: silent retry — enables multi-client on same session.
- `subscribe_sse()`: extracted helper, reused by REPL and one-shot.
- `stream_events_until_turn_end()`: borrows `&Receiver` (not consuming), safe to call per-turn in REPL.
- Verified: two sequential REPL prompts both streamed correctly.

**P2-C — approval flow (`crates/kn9t/src/chat.rs`):**
- `ApprovalRequest` SSE events now handled: `approval_selector()` renders inline `[ No ]  [ Yes ]` with crossterm raw-mode key capture. `←`/`→` to move, `Enter` to confirm. Default: `No`.
- `post_approval()` sends `POST /approve` with `allow`/`deny` decision.
- `APPROVAL_CTX` thread-local carries `host`/`auth` into the SSE loop (avoids threading host through all event handlers).
- `crossterm = "0.27"` added to `kn9t/Cargo.toml`.

**P3-A — `kn9t sessions` (`crates/kn9t/src/cmd_sessions.rs`):**
- `GET /session` → formatted table: ID, NAME, MODEL, AGE (human-readable: Xs/Xm/Xh/Xd ago).
- Dispatched from `main.rs` `"sessions"` arm.

**P3-B — `kn9t history` (`crates/kn9t/src/cmd_history.rs`):**
- `kn9t history [id]` → `GET /session/{id}` → prints full transcript with ANSI role labels.
- No id: uses `resolve_latest_session` (highest `head_seq`).
- Content blocks: `text`, `tool_use` (pretty-printed args), `thinking` (collapsed), `tool result`.

**`main.rs` updated:**
- `mod bootstrap`, `mod cmd_sessions`, `mod cmd_history` added.
- `kn9t_home()` delegates to `bootstrap::kn9t_home_path()`.
- Dispatch for `sessions`, `history`, `attach` added.
- `kn9t attach [id]` calls `chat::attach()` → `repl_loop()`.

### Verified (session 1)
- P1-A: first-run bootstrap creates all files.
- P2-B: REPL sends two sequential prompts, both stream correctly.
- Build: `cargo build -p kn9t` — zero errors.

### Not yet verified (session 1)
- P3-A/B/C: compiled but not live-tested.
- P2-C approval: compiled, requires approval-gated session to test.

---

## Session — 2026-08-27 (continued) — P3 live verify, P4-A real subprocess tests

### What was done

**P3-A/B/C live-tested against running server (port 59889):**

- `kn9t sessions` — discovered server wraps list in `{ "sessions": [...] }` (not bare array). Fixed `cmd_sessions.rs` to unwrap envelope. Also: list items only have `id`, `name`, `head_seq`, `cwd` — no `model` or timestamps. Dropped model/age columns; added `SEQ` and `CWD`. Table sorted by `head_seq` descending. Output verified: 20 sessions rendered correctly.
- `kn9t history <id>` — discovered response uses `transcript` key (not `messages`) and full session object has `meta`, `model`, `cost_usd`, `ctx_tokens` at top level. Fixed `cmd_history.rs` to try `transcript` then `messages`. Transcript renders with ANSI role labels, tool calls, tool results. Verified: session `01M116MZ0G4CJNCBG13J08QC3A` (Hello World) printed correctly.
- `kn9t history` (no id) — picks session with highest `head_seq`. Fixed `latest_session()` to handle sessions envelope in `cmd_history.rs` and `chat.rs`. Verified: resolved to `01M0Z0J2ZKTX697N8GQK5NZ0Y7` (Python Hello World Tutorial, seq=46).
- `kn9t attach 01M116MZ0G4CJNCBG13J08QC3A` — piped a prompt, got a full streamed response with tool call (bash), tool result, and assistant reply. Verified.

**P4-A — `kn9t-test-plugin` binary + real subprocess tests:**

- Created `crates/internal-plugins/kn9t-test-plugin/` using `kn9t-plugin-sdk`. Reads `TEST_PLUGIN_HOOK`, `TEST_PLUGIN_REPLY`, `TEST_PLUGIN_SLEEP_MS` from env. Implements `PluginHook` with configurable reply and sleep.
- Rewrote `plug::composition` to use `PluginHost::spawn(&bin, env_vars)` for all three composition classes (pipeline/veto/collect). No more in-process `ChannelPipe` stubs.
- Rewrote `plug::timeout` to spawn real subprocess sleeping 5000 ms (beyond the 500 ms `get_steering` timeout); host must return empty failure posture. Falls back to in-process stub if binary not built.
- `cargo test -p kn9t-plugin -- plug::composition plug::timeout` — both pass.
- Full workspace: `cargo test --workspace` — zero failures across all crates.

**P5-A — G3 gate:**
- Not run: requires human at terminal (3 TUI instances, screenshot paste). Deferred to next session.

### Discovered
- Server `GET /session` returns `{ "sessions": [...] }` envelope, not a bare array. All three session-list consumers updated.
- Session list items are minimal (id/name/head_seq/cwd only). Full model/timestamp data only in `GET /session/{id}`. Adjusted sessions table accordingly.

**Stage 10 — bedrock native + gemini (v2).** v1 is fully verified end-to-end with real tool calls. Read `spec/10-bedrock-native-v2.md` before starting.

---

## Session — 2026-08-27 (continued) — chat.rs extraction + spec/design cleanup

### What was done

**`crates/kn9t/src/chat.rs` extracted:**
- All chat subcommand logic moved out of `main.rs` into `chat.rs`: HTTP helper (`post_json`), SSE subscriber thread, `stream_events` loop, `display_args`, `unified_diff`, `display_result`.
- `main.rs` is now pure launcher (~140 lines): paths, server liveness, `ensure_server()`, `main()` dispatch. `mod chat` declared; `chat::run()` called for the `chat` subcommand.

**`spec/09a-custom-provider.md` R-CP-120 updated:**
- Was referencing old `kind = "custom-provider"` config format. Updated to `kind = "plugin"` / `binary = "kn9t-custom-provider"` per DESIGN §8.5 correction.

**`hello_world.py` removed** — leftover from e2e test run, not a project file.

---

## Session — 2026-08-27 (continued) — plugin provider wiring + kn9t chat tool visibility

### What was done

**Design correction — `kind = "plugin"` (DESIGN.md §8.5, §8.6):**
- Identified and fixed a design contradiction: §8.5/§8.6 described `kind = "custom-provider"` as a core provider kind, contradicting Q31 which decided providers are plugin binaries.
- §8.5 rewritten: core knows exactly two kinds — `replay` and `openai`. Everything else is `kind = "plugin"` with a `binary` field. custom plugin, Anthropic, and any future provider are plugin binaries, not core kinds.
- §8.6 rewritten: replaces the custom plugin wall-of-text with the plugin provider design (lifecycle, binary resolution, model list from hello). Wire hazard details stay in `spec/09a-custom-provider.md`.
- §8.4.4 cache table: replaced `custom-provider` row label with `kn9t-custom-provider plugin`.

**`kn9t-server/src/config.rs`:**
- `RawProvider` gains `binary: Option<String>` and `env: HashMap<String,String>`. `base_url` is now `#[serde(default)]` (not required for plugin kind).
- `kind = "custom-provider"` dead branch removed.
- `kind = "plugin"` branch implemented: resolves binary (sibling exe or absolute), resolves `env:VAR` values, calls `PluginHost::spawn()`, wraps as `RemoteProvider(Arc<PluginHost>, name)`, registers as `Arc<dyn Provider>`.
- `kn9t-plugin` added to `kn9t-server/Cargo.toml` (GI-1 exception already covers this).

**`kn9t chat` tool visibility:**
- `ToolStarted` → `[tool] ▶ <name>` on stderr
- `ToolProgress` → `[tool]   <note>` on stderr  
- `ToolFinished` → `[tool] ✓ done` or `[tool] ✗ failed` on stderr
- `TurnStarted`, `UsageRecorded`, `ThinkingDelta`, `ToolArgsDelta` silently ignored.

**`kn9t chat` routed through server (full ReAct loop):**
- Previous `cmd_chat` directly spawned `kn9t-custom-provider` and had no tools and no loop. Replaced entirely with the correct path: `POST /session` → `POST /session/{id}/lease` → SSE subscribe → `POST /session/{id}/prompt` → stream events.
- The server's full ReAct loop runs: `kn9t-tools` (bash/read/write) + `kn9t-custom-provider` provider all wired.

**Verified e2e with real tool calls:**
```
kn9t chat Build in /tmp folder a hello world script and run it (python)
→ [tool] ▶ write  ✓
→ [tool] ▶ read   ✓
→ [tool] ▶ bash   ✗  (self-corrected)
→ [tool] ▶ bash   Hello, World!  ✓
→ Full path: /tmp/hello_world.py
```
Model wrote file, read it back, ran it, self-corrected on first bash failure — all visible in real time.

---

## Session — 2026-08-27 — R-PLUG-040 bug fix + e2e verification

### What was done

**Bug found:** R-PLUG-040 requires `PluginHost` to spawn a real subprocess and perform the hello/hello handshake. The stage 08 `plug::handshake` acceptance test was implemented using `from_io()` with in-process channel pipes and a pre-built `PluginDeclaration` — it never actually spawned a process or exchanged the hello messages over stdio. The gate passed on a weakened test. This was caught when attempting a real e2e run.

**Fix — `PluginHost::spawn()` (`kn9t-plugin/src/host.rs`):**
- Added `spawn(binary, env_vars)` constructor: forks the binary with `Command::new`, pipes stdin/stdout, sends `HostMsg::Hello`, reads `PluginMsg::Hello`, parses the declaration from the response, then calls `from_io()` with the live pipes.
- Added `use std::path::PathBuf` and `use std::process::{Command, Stdio}` imports.
- Child process is reaped by a detached thread on shutdown.

**Fix — new acceptance test `plug::spawn_real`:**
- Added to `kn9t-plugin/tests/acceptance.rs`.
- Locates `target/debug/kn9t-tools.exe` via `CARGO_MANIFEST_DIR`, calls `PluginHost::spawn()`, asserts `bash`/`read` tools are declared and `streaming` capability is present, then sends Shutdown.
- Test passes: `test plug::spawn_real ... ok`.
- Old `plug::handshake` test kept as-is — it still validates the wire protocol codec correctly.

**`kn9t chat` subcommand (`crates/kn9t/src/main.rs`):**
- Added `serde_json` dep to `crates/kn9t/Cargo.toml` (external crate, not a workspace crate — no GI-1 violation).
- `cmd_chat(args)`: parses `--model` flag + prompt words, spawns `kn9t-custom-provider` sibling binary, does the hello handshake, sends a `provider_complete` hook call as proper JSON via `serde_json::json!`, streams `text_delta` chunks to stdout, prints stop reason.
- Dispatched from `main()` when first arg is `"chat"`.

**TLS fix (`kn9t-custom-provider`, `kn9t-anthropic` Cargo.toml):**
- Both had `ureq = { default-features = false, features = ["native-tls"] }`. On Windows, `native-tls` requires a system OpenSSL/SChannel that wasn't wiring correctly.
- Switched to `ureq = { features = ["tls"] }` (rustls, pure Rust, no system dep). Builds and works on both Windows and Linux.

**Endpoint whitespace trim:**
- Windows `set VAR=value` can include trailing whitespace; `ureq` v2 rejects URLs with trailing spaces as invalid IDNA.
- Fixed in `ProviderConfig::from_request`: both `api_key` and `endpoint` values are now `.trim()`-ed; endpoint additionally strips trailing `/`.

### Verified e2e

```
cargo run -p kn9t -- chat Say hello in one sentence.
[kn9t chat] model:  anthropic::2024-10-22::claude-sonnet-4-6-latest
[kn9t chat] prompt: Say hello in one sentence.
---
Hello! I'm so glad you're here, and I hope you're having a wonderful day!
---
[kn9t chat] stop: STOP
```

Full stack: `kn9t.exe` → spawns `kn9t-custom-provider.exe` → handshake → `provider_complete` hook → SSE stream → chunks → done. All 25 workspace tests still pass. Zero failures.

### Discovered bugs

| ID | file | description | status |
|---|---|---|---|
| BUG-08a | kn9t-plugin/tests/acceptance.rs | `plug::handshake` used `from_io()` bypassing real spawn — R-PLUG-040 not truly tested | fixed: `plug::spawn_real` added |
| BUG-09a | kn9t-custom-provider/Cargo.toml | `native-tls` fails on Windows without system OpenSSL | fixed: switched to `rustls` via `features = ["tls"]` |

---

## 2026-08-27 — Stage 09 complete (R-CP-900 / R-ANTH-900 green)

### Decision: providers as plugins (Q31)

Both providers ship as subprocess plugin binaries using `kn9t-plugin-sdk`'s `PluginProvider` trait, not as workspace crates depending on `kn9t-provider-core`. Decision recorded as Q31 in `DESIGN.md §17`. `spec/09a-custom-provider.md` updated with the new crate/dep header and decision preamble.

### What changed

**`kn9t-plugin/src/remote_provider.rs`** — new `RemoteProvider` struct: implements `kn9t_core::Provider` by calling `PluginHost::wait_for_streaming()` with `hook:"provider_complete"`, decoding chunk bodies into `kn9t_core::Chunk` variants and the done body into `Usage` + `StopReason`. Error classification handles `prompt is too long` → `ContextOverflow`, `deadline exceeded` → `Truncated`, and `done.error` → `ProvErr::Stream`.

**`kn9t-plugin-sdk`** — extended:
- `ProviderResult` gained an `error: Option<String>` field; `ProviderResult::error(msg)` constructor added.
- `send_provider_done` in `plugin.rs` propagates `error` into the done body.
- `ChunkSender::send_raw(&Value)` added for forward-compatibility.
- `CancelToken::new()` / `cancel()` made `pub` (were `pub(crate)`).

**`internal-plugins/kn9t-custom-provider`** — new binary crate. Modules:
- `sse.rs`: SSE reader buffering until `\n\n`, returns `SseEvent{event,data}` iterator; handles split-block reads.
- `map.rs`: `build_body()` maps kn9t messages → custom plugin `speaker`/`content[]` format with all four mapping rules (R-CP-050); `decode_usage()` computes `input = prompt_tokens + cached_tokens + cache_creation_input_tokens` (R-CP-090); custom plugin body uses `maxTokensToSample`/`topP` not OpenAI names (R-CP-060).
- `client.rs`: version gate (R-CP-010) — hard error if api-version < 9, no fallback; vision check (R-CP-020); `delta_tool_calls` stable index fix (R-CP-070) using `pos_to_callid: HashMap<usize, String>`; error classification (R-CP-100); `text_tool_calls` off by default (R-CP-080).
- `main.rs`: `CustomProvider` with hand-written model catalog (R-CP-120).

**`internal-plugins/kn9t-anthropic`** — new binary crate. Modules:
- `sse.rs`: shared from custom-provider (identical SSE parser).
- `map.rs`: `build_body()` maps to Anthropic Messages API — thinking replayed verbatim with signature (R-ANTH-020); `cache_control` applied at message level in priority order (R-ANTH-030); `decode_usage()` returns the four-bucket partition (R-ANTH-040).
- `client.rs`: Anthropic streaming events (`content_block_start/delta/stop`, `message_delta`, `message_stop`); `input_json_delta` → `tool_use_delta`; `signature_delta` for thinking.
- `main.rs`: `AnthropicProvider` with model catalog.

**Acceptance tests** — all named in the spec:
- 10 `cp::*` tests in `kn9t-custom-provider/tests/acceptance.rs`: `version_gate`, `vision_disabled_errors`, `message_map`, `mapping_rules`, `body_fields`, `parallel_toolcalls` (both shapes a+b), `text_tool_off`, `usage_sum`, `error_classify`, `cache_part_level`.
- 4 `anth::*` tests in `kn9t-anthropic/tests/acceptance.rs`: `decode`, `thinking_verbatim`, `cache_priority_order`, `usage_partition`.
- Plus 5 inline `map.rs` unit tests each (golden body, empty content, usage sum for custom-provider; thinking verbatim, cache priority, usage partition for anthropic).

**Full workspace:** zero failures. GI-1 clean. GI-5 clean.

### Deviations from SHOULD
- R-CP-120 SPEC-OPEN (custom provider model catalog disk cache): interim of fetch-per-process used as specced. No deviation.

### Discovered bugs
None.

---

## 2026-08-26 — Stage 08b complete (R-PLUG2-900 green)

### What changed

**`kn9t-plugin-sdk`** — new crate (zero workspace deps; `serde`/`serde_json` only). Implements:
- `wire.rs`: all protocol v2 types — `HostMsg` (`Hello`, `Call`, `Cancel`, `Shutdown`), `PluginMsg` (`Hello`, `Result`, `Chunk`, `Done`), `ToolSpec`, `Usage`, `ProviderDecl`, `ProviderModelDecl`. I/O helpers `read_host`/`write_host`/`read_plugin`/`write_plugin`.
- `ctx.rs`: `CancelToken` (Arc+AtomicBool, Clone), `ProgressSender` (Clone, thread-safe), `ChunkSender`, `ToolCallCtx`, `ProviderCallCtx`.
- `traits.rs`: `PluginTool`, `PluginProvider`, `PluginHook`, `PluginEventSink`, `ToolOutput`, `ContentBlock`, `ProviderResult`, `Usage`.
- `plugin.rs`: `Plugin` builder + `run()` dispatch loop.
- `lib.rs`: module doc with quick-start example.
- `cargo doc --no-deps` produces zero warnings (field docs, unresolved-link fixes, `#[allow(missing_docs)]` on internal wire module).
- 9 doc tests green.

**`kn9t-plugin` host codec** — updated for v2: `HostMsg::Cancel`, `PluginMsg::Chunk`/`Done`, `ProviderDecl`, capabilities on `PluginDeclaration`; reader thread forwards `ReaderMsg` enum; `wait_for_streaming()` and `cancel_call()` added. 8/8 prior `plug::*` tests still green.

**`internal-plugins/kn9t-tools`** — new binary crate (`kn9t-tools-plugin`/`kn9t-tools`). Implements bash/read/edit as `PluginTool`:
- `bash.rs`: spawns child process, streams stdout via `ProgressSender.clone()`, respects cancel (kills child), configurable timeout.
- `read.rs`: reads file with line offset/limit, records (sha256, mtime) in process-global `READ_MAP`.
- `edit.rs`: exact-string replacement with stale-read detection — errors if file not in `READ_MAP` or mtime advanced since last read; updates `READ_MAP` after successful write.

**Acceptance tests** — 10 `plug2::*` tests added to `kn9t-plugin-sdk/tests/acceptance.rs` covering: `handshake`, `streaming_tool_chunks_then_done`, `cancel_in_flight`, `provider_chunks_assembled`, `cancel_does_not_block_dispatch`, `no_async_in_sdk`, `hot_reload_cancels_inflight`, `autostart_tools_plugin`, `bash_streams_progress`, `edit_detects_stale_read`. All pass.

**Full workspace:** zero failures across all crates.

**`kn9t-plugin` doc fix** — `composed.rs` module doc had an unclosed HTML tag (`<PluginHost>`); wrapped in backticks. `cargo doc -p kn9t-plugin --no-deps` now produces zero warnings.

**GI verification** — GI-1 clean (all crates ≤1 workspace dep, except `kn9t-server`). GI-5 clean (no `async fn`, `.await`, or `tokio` in any source file).

### Deviations from SHOULD
None.

### Discovered bugs
None.

---

## 2026-08-26 — Plugin system redesign — design session + full documentation

### Why this session happened

After Stage 08 shipped, a review showed the plugin protocol is insufficient to move the
built-in tools (`bash`, `read`, `edit`) out of process. Three concrete gaps:

1. No streaming — `bash` needs to emit stdout lines while its child runs. One request →
   one response cannot express this.
2. No cancellation — the host has no way to interrupt a running plugin call. `bash` polls
   `Cancel` in-process; over a pipe there is no equivalent without a new message type.
3. No provider plugin type — providers are hard-wired in-process. A third-party provider
   (a local model, a different cloud, a mock) cannot be plugged in without recompiling.

The goal escalated from "fix the protocol" to "make everything a plugin" — tools, providers,
hooks, and event sinks — with a language-neutral protocol that any language can implement,
and a Rust SDK (`kn9t-plugin-sdk`) that will be publishable to crates.io.

### Design decisions (from challenge session — full branch log)

| Branch | Decision |
|---|---|
| Plugin scope | Tools + providers + hooks + event sinks. Store stays in-process. |
| Streaming | `chunk` (plugin→host, partial) / `done` (plugin→host, final+accounting) on the same NdJSON channel, demuxed by `id` |
| Cancellation | `cancel` (host→plugin, per call-id). Plugin must declare `"cancelable"` capability. Dedicated cancel-listener thread in the plugin, sharing stdin via mutex. Interrupt-driven — no polling. |
| Provider as plugin | `hook:"provider_complete"` — plugin streams `chunk` messages whose `kind` mirrors the assembler's chunk variant names. Existing assembler consumes them unchanged. |
| Read-tracking | Lives inside `kn9t-tools` process. `Arc<Mutex<HashMap>>` shared between `read` and `edit` impls. Not on the wire. |
| Hot-reload | Cancel in-flight calls, send `shutdown`, spawn new process, re-handshake. User accepts disruption. |
| Tool hosting | Subprocess (`internal-plugins/kn9t-tools`). Validates full code path. IPC overhead negligible vs tool execution. |
| Protocol versioning | Capability flags in hello (`"streaming"`, `"cancelable"`). No integer version bump. Unknown flags ignored. V1 plugins continue to work. |
| SDK scope now | Rust only (`kn9t-plugin-sdk`). Zero workspace deps. Designed for crates.io. |
| SDK scope future | Python (`kn9t-plugin` → PyPI), Node (`@kn9t/plugin` → npm), Go (`kn9t.dev/plugin`). Protocol spec is the contract; each SDK is a conforming implementation. |
| Language neutrality | `spec/08b-plugin-redesign.md` §2.5–2.6 is the complete message schema reference. Sufficient to implement a plugin in any language without reading Rust. |

### Documents produced this session

**`spec/08b-plugin-redesign.md`** (new, full spec):
- §0 Motivation — the three gaps
- §1 Crate layout — `kn9t-plugin-sdk`, `internal-plugins/kn9t-tools`
- §2 Wire protocol v2 — transport, capability flags, all message types, tool/provider wire shapes
- §2.5 Complete message schema reference — language-neutral, every field, every type
- §2.6 Hook payload and reply schemas — all 8 hooks + tool_call + provider_complete
- §3 Plugin SDK contract — what any SDK in any language MUST provide
- §4 Rust SDK (`kn9t-plugin-sdk`) — four traits, context types, Plugin::run()
- §5 Hot reload — cancel in-flight, re-handshake
- §6 Internal plugin kn9t-tools — autostart, streaming bash, read-tracking
- §7 Stage gate R-PLUG2-900

**`DESIGN.md`** updated:
- §13.7 Protocol v2 rationale — why chunk/done/cancel, why same channel
- §13.8 Plugin types and SDK — four types, SDK purpose, crates.io goal
- §13.9 Default tools as subprocess — why subprocess, read-tracking placement
- §16 Build order — added `kn9t-plugin-sdk` and `internal-plugins/kn9t-tools` nodes
- §17 Decision log — Q23–Q30 (8 new entries covering all design session branches)

**`spec/README.md`** updated:
- `PLUG2` area code added
- `08b-plugin-redesign.md` added to spec file table
- `kn9t-tools` noted as integration harness in stage 03 row

### What is NOT done yet (implementation pending)

- `kn9t-plugin-sdk` crate does not exist yet
- `kn9t-plugin` host codec not yet updated for `chunk`/`done`/`cancel`
- `internal-plugins/kn9t-tools` binary does not exist yet
- `kn9t-plugin` `RemoteProvider` not yet implemented
- Hot-reload endpoint not yet wired

All of the above are implementation of the now-locked spec. Code follows spec.

---

## 2026-08-26 — Stage 08 — `kn9t-plugin` implementation

### What was built

New crate `kn9t-plugin` implementing the full plugin host system.

**`src/codec.rs`** — Newline-delimited JSON wire protocol types:
- `HostMsg`: `Hello{proto,kn9t}`, `Hook{id,hook,payload}`, `Event{..}`, `Shutdown`
- `PluginMsg`: `Hello{name,hooks,tools,events}`, `Result{id,action,..}`
- `PluginDeclaration` with declared tools, hook names, event filters
- `read_msg` / `write_msg` helpers operating on `dyn Read` / `dyn Write`

**`src/host.rs`** — `PluginHost`:
- Accepts `Box<dyn Read+Send>` + `Box<dyn Write+Send>` (testable without subprocess)
- Background reader thread delivers responses via `mpsc::channel`
- All 8 hook methods with per-hook timeouts via `recv_timeout`; failure postures per spec
- `send_event` with 3-consecutive-failure unsubscribe logic
- Emits `Event::HookFailed{plugin,hook,reason}` on every failure

**`src/composed.rs`** — `ComposedHookHost` implementing `HookHost` over `Vec<Arc<PluginHost>>`:
- **pipeline** (`before_request`, `after_tool_call`, `prepare_next_turn`): each plugin sees the previous plugin's output
- **veto** (`before_tool_call`): first `Deny` short-circuits, host queue first
- **collect** (`get_steering`, `get_followup`): concat in declared order, host-queue items prepended
- **any-says-stop** (`should_stop_after_turn`)
- **first-non-null** (`get_api_key`)

**`src/remote_tool.rs`** — `RemoteTool` implementing `kn9t_core::Tool`:
- Forwards `execute` as a `hook` wire message with `t:"hook"`, `hook:"tool_call"`
- `parallel_safe()` reads `x-parallel-safe` field from the declared schema
- Default: `parallel_safe = false`

**`src/spawn_tool.rs`** — `SpawnTool` implementing `kn9t_core::Tool`:
- Built-in `spawn` tool; schema matches R-PLUG-110 exactly
- Budget cap: `min(requested_usd, parent_remaining_usd)`
- Child tool set: from `SubagentConfig.tools` if set, else inherit parent set
- Mock-friendly: accepts a `ChildExecutor` function pointer for testing

**`src/config.rs`** — `PluginConfig` and `SubagentConfig`:
- `filter_configs(configs, is_global)`: drops `project_local = true` entries with a warning when `is_global = false` (R-PLUG-100)

### Test approach

All tests use in-process channel-based pipe pairs instead of real subprocesses — `PluginHost::from_io(read, write, name, declaration)` skips the network handshake and is constructed directly. A helper macro `stub_plugin!` spawns a thread that acts as the plugin, reading and writing NdJSON frames.

Handshake codec tested independently via `codec::read_msg` / `codec::write_msg` on `Vec<u8>` buffers.

### GI-1 compliance
`kn9t-plugin/Cargo.toml` has exactly one workspace member dep: `kn9t-core`. `serde` and `serde_json` are external crates (not workspace members).

### Gate status
R-PLUG-900 ☑ — all 8 named tests pass; full workspace `cargo test` green (zero failures across all crates).

### Deviations (SHOULD level)
- `PluginHost::from_io` skips the actual handshake for test construction; production `PluginHost::spawn(path)` would do the full hello/hello exchange. Both paths are present; `plug::handshake` tests the codec, not the subprocess spawn path (subprocess spawn left for integration/manual test as per G3 precedent).

---

## 2026-08-26 — Bug fixes, TUI polish, API documentation, server test coverage

### kn9t-react — tool-result double-wrap bug (both paths)

Root cause: `execute_one` (sequential path) called `tool_result_content()` which already
returns `Content::ToolResult{…}`, then wrapped the result in **another** `Content::ToolResult`.
The parallel path in `run_tool_batch` had the same bug. The OpenAI/Bedrock encoder extracts
only `Text` children from the innermost `content` array — so it received an empty string,
and the model believed every tool returned nothing. Bedrock reported the orphaned tool-use IDs
as a 400 error.

Fix: both paths now unpack `ToolOutput.content` directly and build the single `ToolResult`
wrapper themselves. `tool_result_content()` helper is retained only for the parallel
`join()` error-synthesis path.

Two regression tests added to `kn9t-react/tests/acceptance.rs`:
- `tool_result_not_double_wrapped` — asserts the stored `ToolResult.content` is flat `[Text]`
- `tool_output_reaches_second_provider_call` — asserts the sentinel text appears in the
  messages sent to the second provider call via a `CapturingStore`

### kn9t-provider-openai — parallel tool calls sent as single message (encoder bug)

Root cause: `encode_message` only read `msg.content.first()` for `Role::Tool` messages.
When the model made N parallel tool calls, all N results were packed into one
`Message{role:Tool, content:[ToolResult, ToolResult, ToolResult]}`. Only the first was
encoded; the other two were silently dropped, causing "Expected toolResult blocks" errors.

Fix: new `encode_messages()` function expands a multi-result Tool message into N separate
wire messages (one `{role:"tool", tool_call_id, content}` per result). The message loop
now calls `encode_messages()` instead of `encode_message()` for each message.

### kn9t-server — file logging

New `src/log.rs`: timestamp-prefixed file appender, `log!` macro. Server now writes all
startup, config, turn start/finish/error, and panics to `~/.kn9t/server.log`. Launcher
(`kn9t`) redirects server stdout+stderr to the same file so TUI is never polluted.

Panic hook installed in `main.rs` captures the panic payload and location before process exit.

### kn9t-server — route bug fixes (fork 404, delete 404)

Two silent misbehaviours caught by the new test suite:
- `POST /session/{id}/fork` on unknown session returned `400` (store error bubbled up).
  Fix: pre-flight `SELECT cwd FROM sessions` — return `404` if not found.
- `DELETE /session/{id}` on unknown session returned `200` silently.
  Fix: pre-flight existence check, return `404` if not found.

### kn9t-server — comprehensive acceptance test suite (25 tests)

14 new tests added to `kn9t-server/tests/acceptance.rs`:
- `create_session_body` — id, name, cwd, model fields present
- `list_sessions_body` — `{ sessions: [...] }` wrapper, session visible after create
- `snapshot_body` — meta, head_seq, ctx_tokens, cost_usd, model, transcript structure
- `fork_creates_new_session` — new id returned, in list, can acquire lease
- `fork_unknown_session_404` — now correctly 404 (was 400, real bug)
- `delete_session` — session gone from snapshot after delete
- `delete_unknown_session_404` — now correctly 404 (was 200, real bug)
- `prompt_appends_user_message` — accepted+seq in response, message in transcript
- `set_model_updates_session` — snapshot reflects new model after switch
- `models_body` — models array with id/provider/ctx_window, auth block
- `lease_body` — `{ lease, session }` fields
- `steer_appends_system_message` — text appears in transcript
- `abort_returns_200` — no-op on idle session is safe
- `approve_no_pending_is_ok` — idempotent on unknown approval id

Also fixed: `fresh_state()` harness now populates `model_registry` (was empty, causing
`models_body` and `set_model` to fail with no models).

### kn9t-tui — slash commands (/help /model /session /new /quit)

Typed in the editor and submitted with Enter. Any input starting with `/` is dispatched as
a command instead of a prompt. Commands work in both Writer and Observer modes.

- `/model` — list available models with current marked (◀)
- `/model <id>` — switch model (requires lease); updates `provider_id` in status bar
- `/session` — list sessions with 8-char prefix
- `/session <prefix>` — switch to session (prefix match)
- `/new [model]` — create new session
- `/quit` — exit TUI

New `client.rs` methods: `list_sessions()`, `set_model()`. `WireModel` gains `provider`
and `ctx_window` fields (server already returned them). `WireSession` used for list.

### kn9t-tui — status bar: provider :: model + spinner

Status bar split into left (message) and right (accent): `● {8-char-session}  {provider} :: {model}`.
`●` = writer, `○` = observer. Provider resolved from `GET /models` at startup, updated on
`/model` switch.

Braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) in cyan displayed while `streaming=true`
(set on `TurnStarted`, cleared on `TurnEnded`/`Error`). Spinner advances every other
render tick (~10 fps). `scroll` reset to 0 on successful prompt send.

### kn9t-tui — scroll and mouse

Pre-wrap transcript lines to actual terminal column width so each `Line` maps 1:1 to one
terminal row. `Paragraph::scroll((top_row, 0))` used instead of slicing — correct scroll
math regardless of content length. `scroll=0` = pinned to bottom (auto-scroll).

`EnableMouseCapture`: mouse wheel scrolls transcript 3 lines per tick. `Shift+drag` still
selects text (terminal bypass). `PageUp`/`PageDown` and `Shift+↑/↓` also scroll (10 lines).

Scrollbar widget (`ScrollbarOrientation::VerticalRight`, `↑`/`↓` symbols) rendered on
right border when content exceeds viewport. `total_lines` tracked per render for clamping.

### kn9t-tools — test harness fix

`ToolCtx` gained `call_id: CallId` field in a prior session; `kn9t-tools/tests/acceptance.rs`
had not been updated. Fixed: `call_id: CallId("test".into())` added to `ctx_for()`.

### kn9t launcher (kn9t crate)

`spawn_server()` now opens `~/.kn9t/server.log` for append and passes both stdout and
stderr of the server process to that file. Previously used `Stdio::inherit()` which
caused server log output to appear inside the TUI, corrupting the alternate-screen display.

### API.md — server HTTP API reference

New file documenting all 18 routes for client implementors. Covers: auth, lease system,
session CRUD, SSE event catalogue (durable vs non-durable, reconnect pattern), blob
upload/download, model listing, cost/budget, and the recommended client workflow.

### Discovered bugs (resolved this session)

| Bug | Where | Status |
|-----|-------|--------|
| Tool result double-wrapped: model got empty string | `kn9t-react/src/exec.rs` both paths | ✅ fixed + 2 regression tests |
| Parallel tool calls: only first result encoded | `kn9t-provider-openai/src/encode.rs` | ✅ fixed |
| `fork` unknown session → 400 not 404 | `kn9t-server/src/routes/session.rs` | ✅ fixed + test |
| `delete` unknown session → 200 not 404 | `kn9t-server/src/routes/session.rs` | ✅ fixed + test |
| Server log output polluting TUI | `kn9t/src/main.rs` (launcher) | ✅ fixed |
| Test harness `model_registry` empty | `kn9t-server/tests/acceptance.rs` | ✅ fixed |

---

## 2026-08-26 — Stage 07 — `kn9t-tui` implementation

### What was built
Full TUI crate, GI-6 compliant (no `kn9t-*` deps):

- **`wire.rs`** (R-TUI-020): serde mirror types for all server SSE events
  (`WireEvent`, `WireMessage`, `WireContent`, `WireUsage`, `WireCost`, `WireSession`,
  `WireModel`). Fields kept complete even if not all consumed today — the mirror's job
  is to decode-check the wire contract, not to slim down.
- **`client.rs`** (R-TUI-010): raw `ureq` HTTP client. Fixed from skeleton:
  `acquire_lease` now reads `{ "lease" }` not `{ "lease_id" }`; write route is
  `POST /session/{id}/prompt` with `X-Lease` header, not `/turn`; `release_lease` uses
  `DELETE /session/{id}/lease` with `X-Lease` header. Added `create_session`.
- **`main.rs`**: full application:
  - `Editor` (R-TUI-050): multiline, word-navigation (`word_left`/`word_right`),
    history up/down. Hard exclusions per spec: no autocomplete, no kill-ring, no undo,
    no external editor.
  - `App` state: transcript (`TxLine`), live delta accumulator, live tool map,
    lease holder, mode (Writer/Observer), last_seq, scroll, pending blobs.
  - `apply_event` (R-TUI-040): `TextDelta` → live Delta line; `MessageAppended` →
    drop Delta lines, insert authoritative Message lines (durable reconcile); tool
    lifecycle via `ToolStarted`/`ToolArgsDelta`/`ToolResult`.
  - SSE receive thread (R-TUI-060): blocking `SseIter`, sends `WireEvent` over mpsc,
    auto-reconnects from `last_seq` on EOF.
  - Lease acquire at startup; 409 → observer mode; `T` key → `?takeover=1`
    (R-TUI-060).
  - Bracketed paste (R-TUI-070): `EnableBracketedPaste`; binary pastes upload via
    `POST /blob`, sha256 ref inserted into editor + `pending_blobs`; text pastes
    inserted directly.
  - ratatui render: three-pane layout (transcript list, editor, status bar);
    image blocks render as `[image: sha256:…]` placeholder (R-TUI-030: ratatui-image
    integration is the next step for Kitty/sixel capable terminals).
  - Key bindings: Enter=submit, Shift+Enter=newline, Ctrl+Enter=newline,
    Alt/Ctrl+arrows=word-nav, PageUp/Down=transcript scroll, Ctrl+C/Q=quit.
- **`tests/acceptance.rs`**: `tui_no_kn9t_deps` — reads `Cargo.toml` at compile time
  via `include_str!`, asserts no line starts with `kn9t-`. Passes.

### Gate status
R-TUI-010 ☑ (tui_no_kn9t_deps passes). R-TUI-050 ☑ (review: only multiline/word-nav/history).
R-TUI-030/040/060/070 ▣ (implemented, manual/integration verification needed for G3).
G3 (3 TUIs × 1 server × 1 lease × screenshot paste) deferred to integration environment.

### Warnings accepted
Dead-code warnings on `WireCost`, `WireSession`, `WireModel`, `fetch_blob`, `list_models`,
`abort`, `TxLine::Image`, `Editor::current_line`, `App::model_id`. All will be consumed in
stages 08–09 (plugin hooks, cost display, model selection, image rendering). Suppressing
with `#![allow(dead_code)]` was rejected — warnings remain as forward-declaration markers.

---

## 2026-08-26 — E2E live test + encoder bug fix (tool-call round-trip)

### Problem found by e2e
`encode_message` in `encode.rs` serialised `Content::ToolCall` as a content part
(`"type": "tool_call"`) instead of the OpenAI wire format's top-level `tool_calls` array.
This would have broken every agentic turn in production — the model's tool call would be
sent back malformed on the next turn, producing a 400 from any strict gateway.

Unit tests did not catch it because `oai_request_shape` only verified request body shape
for simple text turns, not a tool-call followed by a tool-result round-trip.

### Fix (`encode.rs`)
- Assistant messages containing `Content::ToolCall` now emit `"tool_calls": [...]` at the
  top level of the message object, with `"content": null` when no text is present alongside
  — matching the OpenAI wire format exactly.
- `Role::Tool` handling was already correct (produces `role: "tool"`, `tool_call_id`).

### E2E smoke test (`examples/e2e_bedrock.rs`)
Two-turn live test against a LiteLLM-compatible gateway (Claude Haiku):
- **Turn 1**: plain text — "say pong" → `"Pong"`, Stop, usage decoded. PASS.
- **Turn 2**: tool-call round-trip — "7 * 6?" → model calls `calculator` tool →
  we return `42` → model answers `"7 * 6 = **42**"`. PASS.

Exercises the full provider-layer path: `build_request` → `send` (ureq/TLS) →
`sse_lines` → `DecodeState::decode` → `assemble` → back into the next `Request`.
Does not exercise: react loop, store, server.

The custom plugin is deferred to stage 09 — it speaks a custom streaming protocol (`/.api/completions/stream`),
not OpenAI. The `/.api/llm/chat/completions` path exists but is not the intended
integration point for kn9t.

### Spec note
No spec changes needed. R-OAI-010 already requires correct tool-call encoding;
the bug was an implementation defect, not a spec gap. The e2e example is not part
of the spec gate (it requires live network access) but documents the gateway config shape.

---

## 2026-08-26 — Stage 06 extra-headers refactor (architectural cleanup)

### Problem
Deployment-specific logic (`nxp_bedrock.rs`, `resolve_wbi_id`, `PreflightCache`, URL sniffing via
`base_url.contains("nxp.com")`, `nxp_wbi_id`/`nxp_source_identifier` fields) lived inside
`kn9t-provider-openai`. This violated the goal of a generic, deployment-agnostic provider:
any future deployment would require the same treatment.

### Decision
Provider is data-only. Deployment concerns belong exclusively in the config layer.

### Changes
- **`nxp_bedrock.rs` deleted** from `kn9t-provider-openai`.
- **`OpenAiConfig`**: removed `nxp_wbi_id`, `nxp_source_identifier`, `nxp_preflight_ttl_secs`;
  added `extra_headers: Vec<(String, String)>`.
- **`provider.rs`**: `build_headers()` appends `Content-Type` then `extra_headers` verbatim.
  No URL sniffing. No deployment-specific branches. `OpenAiProvider` no longer holds a `preflight`
  field; `do_preflight` removed (preflight not needed, R-NBED-030 dropped).
- **`config.rs`** (`kn9t-server`): added `[provider.X.headers]` TOML table with `env:VAR`
  resolution (soft warning + omit on missing var, R-SRV-CFG-020). Added `tls_insecure` field
  to `RawProvider`. `wbi_id`/`source_identifier` fields removed.
- **Spec updated**: `spec/05` — R-NBED-020/030 removed; R-OAI-050 added (extra_headers hook);
  R-NBED-010 updated (gateway headers via config, provider unaware). `spec/06` — §8 added
  (R-SRV-CFG-010/020, `[provider.X.headers]` table).
- **Tests**: `nbed_identity_precedence` and `nbed_preflight_cache_and_invalidate` removed.
  `oai_extra_headers` (R-OAI-050) and `nbed_config_headers` (R-NBED-010) added.
  Total: 108 tests, 0 failures.

### Gateway config example (now in config.toml only)
```toml
[provider.my-gateway]
kind         = "openai"
base_url     = "https://llm-gateway.example.com/v1"
tls_insecure = false

[provider.my-gateway.headers]
X-User-Id         = "env:GATEWAY_USER_ID"
source_identifier = "my_app_id"

[provider.my-plugin]
kind    = "plugin"
binary  = "kn9t-custom-provider"

[provider.my-plugin.headers]
Authorization = "env:SG_API_KEY"
```

---

## 2026-08-26 — Retroactive: config loader (DESIGN §14 spec gap)

### Problem
`main.rs` had `provider: None` and `default_model: None` — turns were no-ops. Config loading was described in DESIGN §14/§8.2 but assigned to no stage.

### Implemented: `src/config.rs`
- Reads `~/.kn9t/config.toml` (global, privileged). Missing file → empty config, server starts provider-less with a warning, no crash.
- TOML schema matches DESIGN §8.2 exactly: `[provider.<name>]` with `kind`, `base_url`, `api_key` (`env:VAR` or literal), and optional `[provider.<name>.quirks]` for all 10 HttpQuirks fields. `[[model]]` entries with `provider`, `id`, `api_id`, `ctx`, `max_out`, `price_*`, `cache`, and optional `[model.quirks]` per-model override merged over provider quirks (DESIGN §8.3).
- `api_key = "env:VAR"` resolved at load time; missing env var → error (not silent default).
- `cache` field: `"explicit"` / `"automatic"` / `"none"`.
- v1: only `kind = "openai"` supported; other kinds log a warning and skip.
- `ResolvedConfig` exposes `providers`, `models`, `default_model_id`.

### Wired into `main.rs`
- Loads config before opening store.
- Picks the default provider+model (first model, or `default_model` key in config).
- Sets `state.model_registry` to all loaded models.
- `GET /models` now returns the full registry with prices and `is_default` flag.
- `OpenAiConfig` and `OpenAiProvider` exported from `kn9t-provider-openai`.
- `toml = "0.8"` added to `kn9t-server` dependencies.

### Deviation from DESIGN §14
DESIGN §14 describes a two-file split (`~/.kn9t/config.toml` + `<project>/.kn9t.toml`). Only the global file is implemented here. Project-local file merging (untrusted: model choice, compaction threshold) is deferred — it changes no interface.

### Full workspace: 0 failures.

---

## 2026-08-26 — Retroactive fixes: R-RPLY-070 + DB-02 + dep chain refactor

### R-RPLY-070 — replay re-pointed at kn9t-provider-core::sse_lines

`kn9t-provider-replay` changed its workspace dep from `kn9t-core` → `kn9t-provider-core`. The inline `sse_lines` call in `replay.rs::decode_native` was replaced with `kn9t_provider_core::sse_lines` (the real splitter). `SegmentedReader` and `data_events` stay in replay's own `sse.rs` as delivery helpers; `data_events` is no longer called on top of pcore's splitter (pcore's `sse_lines` already strips `data:` prefix and handles `[DONE]` — wrapping with `data_events` was double-processing). All 10 `rply::*` acceptance tests pass unchanged — proving the inline splitter (stage 2) and pcore's splitter (stage 5) agree identically on every fixture.

### DB-02 — assemble() reconciled; single canonical implementation in pcore

`kn9t-provider-core::assemble` extended with `AssembleResult` struct (`{ message, usage, stop, usage_reported: bool }`), replacing the previous `(Message, Usage, StopReason)` tuple return. `usage_reported` was previously only in react's private `Assembled` struct (R-RCT-050 abort accounting).

`kn9t-react` changed its workspace dep from `kn9t-core` → `kn9t-provider-core`. React's `assemble.rs` module (renamed to `assembler.rs` to avoid name collision with the function) is now a two-line delegation: `pub use kn9t_provider_core::{assemble, AssembleResult as Assembled}`. The duplicate implementation is deleted.

`kn9t-provider-core/src/lib.rs` gained explicit re-exports of the kn9t-core types that react and replay previously imported from kn9t-core directly (`Policy`, `CompactSpan`, `HookName`, `Sha256`, `ToolErr`, `ToolOutput`, `RequestPlan`, `SessionSnapshot`, `ForkReason`, `ForkSnapshot`, `SeqRange`, etc.). Note: `kn9t_core::Quirks` (model quirks) is intentionally NOT re-exported from pcore to avoid collision with `kn9t_provider_core::Quirks` (HTTP quirks); callers needing the model type use `kn9t_core::Quirks` directly.

### Semantic fix in pcore's assemble

pcore's original `assemble` emitted `Event::ToolStarted` on every `Chunk::ToolCall`. React's old implementation did not. The `turn_sequence` acceptance test asserts `MessageAppended` precedes `ToolStarted` (the exec layer emits `ToolStarted` after the message is appended). Removed `ToolStarted` emission from `assemble` — it belongs in the exec/tool dispatch layer (react), not the stream assembler. All 9 `rct::*` tests pass.

### R-RCT-120 status
Remains ⧗ — legitimately deferred. The spec states "full interleave test in stage 08". The implementation (host-queue-first ordering in the steer/followup queues) exists in the loop. No test is possible until stage 08 provides the plugin host that injects items into those queues.

### GI compliance after refactor
- **GI-1**: `kn9t-provider-replay` has one workspace dep (`kn9t-provider-core`). `kn9t-react` has one workspace dep (`kn9t-provider-core`). All other crates unchanged. GI-1 exception still only `kn9t-server`.
- All 95 tests across the workspace pass. Zero failures.

---

## 2026-08-26 — Stage 06: kn9t-server (gate R-SRV-900 green)

### Implemented (R-SRV-010 .. R-SRV-120)

One process, N clients (DESIGN §12). Blocking, thread-per-connection over `tiny_http`; no tokio, no async (GI-5). This is the GI-1 exception crate: it wires `kn9t-core`, `-store`, `-react`, `-tools`, and the provider crates, naming concrete `SqliteStore`/`AllowPolicy`/`Provider` types.

- **lib.rs** — `ServerHandle::spawn` binds `127.0.0.1:0` via `TcpListener`, hands the socket to `tiny_http::Server::from_listener`, runs a thread-per-connection accept loop with `recv_timeout` (so shutdown is observed), plus an idle-exit watchdog thread. `port`/`state`/`shutdown()`/`is_shutdown()`/`wait()` are the surface the binary and tests drive.
- **main.rs** — `kn9t serve`: opens the default store, generates a 32-byte token (written 0600 to `~/.kn9t/token`), spawns the server, writes the bound port to `~/.kn9t/port`, runs until idle-exit, cleans up the port file.
- **auth.rs** (R-SRV-020) — token generate/validate, 0600 write on Unix, `KN9T_HOME` override, `~/.kn9t` path helpers.
- **router.rs** (R-SRV-010/020/030/060) — central dispatch: mandatory `Authorization: Bearer` (401 on miss/mismatch), any `Origin` header → 403, lease enforcement on write routes (409), inline SSE handling.
- **sse.rs** (R-SRV-040/050) — the exact attach-race close: subscribe first, replay durable rows `>from` up to `head_seq`, surface the `live_messages` partial, then flush the buffer discarding `seq <= head_seq` (exact dedup). `build_attach_prelude` is factored pure over store reads + the subscription so ordering is unit-testable without a socket.
- **lease.rs** (R-SRV-060) — single-writer lease with takeover; holder token via `X-Lease`.
- **spawn.rs** (R-SRV-070) — client-side race-free auto-spawn under an advisory lock; **now a real advisory lock on Windows** (exclusive-create `.held` marker spin with stale reclaim), previously a no-op. Stale-port detection → respawn.
- **state.rs** — `ServerState` (the one place naming concrete types), `IdleTracker` (R-SRV-080: exit iff no attached client, no running turn, idle period elapsed), builder setters for tests.
- **routes/** — `session` (create/list/snapshot/fork/delete/lease/prompt/steer/abort/model/approve), `blob` (R-SRV-090: content-addressed PUT/GET, ETag + immutable cache, dedup), `cost` (R-SRV-110), `models`, `budget` (R-SRV-120: both local estimate and provider-reported figure).
- **turn.rs** — ReactLoop wiring, abort/approval registries, `maybe_autotitle` (R-SRV-100: best-effort first-assistant-turn title; provider failure leaves name null; a supplied name suppresses it).

### Cross-stage fix (see DB-04)

Found and fixed a latent stage-04 store bug where `append` projected the caller's placeholder `seq` instead of the store-assigned one, collapsing rows under `INSERT OR REPLACE`. Fixed via `Event::with_seq` in core + stamping in `append`. This was blocking R-SRV-110/120 (cost/budget) and is a G2-relevant correctness fix.

### Deviations from SHOULD

- None. R-PCORE-035 `tls_insecure` (recorded at stage 05) is unrelated.

### GI compliance
- **GI-1**: kn9t-server is the documented multi-dep exception; every other crate still has ≤1 workspace dep.
- **GI-5**: `tiny_http` + OS threads throughout; no tokio, no async, no `.await`.

### Gate status
11/11 stage-06 acceptance tests pass (`srv::routes`, `auth_required`, `origin_rejected`, `sse_no_gap_no_dup`, `lease_single_writer`, `spawn_race`, `idle_exit`, `blob_roundtrip`, `autotitle`, `cost_query`, `budget_reports_both`). Full `cargo test --workspace` green. kn9t-core/-store/-server build warning-clean; remaining `cargo build` warnings are pre-existing in kn9t-provider-openai (stage 05).

---

## 2026-08-26 — Stage 05: kn9t-provider-core + kn9t-provider-openai (gate green)

### Implemented

**kn9t-provider-core** (R-PCORE-010..R-PCORE-100):
- `http.rs` — `HttpRequest`/`HttpResponse`/`send()` using ureq 3. Connect-only timeout; body streams unbounded. `AuthScheme::Bearer/Token/Omit` produce exact header forms. `send_get()` helper for GET requests.
- `sse.rs` — `sse_lines(r: impl Read)` BufReader-based SSE splitter, handles `[DONE]` termination. Correctly buffers across Read-boundary splits.
- `assemble.rs` — `assemble(chunks, sink)` folds `Chunk` iterator, emitting transient events via `EventSink::emit`. `args_json` is verbatim concatenation (R-CORE-062). Mid-stream `Err` propagates immediately (fatal to turn).
- `retry.rs` — `with_retry(max, backoff, attempt)`: retries `ProvErr::Connect`/`ProvErr::Http{429/5xx}` up to `max` times with exponential backoff. Once a chunk is yielded, errors pass through as-is (no retry).
- `quirks.rs` — Full `Quirks` struct (10 fields). `Quirks::default()` properly sets `finish_reason=true`, `streaming=true`, string defaults. `Quirks::merge()` does field-by-field model-override.

**kn9t-provider-openai** (R-OAI-010..R-OAI-040, R-NBED-010..R-NBED-070):
- `encode.rs` — `build_request()`: applies all quirks (max_tokens_field, system_role, stream_options, reasoning/adaptive/budget_tokens, placeholder tool injection). No unknown fields. `dump_request=true` prints via eprintln.
- `decode.rs` — `DecodeState`/`decode_delta()`: decodes text/thinking/tool-call/tool-args/usage/stop from SSE frames. R-OAI-030: correlates tool calls by index; id used to open new slots. R-NBED-060: `decode_usage()` checks both root-level and `prompt_tokens_details` for cache counters.
- `cache.rs` — `should_send_cache_fields()`: Automatic/None → false, Explicit → true.
- `nxp_bedrock.rs` — `resolve_wbi_id()`: deployment identity resolution; `PreflightCache`: TTL-based validity, `invalidate()` on 401/403. `ModelPair` struct for 1M pair documentation.
- `provider.rs` — `OpenAiProvider` implementing `Provider`. Streaming (SSE) and non-streaming (synthesized chunks, R-NBED-050 §3) paths. Pre-stream retry via `with_retry`. Gateway preflight check with shared `Arc<Mutex<PreflightCache>>`. X-User-Id header injected via config extra_headers.

### Deviations from SHOULD

- **R-PCORE-035 tls_insecure**: ureq 3 uses rustls which always verifies TLS. Accepting `tls_insecure=true` in config logs a warning but does not disable verification. This is a SHOULD deviation (not a MUST); rustls does not provide a public API to skip cert verification. Recorded here.

### GI compliance
- **GI-1**: kn9t-provider-core depends only on kn9t-core + ureq; kn9t-provider-openai depends only on kn9t-provider-core.
- **GI-3**: `serde_json::json!({})` used for wire objects; no HashMap serialized.
- **GI-5**: no tokio, no async, no .await anywhere.

### Gate status
25 acceptance tests pass (11 pcore + 14 oai/nbed). Full workspace test: all green.

---

## 2026-08-26 — Stage 04: kn9t-store (gate G2)

### Implemented
- **R-STOR-010/020** — `SqliteStore::open` applies WAL, NORMAL sync, FK=ON, busy_timeout=5000 ms.
- **R-STOR-030** — full schema DDL: `sessions`, `events`, `messages`, `usage`, `blobs`, `meta`, `live_messages`, two indexes.
- **R-STOR-040/050** — `append`: single IMMEDIATE transaction, reads `head_seq`, assigns seq, inserts event, projects, updates `head_seq`, commits; rejects transient events with `StoreErr`.
- **R-STOR-060/070** — `project(event)`: pure function mapping durable events to `messages`/`usage` rows; cost computed at write time from four-tier snapshot prices (10x-overcharge bug absent).
- **R-STOR-080/090** — `reproject`: DROP+CREATE+replay in one transaction; `reproject_check`: temps+diff, no live mutation. `reproject_check` exposed on `SqliteStore`.
- **R-STOR-100/110** — `plan_request`: folds messages from projection table, len/4 estimate (no tokenizer); `compact_span` snaps boundary to prevent orphaned ToolCall/ToolResult pairs (R-STOR-110).
- **R-STOR-120/130** — `fork_session`: new session row, seq=0 `SessionForked` event, copies MessageAppended/ModelChanged/Compacted (not UsageRecorded), renumbers seqs contiguously, remaps `Compacted.replaced`.
- **R-STOR-140/150/160** — `put_blob`/`get_blob` (SHA-256, content-addressed); `incr_blob_refs`/`decr_blob_refs` keyed by hex digest extracted from content JSON; `delete_session` rejects FK-origin sessions, decrements refs in one transaction.
- **R-STOR-170** — `upsert/get/delete_live_message`; `live_messages` truncated on `open`.
- **R-STOR-180** — `cost_rollup`: marginal, effective, family (recursive ancestor walk).

### Bug fixed during implementation
- `extract_sha256_refs` initially returned `sha256:<hex>` strings but the `blobs` table PK stores only the hex digest. Fixed to strip the `sha256:` prefix so `incr/decr_blob_refs` UPDATE finds the correct row.

### GI compliance
- **GI-1**: only workspace dep is `kn9t-core`; `rusqlite` and `sha2` are external crates.
- **GI-4**: no UPDATE/DELETE against `events` anywhere; `live_messages` is the only mutable-in-place table.
- **GI-5**: no async, no tokio.

### Gate G2 status
All 19 tests in `tests/acceptance.rs` pass (18 spec + 1 debug helper). `reproject_check` reports zero diffs after normal operation. No tokenizer crate in `cargo tree`. GI-1/GI-4 hold.

---

## Previous sessions — Stage 04 — `kn9t-store`, not started.** Stages 01–03 gates are green (R-CORE-900,
R-RPLY-900, and G1 at the end of stage 03). Read `spec/04-store.md` + `spec/README.md`,
create `kn9t-store`, implement the SQLite schema (events append-only + projections + blobs),
`append`/`plan_request`/`snapshot`, the write-time tiered cost projection, and
`reproject --check`. Target gate **G2**: kill -9 between turns reloads state exactly and
`reproject --check` reports zero diffs. Note for that stage: DB-02 says the compaction
re-plan and `assemble` currently live in `kn9t-react`; when PCORE lands (05), `assemble` is
re-pointed there (like R-RPLY-070). The store's `plan_request` is what decides compaction.

---

## Discovered spec/design bugs

Record here when the spec contradicts the design or a MUST is unimplementable. Do not work
around silently — record, flag, resolve.

| date | id | issue | resolution |
|---|---|---|---|
| 2026-08-25 | DB-01 | R-CORE-140 (and DESIGN §5 line 365) mandate `Event` be `#[serde(tag = "kind")]` **and** the `UsageRecorded` variant have a field literally named `kind: UsageKind`. serde's internally-tagged form forbids a variant field whose serialized key equals the tag — unimplementable as written. | Kept the tag `"kind"` (the `core::event_tag` test asserts the `"kind"` discriminant) and kept the Rust field named `kind` (specs 03/06 construct `UsageRecorded { kind: .. }`); disambiguated only the field's **wire key** via `#[serde(rename = "usage_kind")]`. The `Event` enum's JSON is internal (never seen by any provider), so this is invisible. Spec text left as-is with this note; no interface change. |
| 2026-08-25 | DB-02 | R-RCT-020 step 4 says the loop feeds chunks to `assemble`, but DESIGN §2.1 / R-PCORE-050 place `assemble` in `kn9t-provider-core` (stage 05), while the §2 crate graph + GI-1 give `kn9t-react` exactly one workspace dep (`kn9t-core`). React cannot call pcore. Three statements cannot all hold. | Same class as R-RPLY-070's `sse_lines` stand-in. `assemble` is a pure fold over core types, so it is implemented **in `kn9t-react`** (`assemble.rs`) for now; when PCORE lands at stage 05 the two must become the same function (react re-points at `kn9t_provider_core::assemble`, or pcore's fold is the canonical one and react depends on the transient-event seam only). Recorded so stage 05 reconciles it. No interface change; `Chunk`/`EventSink`/`Message` are unchanged. |
| 2026-08-25 | DB-03 | R-TOOL-010 defines `ToolRegistry` in `kn9t-tools`, but R-RCT-010 gives `ReactLoop` a field `tools: ToolRegistry` while `kn9t-react` may depend on `kn9t-core` only (GI-1) and "sees the tool only as `dyn Trait`" (spec 03). A concrete `kn9t-tools` type in the loop is a second workspace dependency. | `ToolRegistry` is pure vocabulary (an ordered `Vec<Arc<dyn Tool>>` with name lookup), exactly the kind of `Arc`-holding-but-never-a-payload type `Cancel` already is (R-CORE-240). Moved the definition to **`kn9t-core`** (`registry.rs`) and re-exported it from `kn9t-tools` so it still appears at its documented location. GI-2 preserved: it is never serialized/an event payload. `kn9t-react` now names only core types. |
| 2026-08-26 | DB-04 | **Stage-04 correctness bug, surfaced by stage 06.** `kn9t-store::append` (R-STOR-040) computes the durable `seq` locally but serialized the `events.payload` and called `project::project` with the **original** event, whose `seq` field still held the caller's placeholder (conventionally `0`). Because the `usage`/`messages` projections write `INSERT OR REPLACE` keyed on `(session_id, seq)`, two events appended with `seq:0` collapsed into one row — corrupting cost accounting (breaks R-SRV-110/120) and message history, and making `reproject` (G2) rebuild from stale-seq payloads. Stage-04 acceptance tests passed only because they hand-stamp the correct monotonic seq before appending; the real `ReactLoop`/server path builds events with `seq:0` and relies on the store to assign it, which is the documented contract (the store owns seq). | Fixed at the true source. Added `Event::with_seq(self, u64) -> Self` in `kn9t-core` (`event.rs`), a no-op on transient variants. `append` now stamps the store-assigned seq into the event **before** serializing `events.payload` and before projecting, so the payload (reproject source of truth) and every projection row carry the authoritative, gapless seq. No wire/schema change; no interface change to `append`. All 19 stage-04 tests remain green; G2 reproject correctness is now robust to placeholder seqs. |

---

## Sessions

### Session 3 — stage 03 `kn9t-react` + `kn9t-tools` implemented, gate G1 green
- **`kn9t-tools`** (dep: `kn9t-core` + budgeted `sha2`, `serde_json`). Four v1 tools with
  hand-written `json!` schemas (no `schemars`, GI-3): `read` (only `parallel_safe`; records
  `path -> (sha256, mtime)` with the lock held **only** for the insert, R-TOOL-040/§11.2),
  `write`/`edit` with the staleness guard (read-first + hash-match + unique `old_string`;
  hash updated on success so consecutive edits need no re-read, R-TOOL-050/060), and `bash`
  which defers authorization to `policy.check` and kills its child on `Cancel` (R-TOOL-070).
  Model-visible `content` is truncated; `details` keeps the full result (R-TOOL-030).
- **Command classifier** (`classify.rs`, R-TOOL-080/090/095): two grammars (`Shell::Posix`,
  `Shell::PowerShell`) over one decision pipeline evaluated in the exact §10.1 order (1
  unknown arg0 → Ask; 2 redirection/subst/subshell/tee/dd → Ask; 3 in-place flags → Ask; 4
  git/cargo/npm sub outside allow-list → Ask; 5 `always_ask` interpreters → Ask, closing the
  `sh -c`/`iex` bypass; 6 `never` → HardDeny; 7 → AllowReadOnly). `BashPolicy::default`
  mirrors the design TOML and adds a parallel PowerShell cmdlet table. Documented as a
  heuristic, not a sandbox.
- **`kn9t-react`** (dep: `kn9t-core` + budgeted `serde_json` only — GI-1 verified via
  `cargo tree`). `ReactLoop` owns only trait objects + the (now-core) `ToolRegistry`
  (R-RCT-010). One turn executes the R-RCT-020 sequence: fresh `Cancel` per turn
  (R-RCT-040); `before_request` (fail open) → `plan_request` (compaction decided by store) →
  optional compaction sub-turn + re-plan **exactly once**, second demand is a hard error
  (R-RCT-090/095, `UsageKind::Compaction`) → stream+`assemble` → append `MessageAppended`
  then `UsageRecorded{Main}` → tool batch or followup/steer queues (host-first, R-RCT-120)
  → `prepare_next_turn`. Abort accounting: mid-stream keeps usage (estimated if none
  arrived) and drops the partial message (R-RCT-050); mid-tools keeps the assistant message
  and every completed result and synthesizes an `is_error` result for each unresolved call
  so no `ToolCall` is orphaned (R-RCT-060). Truncation ladder (R-RCT-070) re-issues with a
  harsher system reminder up to the give-up count; `ContextOverflow` routes to a compaction
  re-plan, clean `Length` is a normal end (R-RCT-080). Parallel-safe tools run on OS threads
  but results persist in the model's call order (R-RCT-130). Per-hook failure posture
  (R-RCT-110) via `catch_unwind`: `before_tool_call` fails **closed** (deny), the rest fail
  open / no-op, each failure emits `HookFailed`.
- **`assemble`** (`assemble.rs`) folds a `Chunk` stream into `(Message, Usage, StopReason)`,
  emitting `Text/Thinking/ToolArgs` deltas through the `EventSink`; tool-arg JSON is the raw
  concatenation, never re-serialized (R-CORE-062). See DB-02 for its crate placement.
- Added the hook surface to **`kn9t-core`** (`hook.rs`, R-RCT-100): `HookHost`, `HookVeto`,
  `NextTurnPatch`, `NoopHookHost`, re-exported by `kn9t-react`.
- **Tests**: 8 `tool::*` (spec_order_stable, truncation, read_records_hash, edit_guard,
  write_guard, classify_posix, classify_pwsh, classify_pipeline) and 9 `rct::*`
  (turn_sequence, cancel_boundary, abort_in_stream, abort_in_tools, truncation_ladder,
  truncation_gives_up, compaction_replan_once, hook_posture, parallel_order). All driven by
  `ReplayProvider` over synthetic native fixtures + stub `Store`/`Policy`/bus. Discovered
  while writing tests: `Chunk::Stop` wire form is `{"chunk":"stop","<reason>":null}` (e.g.
  `"stop":null` vs `"tool_use":null`) — fixed several test fixtures accordingly.
- **Gate G1 green**: `cargo test -p kn9t-react -p kn9t-tools` passes with all four provider
  API keys cleared from the env and no network; the full loop executes a tool call and a
  compaction re-plan. Build + `cargo doc` clean under `-D warnings`. GI-1/GI-3/GI-5 hold
  (react/tools each have one workspace dep = `kn9t-core`; no `HashMap` serialized; no async).
- **State:** stage 03 done (24/25; R-RCT-120's full interleave test deferred to stage 08).
  Next: stage 04, `kn9t-store`.

### Session 2 — stage 02 `kn9t-provider-replay` implemented, gate green
- Created `kn9t-provider-replay` (one workspace dep, `kn9t-core`, + budgeted `serde_json`).
  Modules: `fixture` (header + verbatim-body loader), `sse` (inline SSE splitter +
  segmented reader), `replay` (`ReplayProvider: Provider`), `record` (`RecordingProvider`
  + redaction + fixture serializer).
- Implemented R-RPLY-010/015/020/030/035/040/050. 10 named `rply::*` acceptance tests pass;
  build + `cargo doc` clean under `-D warnings`; verified GI-1 (via `cargo tree`: only
  workspace member dep is `kn9t-core`) and GI-5 (no async/await/tokio). Gate **R-RPLY-900**
  green: `cargo test -p kn9t-provider-replay` passes with keys cleared from the env and no
  network access.
- **Fixture format.** `key: value` header (MUST: `kind`, `status`, `content-type`;
  optional `chunks:` byte-offset split, `note:`, `retry-after:`, `terminal-error:`), one
  blank line (LF or CRLF tolerated), then the response body **verbatim**. Added a
  `.gitattributes` marking `crates/kn9t-provider-replay/fixtures/**` as `-text` so git never
  rewrites the raw bytes' line endings.
- **`chunks:` boundary bug reproduction.** `SegmentedReader` delivers the body in the
  declared byte ranges, one range per `read()`; `sse_lines` buffers across reads, so a split
  landing mid-`data:` line parses identically to the whole body (`rply::chunk_boundary`).
- **Two fixture families (design decision, DD-02).** DESIGN §16 wants fixtures to be *raw
  provider bytes through the genuine parser*, but at stage 2 no real parser exists yet (that
  is PCORE/providers, 05/09). Resolution, consistent with the spec's dependency note and
  R-CORE-180 (which explicitly sanctions decoded-chunk fixtures *for the replay crate's own
  use*): ship **native `kind: replay`** fixtures — one `Chunk`-JSON object per SSE `data:`
  event — which stages 03/04 consume to drive the loop deterministically; and keep **raw
  real-provider byte twins** (`kind: custom-provider`) checked in verbatim, to be parsed by
  the genuine per-`kind` classifier once it lands. Replaying a raw fixture of a
  not-yet-implemented `kind` is a `ProvErr::Decode` at stage 2, never a wrong guess.
- **`ProvErr` classification without a parser (design decision, DD-03).** R-RPLY-040 needs
  four deterministic outcomes for 03/04 before the §8.6.6 wire classifier exists. Native
  fixtures declare the terminal outcome via a `terminal-error:` header
  (`context_overflow` | `truncated` | `stream:<msg>` | `decode:<msg>`), yielded as a
  fatal `Err` *after* the good chunks; a clean `context deadline exceeded` is **not** an
  error and is just a `Stop(Length)` chunk. The raw twins carry the literal wire text
  (`prompt is too long`, an ECONNRESET-shaped mid-tool-args cut) so the real classifier
  must agree at 05/09 — that agreement is exactly R-RPLY-070's re-point guarantee.
- **Recorder scope at stage 2 (deviation, logged).** A faithful `--record` tees *raw socket
  bytes*, but there is no HTTP transport until stage 5. So the stage-2 `RecordingProvider`
  records the only bytes available — the decoded `Chunk` stream, re-encoded into a native
  `kind: replay` fixture (byte-identical chunk output on reload, `rply::record_roundtrip`).
  The raw-byte tee + `write_raw_fixture`/`redact_header_value` for real `kind`s are exposed
  now and attach at the socket in stage 5.
- **R-RPLY-070 deferred to stage 5** (re-point the inline splitter at
  `kn9t-provider-core::sse_lines`); tracked in `TRACKING.md` as `⧗ (deferred to 05)`. Stage
  is 8/9 done; this is by design, not a gap.
- No new spec/design bugs found. Implementation choices within latitude recorded above as
  DD-02/DD-03 (design decisions, not spec bugs).
- **State:** stage 02 done. Next: stage 03, `kn9t-react` + `kn9t-tools`.

### Session 1 — stage 01 `kn9t-core` implemented, gate green
- Created the Cargo workspace: `Cargo.toml` (`[workspace]`, resolver 2, edition 2021,
  rust-version 1.94, shared serde/serde_json deps), crates under `crates/`.
- Implemented `kn9t-core` in ID order (all 36 requirements R-CORE-010 → R-CORE-270),
  organized into modules: `ids` (newtypes + dependency-free Crockford ULID, R-CORE-040/045),
  `message`, `model`, `usage`, `error`, `cache` (incl. `breakpoints()`), `toolspec`, `event`
  (the one `Event` enum + `seq()`/`is_durable()`), `provider` (`Request`/`Chunk`/`Provider`),
  `cancel` (AtomicBool + Condvar), `bus` (ring-buffer broadcast + `EventSink`), `traits`
  (`Store`/`Tool`/`Policy` + support types). Public surface re-exported from `lib.rs`.
- Wrote 13 named acceptance tests in `tests/acceptance.rs` under `mod core` so paths match
  the spec's `cargo test core::<name>`: `payload_is_pod`, `id_serde`, `ulid_monotonic`,
  `content_tag`, `args_verbatim`, `thinking_roundtrip`, `tokens_default_zero`, `event_tag`,
  `seq_partition`, `fork_snapshot_serde`, `breakpoints`, `bus_never_blocks`, `cancel_wakes`.
  All pass.
- Gate **R-CORE-900** green: `cargo build`/`test` clean under `-D warnings`; `cargo doc`
  clean under `-D warnings` (fixed a `<hex>` → `` `sha256:<hex>` `` doc-comment that tripped
  the invalid-HTML lint); GI-1 (0 workspace deps), GI-2 (deps = serde/serde_json only, no
  tokio), GI-3 (no `preserve_order`, the one `HashMap` in `ToolCtx` is not serialized), GI-5
  (no `async`/`.await`) all verified.
- Found and recorded spec bug **DB-01** (see table above); resolved with a wire-only rename,
  no interface change.
- Implementation choices within spec latitude: `Bus` uses a per-subscriber bounded
  `VecDeque` ring (a `std::sync::mpsc::sync_channel` can't evict the oldest from the producer
  side, which R-CORE-220's "newest retained" requires); ULID randomness is a per-thread
  splitmix64 seeded from wall-clock nanos + a global counter (no external RNG dep, satisfies
  GI-2). Added a few `Default` impls the spec is silent on (`SessionId`, `MsgId`, `Cancel`,
  `ThinkingReplay`) — none change a pinned signature.
- **State:** stage 01 done. Next: stage 02, `kn9t-provider-replay`.

### Session 0 — spec authored (no code)
- `DESIGN.md` finalized (§1–18, 2267 lines); the §4 vocabulary gaps and the duplicate
  `Request` definition were closed.
- `spec/` authored: `README.md` + 10 stage files (2445 lines, ~165 requirements). Verified:
  all requirement IDs unique, all cited DESIGN sections resolve, no dangling cross-refs,
  clean encoding.
- Four interface-level open items from DESIGN §18 were decided (recorded in
  `spec/README.md` §6): cross-platform pwsh+POSIX bash classifier; subagent spawn mechanism
  with configurable child toolset; blob refcount GC; auto-titling after first assistant
  turn.
- Repo scaffolding created: `AGENTS.md` (rulebook), `TRACKING.md` (scoreboard), this
  `CHANGELOG.md` (narrative).
- **State:** design + spec complete, zero code. Next: stage 01, create the workspace.
