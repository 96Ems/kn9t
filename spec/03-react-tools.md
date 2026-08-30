# 03 — `kn9t-react` + `kn9t-tools`

**Crates:** `kn9t-react`, `kn9t-tools`
**Depends on:** each on `kn9t-core` only (GI-1). `kn9t-react` sees the store/provider/tool/
policy only as `dyn Trait` (GI-1, §2).
**DESIGN:** §9, §9.1, §10, §10.1, §11, §11.1, §11.2, §8.6.6.
**Build order:** stage 3 of 10. Gate **G1**: the full loop runs end-to-end against the
replay provider (02) with no network and no spend (§16).

Two crates, one file, because they are gated together by G1 and the loop is meaningless
without tools to call.

---

# Part A — `kn9t-react`

## A.1 Loop structure

> **R-RCT-010 → DESIGN §9**
> `kn9t-react` MUST expose a loop driver that owns only trait objects:
> ```rust
> pub struct ReactLoop {
>     pub provider: Arc<dyn Provider>,
>     pub store:    Arc<dyn Store>,
>     pub policy:   Arc<dyn Policy>,
>     pub tools:    ToolRegistry,          // R-TOOL-010
>     pub hooks:    Arc<dyn HookHost>,     // R-RCT-100
>     pub bus:      Arc<dyn EventSink>,
> }
> ```
> It MUST NOT name any concrete `Provider`, `Tool`, `Store`, or `Policy` type (GI-1). The
> core loop body is small (§9: ~40 lines); everything else is hooks and queues.

> **R-RCT-020 → DESIGN §9**
> One turn MUST execute this sequence, matching the §9 flowchart exactly:
> 1. `before_request` hook (pipeline, **fail open** — R-RCT-110).
> 2. `store.plan_request(session)` — compaction decided here (A.4).
> 3. If `plan.compact.is_some()`: run the compaction sub-turn (A.4) then re-plan **once**.
> 4. `provider.stream(req, cancel)`; feed chunks to `assemble` (PCORE), which emits
>    `TextDelta`/`ThinkingDelta`/`ToolArgsDelta` transient events.
> 5. `store.append(MessageAppended)` then `store.append(UsageRecorded{kind: Main})`.
> 6. If no tool calls: `should_stop_after_turn` hook (any-says-stop, fail open) → followup
>    queue (host items first) → either loop with the followup or emit `TurnEnded` and idle.
> 7. If tool calls: for each, `before_tool_call` hook (first-deny-wins, **fail closed**) →
>    `policy.check` → execute (parallel per A.6) → `after_tool_call` hook (pipeline, keep
>    original on failure). A denied/failed call yields a synthesized `is_error` ToolResult.
> 8. `store.append(MessageAppended)` for tool results **in the model's call order**
>    (R-TOOL-... A.6).
> 9. Steer queue (host items first) → `prepare_next_turn` hook (pipeline, no-change on
>    failure) → loop.
> **Accept:** `cargo test rct::turn_sequence` — against a replay fixture producing one tool
> call, assert the ordered event trace: `TurnStarted`, deltas, `MessageAppended`,
> `UsageRecorded`, `ToolStarted`, `ToolFinished`, `MessageAppended(toolresult)`.

> **R-RCT-030 → DESIGN §9**
> `TurnStarted { turn }` MUST be published at turn entry and `TurnEnded { turn, stop }` when
> the loop goes idle. `turn` is a per-session monotonic counter.

## A.2 Cancellation

> **R-RCT-040 → DESIGN §9.1, §3.2, R-CORE-240**
> The loop MUST create a fresh `Cancel` at the **start of each turn** and pass the same
> clone to `provider.stream` and every `tool.execute` in that turn. Cancellation MUST be
> checked only at loop boundaries — never inside a `store.append` — so no transaction is
> half-applied.
> **Accept:** `cargo test rct::cancel_boundary` — a cancel signalled mid-stream leaves the
> store with either the full `MessageAppended` or none, never a partial row.

> **R-RCT-050 → DESIGN §9.1**
> On abort **during the provider stream**, the loop MUST:
> - append `UsageRecorded` for tokens the provider already reported; if the stream was cut
>   before any usage arrived, append it with `estimated: true` (R-CORE-142) using the §7.4
>   estimate;
> - **NOT** append `MessageAppended` for the discarded partial assistant message.
> **Accept:** `cargo test rct::abort_in_stream`.

> **R-RCT-060 → DESIGN §9.1, §7.5**
> On abort **during tool execution**, the loop MUST:
> - keep the already-written assistant `MessageAppended`;
> - keep every `ToolResult` that completed;
> - synthesize `Content::ToolResult { is_error: true, "aborted by user" }` for each tool
>   call that never ran or never finished, so no `ToolCall` is left without its
>   `ToolResult` (the §7.5 invariant every provider 400s on).
> The loop MUST NOT roll back the turn (append-only log, GI-4; filesystem already mutated).
> **Accept:** `cargo test rct::abort_in_tools` — every emitted `ToolCall` id has a matching
> `ToolResult` id in the persisted transcript.

## A.3 Truncation retry policy

> **R-RCT-070 → DESIGN §8.6.6, §18.9**
> Truncation retry policy lives in the loop, not the provider (which is stateless). On
> `ProvErr::Truncated` the loop MUST maintain a per-session attempt counter and re-issue the
> turn with a progressively harsher write-size reminder injected as a system-role reminder
> message, up to a give-up threshold. Defaults (**SPEC-OPEN**, §18.9): **4 attempts**, line
> ladder **150 → 100 → 50 → 25 → 10**. On exhaustion the loop surfaces `Event::Error` and
> ends the turn.
> The static "chunk your writes" instruction is provider config (`system_prelude`, custom provider),
> not this dynamic ladder.
> **Accept:** `cargo test rct::truncation_ladder` — a fixture sequence returning `Truncated`
> three times then success drives exactly three reminders in the ladder order.

> **R-RCT-080 → DESIGN §8.6.6**
> `ProvErr::ContextOverflow` MUST trigger a compaction re-plan (A.4), not a truncation
> retry. `StopReason::Length` from a clean `context deadline exceeded` is a normal turn end,
> not an error.

## A.4 Compaction execution

> **R-RCT-090 → DESIGN §7.5, R-CORE-250**
> When `plan_request` returns `compact: Some(span)`, the loop MUST:
> 1. issue a provider call over `span.messages` with the compaction prompt (interim
>    template lives in STOR/04; wording **SPEC-OPEN** §18.1);
> 2. `store.append(UsageRecorded { kind: Compaction })`;
> 3. `store.append(Compacted { replaced: span.replaced, summary })`;
> 4. call `plan_request` **exactly once more**.
> If the second plan still returns `compact: Some(..)`, the loop MUST emit `Event::Error`
> and abort the turn — it MUST NOT loop (each pass is a paid call; §7.5).
> **Accept:** `cargo test rct::compaction_replan_once` — a store stub that returns
> `compact: Some` twice causes exactly one summarize call then a hard error, never two.

> **R-RCT-095 → DESIGN §7.5**
> The compaction summarize call MUST use `UsageKind::Compaction` and MUST NOT be attributed
> as `Main`. The loop is the only component that calls a provider or emits `UsageRecorded`
> (§3); the store never does.

## A.5 Hook host integration

> **R-RCT-100 → DESIGN §13, §13.3**
> `kn9t-react` MUST invoke hooks through a `HookHost` trait. The trait, `HookVeto`, and
> `NextTurnPatch` are defined in **`kn9t-core`** and re-exported by `kn9t-react`, so both
> `kn9t-react` and `kn9t-plugin` depend only on `kn9t-core` (GI-1; see R-PLUG-010). The
> subprocess implementation lives in PLUG/08. Signatures:
> ```rust
> pub trait HookHost: Send + Sync {
>     fn before_tool_call(&self, tool: &str, args: &Value, cwd: &Path) -> HookVeto;
>     fn after_tool_call(&self, tool: &str, args: &Value, result: Vec<Content>) -> Vec<Content>;
>     fn before_request(&self, msgs: Vec<Message>, model: &ModelRef, system: Option<&str>) -> Vec<Message>;
>     fn should_stop_after_turn(&self, stop: StopReason, usage: &Usage, turn: u32) -> bool;
>     fn prepare_next_turn(&self, stop: StopReason, usage: &Usage) -> NextTurnPatch;
>     fn get_steering(&self) -> Vec<Message>;
>     fn get_followup(&self) -> Vec<Message>;
>     fn get_api_key(&self, provider: &str) -> Option<String>;
> }
> pub enum HookVeto { Allow, Deny(String), Replace(Value) }
> pub struct NextTurnPatch { pub model: Option<ModelRef>, pub thinking: Option<Thinking> }
> ```
> A no-op `HookHost` MUST be provided so the loop runs with zero plugins configured.

> **R-RCT-110 → DESIGN §13.5**
> The loop MUST apply the per-hook failure posture on timeout/crash:
> `before_tool_call` → **deny (fail closed)**; `before_request`, `after_tool_call`,
> `prepare_next_turn` → use original / no change (fail open); `should_stop_after_turn` →
> continue; `get_steering`/`get_followup`/`get_api_key` → empty/null. Every failure MUST
> publish `Event::HookFailed { plugin, hook, reason }`.
> **Accept:** `cargo test rct::hook_posture` — a HookHost stub that panics per hook produces
> the specified fallback and a `HookFailed` event each time.

> **R-RCT-120 → DESIGN §9, §13.4**
> Steering and followup queues MUST place **host-supplied items ahead of plugin items**
> (`get_steering`/`get_followup` results are concatenated after the host queue), so a plugin
> cannot starve or reorder client-issued steering.

## A.6 Parallel tool execution

> **R-RCT-130 → DESIGN §11.2**
> Tools whose `parallel_safe()` is `true` (v1: `read` only) MAY run concurrently within one
> tool batch on OS threads. Tools with `parallel_safe() == false` (`write`, `edit`, `bash`)
> MUST run sequentially. Results MUST be persisted in the order the model emitted the calls,
> regardless of completion order, so transcripts are deterministic (replay/reproject).
> **Accept:** `cargo test rct::parallel_order` — two reads (one artificially slow) plus a
> write persist in call order, not completion order.

---

# Part B — `kn9t-tools`

## B.1 Registry

> **R-TOOL-010 → DESIGN §11, §8.4.2.1, GI-3**
> The tool registry MUST be ordered (`Vec<Arc<dyn Tool>>` or `BTreeMap<String, ...>`), never
> a `HashMap`, so the serialized tools array is byte-stable across processes (level-1 cache,
> §8.4.2.1). Lookup by name is required; iteration order MUST be deterministic.
> ```rust
> pub struct ToolRegistry(Vec<Arc<dyn Tool>>);
> impl ToolRegistry {
>     pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>>;
>     pub fn specs(&self) -> Vec<ToolSpec>;   // stable order
> }
> ```
> **Accept:** `cargo test tool::spec_order_stable` — `specs()` returns identical ordering
> across two process runs (asserted via a golden vector).

> **R-TOOL-020 → DESIGN §11**
> v1 tool set MUST be exactly: `read`, `write`, `edit`, `bash`. Schemas are hand-written
> `serde_json::json!({...})` literals — no `schemars`, no derive (§11 dep budget). A schema
> object MUST have stable key order (GI-3).

> **R-TOOL-030 → DESIGN §11**
> `ToolOutput.content` (model-visible) MUST be truncated to a configured cap; `details`
> (UI/DB) holds the full result. Truncation applies **only** to `content`.
> **Accept:** `cargo test tool::truncation` — a result exceeding the cap sends truncated
> `content` and full `details`.

## B.2 `read`

> **R-TOOL-040 → DESIGN §11, §11.1, §11.2**
> `read` MUST be `parallel_safe() == true`. On each read it MUST record `(path → (sha256,
> mtime))` into `ToolCtx.read`, holding the mutex **only** for the insert, never across the
> file I/O (§11.2). It returns file contents (offset/limit supported) as `Content::Text`,
> or an image as a blob ref (POST to the blob store happens server-side; the tool returns
> `Content::Image { sha256, mime }` when the file is an image and the server accepts it).
> **Accept:** `cargo test tool::read_records_hash`.

## B.3 `write` / `edit` staleness guard

> **R-TOOL-050 → DESIGN §11.1**
> `edit` MUST require all three, in order:
> 1. the path was `read` in this session, else `ToolErr("read the file first")`;
> 2. the current on-disk hash equals the recorded hash, else `ToolErr("file changed on disk
>    since you read it; re-read before editing")`;
> 3. `old_string` occurs **exactly once**, else `ToolErr("N matches, need unique context")`.
> On success it MUST update the recorded hash so consecutive edits work without re-reading.
> `edit` is `parallel_safe() == false`.
> **Accept:** `cargo test tool::edit_guard` — three cases, one per failure, plus the
> consecutive-edit success path.

> **R-TOOL-060 → DESIGN §11.1**
> `write` to an **existing** path is subject to guard rules 1 and 2 (read-first, hash-match).
> `write` to a **new** path is not. `write` is `parallel_safe() == false`.
> **Accept:** `cargo test tool::write_guard`.

## B.4 `bash` and the command classifier

> **R-TOOL-070 → DESIGN §10.1, §18.6**
> `bash` MUST NOT self-authorize; it emits its command through `policy.check` and executes
> only on `Allow`. It is `parallel_safe() == false`. Cancellation (`Cancel`) MUST `kill` the
> child process when signalled.

> **R-TOOL-080 → DESIGN §10.1, §18.6 (decision: cross-platform, README §6)**
> The command classifier MUST support **two shell grammars**, selected at runtime by the
> configured/detected shell:
> - **POSIX** (`sh`/`bash`): segment separators `;`, `&&`, `||`, `|`, newline; command
>   substitution `$(...)` and backticks; subshell `(...)`; redirections `>`,`>>`,`<`,`>|`;
>   `sh -c`/`bash -c` string execution.
> - **PowerShell** (`pwsh`/`powershell`): statement separators `;` and newline; pipeline
>   `|`; `&&`/`||` (PS7 pipeline chain operators); subexpression `$(...)` and `@(...)`;
>   redirections `>`,`>>`,`2>`,`*>`; `Invoke-Expression`/`iex`; `-Command`/`-c` string
>   execution; call operator `&`.
> The two grammars share the same **decision pipeline** (R-TOOL-090); only tokenization
> differs.
> ```rust
> pub enum Shell { Posix, PowerShell }
> pub fn classify(cmd: &str, shell: Shell, policy: &BashPolicy) -> Classification;
> pub enum Classification { AllowReadOnly, Ask, HardDeny(String) }
> ```
> **Accept:** `cargo test tool::classify_posix` and `tool::classify_pwsh` — parallel case
> tables; each asserts the mutating-form of an otherwise-read command (`cat x > y`,
> `Get-Content x > y`) resolves to `Ask`.

> **R-TOOL-090 → DESIGN §10.1**
> The classifier decision pipeline MUST evaluate in this exact order (both grammars):
> 1. split into segments; if **any** segment's `argv[0]` is absent from `allow_read` →
>    `Ask`;
> 2. any redirection / `tee` / `dd` / command substitution / subshell present → `Ask`
>    (`cat x > y` is a write);
> 3. in-place flags (`sed -i`, `perl -i`, `awk` redirect; PS equivalents) → `Ask`;
> 4. `git`/`cargo`/`npm` with a subcommand outside `allow_read_sub` → `Ask`;
> 5. `argv[0]` in `always_ask` → `Ask` (this is where every interpreter — `sh`, `bash`,
>    `pwsh`, `python`, `node`, `perl`, `iex`, `Invoke-Expression` — is listed, closing the
>    `sh -c 'rm -rf /'` / `iex '...'` bypass);
> 6. `argv[0]` matches `never` → `HardDeny` (not presented as an approval prompt);
> 7. otherwise → `AllowReadOnly`.
> The `[policy.bash]` config shape (`allow_read`, `always_ask`, `never`,
> `allow_read_sub`) is exactly DESIGN §10.1's TOML.
> **Accept:** `cargo test tool::classify_pipeline` — one case per numbered rule, both
> grammars; plus the `sh -c` and `iex` bypass cases resolve to `Ask`.

> **R-TOOL-095 → DESIGN §10.1**
> The classifier is documented as a **heuristic, not a sandbox** (§10.1). This requirement
> exists so no downstream code treats `AllowReadOnly` as a security guarantee; real
> isolation is a container and out of scope.

---

## Stage gate G1

> **R-RCT-900 / R-TOOL-900 → DESIGN §16 gate G1**
> Stage 3 is **done** when: `cargo test -p kn9t-react -p kn9t-tools` is green; **the full
> ReAct loop runs end-to-end against `ReplayProvider` (02) with no network and no spend**,
> executing at least one tool call and one compaction re-plan; both classifier grammars pass
> their case tables; abort-in-stream and abort-in-tools leave a consistent transcript; and
> GI-1/GI-3/GI-5 hold for both crates.
