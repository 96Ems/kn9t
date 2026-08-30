# CONTEXT — kn9t Domain Glossary

Lookup reference for domain terms used across DESIGN.md, CHANGELOG.md, and the spec.
Each entry points to its authoritative DESIGN section. Alphabetical order.

---

| Term | Definition | DESIGN ref |
|------|------------|------------|
| **approval** | A blocking decision request emitted when a tool call requires user consent (e.g., `bash rm -rf`). `Event::ApprovalRequest` pauses the loop; `/approve` resolves it. The loop never polls — it blocks on a condvar. | §10 |
| **blob** | Content-addressed binary (images) stored via SHA-256 hash in the `blobs` table. Messages carry `sha256:` refs, not inline bytes. Clients `GET /blob/{hash}` with `Cache-Control: immutable`. | §12.7 |
| **breakpoint** | A cache breakpoint position in the request prefix. Placed at system prompt, last user message, and a rolling pair (second-to-last, last message). Provider-independent; placement is data on `Request.cache`. | §8.4 |
| **compaction** | Summarizing the oldest messages when context nears the model's window limit. Store decides via `plan_request`; the ReAct loop executes exactly one re-plan. A second `compact: Some(..)` is a hard error. | §7.5 |
| **effect** | *(Proposed, ADR-0002)* A declared side-effect kind on a tool argument: `Shell`, `FsWrite`, `FsRead`, `Network`. The server maps effects to checkers (Shell → classifier, FsWrite → path rules). | ADR-0002 |
| **event (durable)** | Carries `seq`; written to SQLite's `events` table in a transaction that also updates projections. Folding durable events reconstructs session state exactly. Examples: `MessageAppended`, `UsageRecorded`. | §5.1 |
| **event (transient)** | No `seq`; broadcast on the bus only, droppable if queue is full. Self-healing: missing deltas are covered by the next durable event. Examples: `TextDelta`, `ToolProgress`. | §5.1 |
| **fork reason** | Why a session was derived: `Fork` (explicit), `Rewind` (backtrack), `Subagent` (spawn), `Tree` (branching). Stored in `sessions.fork_reason`; carried by `SessionForked` at seq 0. | §7.3 |
| **head_seq** | The highest `seq` in a session's event log. Assigned inside the append transaction from `sessions.head_seq + 1`. Used for attach-from-seq and fork-copy. | §3.1, §6 |
| **hook** | One of 8 extension points invoked synchronously by the ReAct loop: `before_tool_call` (fail closed), `after_tool_call`, `before_request`, `should_stop_after_turn`, `prepare_next_turn`, `get_steering`, `get_followup`, `get_api_key`. | §13.3 |
| **idle-exit** | Server exits after a grace period (default 5 s) once all SSE clients disconnect and no turn is running. Configurable via `[server] idle_exit_secs`; `0` disables. Detected via SSE keepalive write failures. | §12.2 |
| **lease** | Single-writer token for a session. One client holds the write lease (`prompt`, `steer`, `abort`, `approve`); others get `409 session_busy`. Released on disconnect, timeout, or explicit `DELETE`. Stolen with `?takeover=1`. | §12.6 |
| **live_messages** | Display cache for mid-stream attach. Updated as deltas arrive; deleted when `MessageAppended` finalizes. Not canonical — ignored by `reproject`, truncated on startup. | §12.3 |
| **plugin host** | The server-side subprocess manager. Spawns plugins, runs the handshake, dispatches hook calls via stdio JSON, enforces per-hook timeouts, and applies failure postures. | §13.1–13.2 |
| **policy** | Trait with `check(call, cwd) -> Decision`. Two impls: `ConfigPolicy` (TOML rules, instant), `InteractivePolicy` (emits `ApprovalRequest`, blocks on condvar). Loop is unaware which is wired. | §10 |
| **projection** | Derived tables (`messages`, `usage`) recomputed from `events` by `reproject`. Schema changes become version bumps, not migrations. `reproject --check` diffs live vs. freshly projected. | §6.2 |
| **provider** | Trait `stream(req, cancel) -> impl Iterator<Item=Chunk>`. Owns HTTP, SSE decoding, retry (pre-stream only), and quirk encoding. Never invoked outside the ReAct loop. | §8 |
| **replay fixture** | Raw provider bytes captured via `--record`, replayed by `ReplayProvider`. Enables offline testing of the full loop (truncation ladder, compaction, classifier) with no network or spend. | §16, Q22 |
| **reproject** | CLI/startup that drops projection tables, replays all events through the projector, and rebuilds. Lets bug fixes and schema changes apply retroactively. | §6.2 |
| **seq** | Monotonic sequence number assigned in the append transaction. Durable events carry `seq`; transient events do not. Gaps are impossible (single source of truth in `sessions.head_seq`). | §3.1 |
| **session** | A linear event log. Every divergence (fork, rewind, subagent) creates a new session row — no lanes, no `parent_seq`, no in-session branching. State = `fold(events ORDER BY seq)`. | §7 |
| **spawn / subagent** | A child session created via `ForkReason::Subagent`. Runs its own ReAct loop with a restricted toolset and budget cap. Returns a summary as `ToolResult` to the parent. | §7, §13 |
| **steering** | User-injected messages appended between tool batches. `POST /steer` queues; `get_steering` hook collects from plugins. Host-queue items always precede plugin items. | §9, §13.3 |
| **tool** | Trait `execute(args, ctx, cancel) -> ToolOutput`. Registry is ordered; `ToolSpec` has hand-written JSON Schema. Default tools (`bash`, `read`, `edit`) ship as a subprocess plugin. | §11 |
| **turn** | One ReAct iteration: plan → stream → assemble → (tool calls → results) → hooks → next turn or idle. `TurnStarted`/`TurnEnded` bracket it. A `Cancel` is scoped to exactly one turn. | §9 |
