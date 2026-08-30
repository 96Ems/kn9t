# 08 — `kn9t-plugin`

**Crate:** `kn9t-plugin`
**Depends on:** `kn9t-core` (GI-1). It implements the `HookHost` trait defined in
`kn9t-react` (R-RCT-100) — but to keep GI-1, the `HookHost` trait actually lives in
`kn9t-core` and `kn9t-react` re-exports it. *(Correction to 03: `HookHost`, `HookVeto`,
`NextTurnPatch` are defined in `kn9t-core` so both `kn9t-react` and `kn9t-plugin` can name
them with a single workspace dep. See R-PLUG-010.)*
**DESIGN:** §13, §13.1–13.6, §18.2.
**Build order:** stage 8 of 10.

A plugin is any executable speaking newline-delimited JSON over stdio — the same codec as
the HTTP layer (§13). A plugin is effectively another client that may additionally answer
hooks.

---

## 1. Trait placement correction

> **R-PLUG-010 → DESIGN §13, GI-1**
> `HookHost`, `HookVeto`, and `NextTurnPatch` (introduced in 03/R-RCT-100) MUST be defined
> in `kn9t-core`, not `kn9t-react`, so that `kn9t-react` and `kn9t-plugin` each depend only
> on `kn9t-core` (GI-1). `kn9t-react` re-exports them for ergonomics. This supersedes the
> placement implied in 03.
> **Accept:** CI GI-1 check passes for both `kn9t-react` and `kn9t-plugin`.

## 2. Why subprocess

> **R-PLUG-020 → DESIGN §13.1**
> Plugins MUST be subprocesses, not dynamic libraries. Rust has no stable ABI (§13.1: cdylib
> struct layout/vtables are UB across rustc; two allocators; a panic across the boundary is
> UB). WASM is rejected (~80 crates, §13.1). Subprocess gives crash isolation, timeouts, and
> any-language plugins.

> **R-PLUG-030 → DESIGN §13.1, §13.6**
> Because a hook call costs ~1 ms (spawn + IPC), **no per-delta hook may ever be exposed**
> (§13.1). The hook surface (R-PLUG-060) contains no per-token/per-delta hook.

## 3. Codec and handshake

> **R-PLUG-040 → DESIGN §13, §13.2**
> The wire codec MUST be newline-delimited JSON over the plugin's stdin/stdout. On spawn the
> host MUST send `{"t":"hello","proto":1,"kn9t":"<version>"}`; the plugin MUST reply with its
> declaration:
> ```json
> {"t":"hello","name":"redact","hooks":["after_tool_call"],
>  "tools":[{"name":..,"description":..,"schema":..}],
>  "events":["MessageAppended"]}
> ```
> The host MUST then register one `RemoteTool` per declared tool and subscribe `on_event`
> per declared filter. Hook invocations are `{"t":"hook","id":N,"hook":..,"payload":..}` →
> `{"t":"result","id":N,"action":..,..}`; events are fire-and-forget `{"t":"event",..}`;
> shutdown is `{"t":"shutdown"}`.
> **Accept:** `cargo test plug::handshake` — a stub plugin process completes the handshake
> and registers its declared tools.

> **R-PLUG-050 → DESIGN §13.2**
> A declared tool MUST be exposed to the ReAct loop as a `Tool` (a `RemoteTool` whose
> `execute` round-trips over stdio). It defaults `parallel_safe() == false` unless the plugin
> declares otherwise.

## 4. Hook surface

> **R-PLUG-060 → DESIGN §13.3**
> The host MUST support exactly these eight hooks plus the `on_event` subscription, with the
> stated reply, composition, and failure posture:
>
> | hook | reply | composition | on timeout/crash |
> |---|---|---|---|
> | `before_tool_call` | `allow`/`deny{reason}`/`replace{args}` | first-deny-wins, short-circuit | **deny (fail closed)** |
> | `after_tool_call` | `keep`/`replace{content}` | pipeline | keep original |
> | `before_request` | `keep`/`replace{messages}` | pipeline | use original |
> | `should_stop_after_turn` | `continue`/`stop` | any-says-stop | continue |
> | `prepare_next_turn` | `keep`/`patch{model?,thinking?}` | pipeline | no change |
> | `get_steering` | `[]` or messages | concat, **host queue first** | empty |
> | `get_followup` | `[]` or messages | concat, **host queue first** | empty |
> | `get_api_key` | `null` or key | first non-null | fall back to config/env |
> | `on_event` | *(no reply)* | all subscribers | drop; unsubscribe after 3 consecutive failures |
>
> `on_event` is a bus subscription over IPC, not a hook (§13.3). The failure postures MUST
> match RCT R-RCT-110 exactly (that is where the loop applies them). `HookName`
> (R-CORE-155) has one variant per hook; `on_event` is absent from it.
> **Accept:** `cargo test plug::hook_surface` — each hook round-trips its reply shape; a
> plugin that never answers triggers the specified posture and a `HookFailed` event.

> **R-PLUG-065 → DESIGN §13.3**
> Two Pi hooks MUST NOT exist: `convertToLlm` (no declaration-merged custom message types
> here; §13.3) and `transform_context` (indistinguishable from `before_request`; §13.3).

## 5. Composition

> **R-PLUG-070 → DESIGN §13.4**
> Plugins run in **config-declared order**. Three composition classes MUST be implemented:
> **pipeline** (B sees A's output: `before_request`, `after_tool_call`, `prepare_next_turn`);
> **veto** (first `deny` wins and short-circuits: `before_tool_call`); **collect** (concat in
> declared order, host-queue items always ahead of plugin items: `get_steering`,
> `get_followup`).
> **Accept:** `cargo test plug::composition` — two stub plugins verify each class, including
> host-queue-first ordering (a plugin cannot reorder client steering).

## 6. Timeouts

> **R-PLUG-080 → DESIGN §13.6**
> Per-hook timeouts MUST be configurable per plugin, with these defaults (ms):
> `before_tool_call 30000` (may block on a human in the plugin's own UI), `after_tool_call
> 2000`, `before_request 2000`, `should_stop_after_turn 1000`, `prepare_next_turn 1000`,
> `get_steering 500`, `get_followup 500` (both polled every turn — tight budget),
> `get_api_key 5000` (may do an OAuth refresh), `on_event 0` (fire-and-forget, never
> awaited). A hook exceeding its timeout is treated as a failure per R-PLUG-060.
> **Accept:** `cargo test plug::timeout` — a slow hook is cut at its budget and the posture
> applies.

> **R-PLUG-090 → DESIGN §13.3, §13.5, §18 plugin-unsubscribe**
> After **3 consecutive** `on_event` delivery failures a plugin MUST be unsubscribed from
> events (**SPEC-OPEN** count §13.3). Every hook failure MUST emit
> `Event::HookFailed{plugin,hook,reason}` so a degraded plugin is visible, not silent
> (§13.5).

## 7. Privilege

> **R-PLUG-100 → DESIGN §14, §13**
> `[[plugin]]` entries MUST be honored **only** from the global config
> (`~/.kn9t/config.toml`), never from a project-local `.kn9t.toml` (§14: a repo-committed
> file must not run arbitrary binaries; `git clone` then `kn9t` must not be code execution).
> A `[[plugin]]` in a project file MUST be ignored with a warning.
> **Accept:** `cargo test plug::project_plugin_ignored`.

## 8. Subagent spawn (decision: mechanism specified, subset configurable)

> **R-PLUG-110 → DESIGN §18.2, §7 (decision: README §6)**
> A built-in `spawn` tool MUST create a **new session** (session-per-subagent, §7) whose
> origin is the current session at the current seq, `fork_reason = subagent` (R-STOR-130).
> The parent thread blocks while the child runs its own ReAct loop; there is no shared
> mutable state between them (§7.1). The child's result is returned to the parent as a
> `Content::ToolResult` (§7 diagram: "summary returned as ToolResult").
> ```json
> // spawn tool schema (hand-written, GI-3-stable):
> {"name":"spawn","description":"...","schema":{
>   "type":"object",
>   "properties":{
>     "task":{"type":"string"},
>     "model":{"type":"string"},          // optional; defaults to parent model
>     "budget_usd":{"type":"number"}      // optional; capped by parent remaining
>   },
>   "required":["task"]}}
> ```
> **Accept:** `cargo test plug::spawn_session` — spawn creates a `subagent` session, the
> child transcript is isolated, and the summary returns as a `ToolResult`.

> **R-PLUG-120 → DESIGN §18.2, §7.3 (decision: configurable subset)**
> The child's allowed tool set MUST be a **config list** with **no hardcoded default**
> (README §6). Config key: `[subagent].tools = [...]`. If unset, the child inherits the
> parent's tool set. *(This is the decision made for the spec: the design left the default
> subset open; the spec makes it explicitly configurable rather than freezing a subset.)*
> **Accept:** `cargo test plug::spawn_toolset` — a configured subset restricts the child; an
> unset list inherits the parent set.

> **R-PLUG-130 → DESIGN §7.3, §18.2 (decision: budget cap)**
> The child MUST enforce a budget cap from `min(budget_usd argument, parent
> budget_remaining_usd)` captured in the `ForkSnapshot` (R-CORE-160), without querying
> ancestors at runtime (§7.3). Exceeding the cap ends the child turn with an error result
> returned to the parent.
> **Accept:** `cargo test plug::spawn_budget` — a child hitting its cap stops and reports,
> and its spend is attributed to the child session (not double-counted, §7.2).

## 9. Stage gate

> **R-PLUG-900 → DESIGN §13, §18.2**
> Stage 8 is **done** when: the handshake registers tools and event subscriptions; all eight
> hooks round-trip with correct composition and failure posture (matching RCT R-RCT-110);
> the dropped hooks are absent; timeouts cut at budget; `HookFailed` is emitted on every
> failure and `on_event` unsubscribes after 3 failures; project-local `[[plugin]]` is
> ignored; and the `spawn` tool creates isolated subagent sessions with configurable tools
> and an enforced budget cap. GI-1 holds.
