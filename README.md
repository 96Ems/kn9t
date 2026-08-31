# kn9t

**Minimal, modular coding agent in Rust — OS threads, no async, events as the wire, the log, and the truth.**

> `kn9t` is a from-scratch coding agent built for correctness over cleverness: one vocabulary crate, one `Event` enum, one SQLite file, strict build order, and a dependency budget — not fewer features.

[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange)](https://www.rust-lang.org) [![License: MIT](https://img.shields.io/badge/license-MIT-blue)](Cargo.toml) [![Tests](https://img.shields.io/badge/tests-411_passed-brightgreen)](#development) [![Status](https://img.shields.io/badge/status-design_locked%20%E2%80%94%20stages_01%E2%80%9309_implemented-yellow)](#status)

---

## Table of contents

- [Principles](#principles)
- [Architecture](#architecture)
- [Crates](#crates)
- [Quick start](#quick-start)
- [Configuration](#configuration)
- [CLI](#cli)
- [TUI](#tui)
- [Server HTTP API](#server-http-api)
- [Providers & prompt caching](#providers--prompt-caching)
- [Plugins](#plugins)
- [Storage & sessions](#storage--sessions)
- [Development](#development)
- [Docs map](#docs-map)
- [Status](#status)
- [License](#license)

---

## Principles

From `DESIGN.md §1`, in priority order:

1. **Rust, OS threads, no async runtime** — ~6 modules exchanging messages with one active provider stream. `async` would infect every trait (`Pin<Box<dyn Future>>`) and buy nothing. Blocking I/O throughout.
2. **One vocabulary crate that knows nobody** — `kn9t-core` owns all types, all traits, and the bus. Depends on `serde` + `serde_json` only (`GI-2`). Every other crate depends on it and never on a sibling (except `kn9t-server`, `GI-1`).
3. **The bus carries facts; traits carry calls** — events are past-tense, fan out to N subscribers, get no replies, never block the publisher. Anything needing an answer is a `&dyn Trait` call. Cancellation is a shared `Cancel` token, never a message.
4. **Events are the wire, the log, and the truth** — one `Event` enum is simultaneously the SSE payload, the SQLite row, and the input to state reconstruction. Every event payload is pure `Serialize + Deserialize` data — no `Arc`, no handles, no closures.
5. **Minimal means a dependency budget, not fewer features** (`DESIGN §14`).

Global invariants enforced in CI:

| # | Rule |
|---|---|
| `GI-1` | No crate except `kn9t-server` has >1 workspace dep (`scripts/check-gi1.sh`) |
| `GI-2` | `kn9t-core` depends only on `serde`/`serde_json` |
| `GI-3` | No `HashMap` is serialized into a request/cached prefix (`preserve_order` off) |
| `GI-4` | `events` table is append-only; only `live_messages` is mutable |
| `GI-5` | No `tokio`, no `async fn`, no `.await` anywhere |
| `GI-6` | `kn9t-tui` does not depend on `kn9t-core` (HTTP + SSE only) |

All JSON uses `snake_case` for fields and enum variants (`#[serde(rename_all = "snake_case")]`, `AGENTS.md §12`).

---

## Architecture

### Bus and call topology

```
calls (value-returning, blocking)          durable (synchronous, never dropped)           facts (broadcast, no reply, droppable)
ReAct ──stream(req,cancel)──► Provider      ReAct ──append(Event)──► Store: BEGIN,        ReAct ──Event──► bus ──► SSE fan-out
      ──execute(args,ctx,cancel)──► Tool                                assign seq,                        ──► tracing/stderr
      ──check(call,cwd)──► Policy                                        write events+projections,           ──► plugin on_event
      ──plan_request()/append()──► Store                                 COMMIT ──► returns seq              (bounded queue, may drop)
```

* **Durable events** carry `seq` — assigned inside the `append()` transaction from `sessions.head_seq + 1` (gapless, correct under concurrent subagent writes). Written to SQLite, then fanned to the bus. Never dropped.
* **Transient events** (`TextDelta`, `ThinkingDelta`, `ToolArgsDelta`, `ToolProgress`, `TurnStarted/Ended`, `ApprovalRequest`, `HookFailed`, `Error`, …) — bus only, bounded queue, droppable. Self-healing: a missed `TextDelta` is covered by the following `MessageAppended`.

See `DESIGN.md §3`, `§5`.

### One `Event` enum — two tiers

```rust
pub enum Event {
    // durable — folding in seq order reconstructs the session exactly
    SessionForked   { seq: u64, fork: ForkSnapshot },
    MessageAppended { seq: u64, msg: Message },
    ModelChanged    { seq: u64, model: ModelRef },
    Compacted       { seq: u64, replaced: SeqRange, summary: Message },
    UsageRecorded   { seq: u64, provider: String, model: String,
                      kind: UsageKind, tokens: Tokens,
                      price_snapshot: Price, cost_usd: f64 },
    // transient — liveness only, never persisted
    TurnStarted { turn: u32 }, TextDelta { msg_id: MsgId, idx: u32, delta: String },
    ThinkingDelta { .. }, ToolArgsDelta { .. }, ToolStarted { .. },
    ToolProgress { .. }, ToolFinished { .. }, ApprovalRequest { .. },
    TurnEnded { .. }, HookFailed { .. }, TitleChanged { .. },
    Error { message: String }, PluginNotification { payload: Value },
}
```

`seq: Option<u64>` distinguishes the tiers (`Event::is_durable()`). Durable payloads are pure data; `args_json` is stored verbatim (never re-serialized) so the message-level cache stays valid (`DESIGN §4.1`).

### Sessions are linear; the tree is at the session level

Every divergence — `/fork`, `/tree`, `rewind`, subagent spawn — creates a **new session row** (`fork_reason = Fork | Rewind | Subagent | Tree`). No lanes, no `parent_seq`, no `navigateTree`. State reconstruction is `fold(events WHERE session_id=? ORDER BY seq)`.

Fork copies `MessageAppended`/`ModelChanged`/`Compacted` (renumbered), **never** `UsageRecorded` (no double-billing). `SessionForked` at `seq 0` snapshots `inherited_cost_usd`, `inherited_ctx_tokens`, `budget_remaining_usd`, etc. — analytics without JSON extraction (`DESIGN §7`).

### Storage

Single `~/.kn9t/kn9t.db`, WAL mode (many readers, one writer). `events` is append-only and canonical; `messages`/`usage` are **projections** — every byte recomputable. `blobs` is content-addressed (`sha256:` refs, not inline bytes). `live_messages` is a display cache for mid-stream attach, outside the event sourcing model.

* **Cost resolved at write time** — `usage.cost_usd` + `price_*_snapshot` are frozen on `append()`. Query-time recomputation would let price changes mutate history.
* **`kn9t reproject`** — drops projections, replays every event through the same `project()` fn the writer uses, rebuilds. `reproject --check` diffs live vs. freshly projected; any diff is a writer/projector bug. Auto-runs on `PROJECTION_VERSION` mismatch at startup (`DESIGN §6.2`).

```
sessions ──1:N──► events ──► messages (projection)
                        ──► usage    (projection)
         ──► sessions (self-FK: origin_session)
         ──► blobs (sha256 PK)
         ──► live_messages (non-canonical)
         ──► plugin_kv (plugin persistent KV)
         ──► meta (PROJECTION_VERSION)
```

---

## Crates

Workspace at `Cargo.toml` (`resolver = "2"`, `edition = "2021"`, `rust-version = "1.94"`).

| Crate | Role | Depends on |
|---|---|---|
| `kn9t-core` | Vocabulary — `Event`, `Message`/`Content`, `ModelSpec`, `Provider`/`Tool`/`Store`/`Policy` traits, `Bus`, `Cancel`, `breakpoints()` | `serde`, `serde_json` only |
| `kn9t-provider-replay` | Raw-byte fixtures through the real parser — offline tests, no network/spend | `kn9t-core` |
| `kn9t-react` | ReAct loop + 8 hooks, cancel/abort, parallel-read / call-order persist, compaction re-plan (once) | `kn9t-core` |
| `kn9t-store` | SQLite (`events` + projections + `blobs` + `plugin_kv` + `live_messages`), `plan_request()`, `reproject` | `kn9t-core` |
| `kn9t-provider-core` | Blocking HTTP/TLS (`ureq`), `sse_lines`, `assemble`, retry (pre-stream only), `Cache` encoding scaffold | `kn9t-core` |
| `kn9t-provider-openai` | OpenAI-compat + LiteLLM gateway (quirks, dual-field usage, cache, adaptive thinking) | `kn9t-provider-core` |
| `kn9t-server` | HTTP (`tiny_http`), SSE, leases, auth, spawn, compaction decision | all of the above |
| `kn9t-tui` | `ratatui` client — **links no workspace crate** (`GI-6`, HTTP + SSE only) | `ratatui`, `crossterm`, `ureq`, `arboard`, … |
| `kn9t-plugin` | Subprocess stdio host, `RemoteTool`/`RemoteProvider`, 8 hooks, subagent spawn | `kn9t-core` |
| `kn9t-plugin-sdk` | SDK for external plugins (`Tool`/`Provider` traits, `SseReader`, `KvClient`, blocking) — zero workspace deps | — |
| `kn9t` | Launcher — `~/.kn9t/` bootstrap, server ensure, TUI/CLI dispatch | `serde_json`, `crossterm` only (`GI-1`) |

Plugins (not workspace members):

| Plugin | Lang | Role |
|---|---|---|
| `plugins/kn9t-custom-provider` | Rust | External OpenAI-compat provider (6 documented hazards) |
| `plugins/kn9t-agents-md` | Go | Auto-discovers & injects `AGENTS.md` (KV-backed, survives restarts) |
| `plugins/kn9t-mcp` | Python | Bridges 4 MCP servers (TeamForge, Jira, Confluence, TestKub) — 148 hidden tools, lazy discovery |
| `crates/kn9t-plugin-sdk` docs + `internal-plugins/kn9t-tools` (now `plugins/`-adjacent) | Rust | Default tools `bash`/`read`/`edit`/`write` as subprocess plugin |

Build order is strict `01 → 10` (`DESIGN §16`, `AGENTS.md §3`). Gates `G1` (loop vs replay), `G2` (`kill -9` + `reproject --check`), `G3` (3 TUIs / 1 server / 1 lease) are hard stops.

---

## Quick start

### Prerequisites

* Rust **1.94+** (`rustup update`)
* On **Windows**, run cargo through `cmd.exe /c` from WSL/bash — there is no Linux `cargo` in this repo's toolchain (see `AGENTS.md §8.1`):

  ```bash
  cmd.exe /c "cargo build --workspace"
  cmd.exe /c "cargo test --workspace"
  ```

### Clone & build

```bash
git clone https://github.com/96Ems/kn9t
cd kn9t
cargo build          # workspace: kn9t + kn9t-server + kn9t-tui + core libs
# or, on Windows/WSL:
cmd.exe /c "cargo build --workspace"
```

The first run of any `kn9t` binary bootstraps `~/.kn9t/` automatically (config template + `token` + `port`, `crates/kn9t/src/bootstrap.rs`).

### Run

```bash
# TUI (default) — auto-starts kn9t-server if needed
cargo run -p kn9t
# or directly:
./target/debug/kn9t

# The launcher writes:
#   ~/.kn9t/config.toml  — provider & model config (edit this!)
#   ~/.kn9t/token         — Bearer token (auto-generated UUID v4)
#   ~/.kn9t/port          — server port
#   ~/.kn9t/kn9t.db       — SQLite store (WAL)
#   ~/.kn9t/server.log    — server log
```

Logs: `tail -f ~/.kn9t/server.log`. No `tokio`, no `.await` — every `cargo tree` is auditable.

---

## Configuration

`~/.kn9t/config.toml` is created on first run from a commented template. Minimal example:

```toml
# ── Anthropic direct (bundled plugin) ──
[[provider]]
id     = "anthropic"
kind   = "plugin"
binary = "kn9t-anthropic"          # bare name → resolved next to kn9t-server

[provider.anthropic.env]
ANTHROPIC_API_KEY = "sk-ant-..."

[[model]]
provider = "anthropic"
id       = "claude-sonnet-4-6"
label    = "Sonnet 4.6"
default  = true
ctx      = 200000
max_out  = 64000
price_in  = 3.0
price_out = 15.0
price_cache_read  = 0.30
price_cache_write = 3.75

[model.quirks]
thinking_replay = "verbatim"       # strip | verbatim

# ── OpenAI-compatible gateway (e.g. LiteLLM) ──
[[provider]]
id       = "my-gateway"
kind     = "openai"
base_url = "https://llm-gateway.example.com/v1"

[provider.my-gateway.headers]
X-User-Id = "env:GATEWAY_USER_ID"

[[model]]
provider = "my-gateway"
id       = "bedrock-sonnet-4"
ctx      = 200000
price_in  = 3.0
price_out = 15.0

# ── Custom provider (external plugin, absolute path) ──
[[provider]]
id     = "custom"
kind   = "plugin"
binary = "/home/you/plugins/kn9t-custom-provider/target/debug/kn9t-custom-provider"

[provider.custom.env]
CUSTOM_API_KEY = "env:CUSTOM_KEY"

# ── Server ──
[server]
port           = 0        # 0 = random free port
idle_exit_secs = 5        # grace after last client disconnects; 0 = disable
log            = "server.log"

# ── Quirks (per-provider, overridable per-model) ──
[provider.my-gateway.quirks]
max_tokens_field = "max_tokens"        # | "max_completion_tokens"
system_role      = "system"            # | "developer"
usage_in_stream  = true
finish_reason    = true
reasoning        = "reasoning_effort"  # | "budget_tokens" | "none"

# Per-model override must be the LAST block of that [[model]]:
# [model.quirks]
# reasoning = "adaptive"
```

* `kind = "plugin"` vs `"openai"` — plugins are subprocess binaries; `binary` is bare (bundled, next to `kn9t-server`) or **absolute path** (external).
* Model prices are **hand-written** — no generated 400-model registry. Required so `cost_usd` freezes correctly at write time.
* `--dump-request` prints the exact built payload for quirk debugging.
* Prices use `Price { input, output, cache_read, cache_write }` (USD per 1M tokens).

See `DESIGN.md §8`, `spec/05-provider-core-openai.md`, `crates/kn9t/src/bootstrap.rs` for the full template.

---

## CLI

All commands go through the server — same path as the TUI. The launcher ensures the server is running before dispatching.

```
kn9t                          # TUI (auto-starts server, passes KN9T_URL/TOKEN)
kn9t chat [--model p/id] <prompt>   # one-shot: send prompt, stream, exit
kn9t chat --json <prompt>     # one-shot JSONL: stdout is one JSON per line (jq-parseable)
kn9t chat                     # REPL: new session, prompt loop (> ), Ctrl-D to exit
kn9t chat --continue          # REPL: attach to latest session (highest head_seq), resume
kn9t sessions                 # GET /session → table of sessions (id, name, model, age)
kn9t history [session-id]     # GET /session/{id} → full transcript (ANSI roles, tool cards)
kn9t history                  # latest session
kn9t attach [session-id]      # observe + write via lease (backoff retry, serialised)
kn9t attach                   # latest session
kn9t status                   # GET /health + models/sessions summary
kn9t models                   # GET /models → table
kn9t cost [--since MS] [--group-by model|kind|session]  # GET /cost + GET /budget
kn9t tools                    # GET /tools → registered tools
kn9t stop                     # POST /stop → graceful server shutdown
kn9t help | --help | -h       # help (no server start)
kn9t --version                # version
```

**REPL / attach** share `chat::repl_loop`: `> ` prompt, one line → `POST /session/{id}/prompt`, stream `TurnEnded`, re-prompt. Image paste in TUI sends `images: ["data:image/png;base64,..."]` — stored as `blobs` (`sha256:` refs), hydrated to data-URIs at `plan_request()` time.

**JSON mode** (`kn9t chat --json "prompt"`): autonomous-friendly. `stdout` is JSONL — one JSON object per line: `session` + `prompt` then raw SSE events (`text_delta`, `tool_started`, `tool_progress`, `tool_finished`, `message_appended`, `turn_status`, `turn_ended`, `approval_request`, `error`, … `snake_case` per `AGENTS.md §12`). `stderr` stays human. Broken-pipe safe (`| head` exits 0). `--format json|text` also accepted.

```bash
kn9t chat --json "list files" | jq -c 'select(.kind=="text_delta") | .delta'
kn9t chat --json "list files" | jq -s 'map(select(.kind=="tool_started"))'
```

**Approval flow** (when bash classifier asks): inline crossterm selector on `ApprovalRequest` (`[ No ] / [ Yes ]`, `←/→`, `Enter`), then `POST /approve {id, decision, scope}`. In `--json` mode the request still blocks on the selector (stderr) and emits `approval_decision` on stdout. See `PLAN.md P2-C`.

---

## TUI

`kn9t-tui` is the **API proving ground** — if the TUI needs something awkward, the server API is fixed, not worked around (`AGENTS.md §11`). No `PATCH` endpoints; action endpoints or full replacement instead.

* **Stack:** `ratatui` + `crossterm` 0.29 (patched for Windows VT bracketed paste via `[patch.crates-io]`), pure event-driven (blocks on `recv()`, zero polling).
* **Layout:** 3-column — left: session picker (hover-to-expand, `Today`/`Yesterday` grouping, filter, `Del` to delete, `✚ New session`), center: transcript (virtual scroll, last-50 + "load earlier", auto-scroll with `[u`/`]u` user-msg jumps, `[a`/`]a` assistant), right: context panel (model/cost/tokens via `TokenTracker`, tools toggle, git sidebar → diff viewer).
* **Transcript:** tool cards with 3 tabs — `Progress` (streaming chunks) | `Output` (final `ToolResult`) | `Input` (`args_json`); collapsible, lazy-load on expand.
* **Input:** `Enter` = send, `Shift/Ctrl/Alt-Enter` = newline; `Ctrl+Z`/`Ctrl+Shift+Z` undo/redo (coalesced 300 ms, 100 states); Emacs kill ring (`Ctrl+K/U/W/Y`, `Alt+Y`); `Ctrl+Left/Right`, `Ctrl+Backspace` word navigation (`unicode-segmentation`); prompt history `Up/Down` with prefix filter (500 entries, `~/.kn9t/prompt_history.json`); `/stash`/`/unstash`; `@path` file mentions with autocomplete; OSC 8 hyperlinks; LaTeX math → Unicode; CSI 2026 synchronized output (flicker-free).
* **Images:** `Ctrl+V` bracketed paste → `arboard` clipboard → `pending_images: Vec<String>` as `data:` URIs → `[imgN: WxH PNG]` marker at cursor → `POST /prompt {text, images}` → `blobs` table → `resolve_image_blobs()` → provider-ready `image_url` parts.
* **Diff viewer:** side-by-side or unified, file tree, mouse wheel/click, `n`/`p` file nav, `b` toggle, comment input (auto-wrap, 8 lines), cursor `▶` highlight.
* **Command palette:** `Ctrl+P` fuzzy search (`Navigation`/`Session`/`Edit`/`View`/`Tools`/`Settings`) + `/palette`.
* **Approval overlay:** blocking — cannot type until resolved.
* **Status bar:** animated braille spinner + rotating phrases while streaming; error cards persist in DB and are copyable.
* **Mouse:** full support (hover, click, drag selection, sidebar hover-to-expand).
* **Keybinds:** vim-style default, fully customizable via `[keybinds]` in config, leader key.
* **Theming:** auto light/dark + user color overrides.
* **Env:** `KN9T_URL`, `KN9T_TOKEN`, `KN9T_MODEL` (launcher sets the first two).

> `GI-6` — `kn9t-tui` never links `kn9t-core`. If it did, it would grow into a second wiring path (`interactive-mode.ts` 222 KB vs `rpc-mode.ts` 23 KB in the reference failure mode).

Detailed design: `docs/TUI-DESIGN.md`, spec `spec/07-tui.md` (`R-TUI-010` … `R-TUI-240`, `G3`).

---

## Server HTTP API

Base `http://127.0.0.1:<port>` (read from `~/.kn9t/port`), `Authorization: Bearer <token>` on every request (`~/.kn9t/token`). Requests with `Origin` are rejected `403` — use a native client, not browser `fetch()`.

**Lease:** single-writer per session. `POST /session/{id}/lease` → `{lease}`; pass `X-Lease: <holder>` on writes (`/prompt`, `/steer`, `/abort`, `/model`, `/approve`); `DELETE /session/{id}/lease`; `409 session_busy` if held, `?takeover=1` to steal. SSE never needs a lease.

Key routes (see `API.md` for the full reference — `API.md` is the intended generated output of the schema-first contract in `ADR-0005`; until then, the server is authoritative):

| Method | Route | Purpose |
|---|---|---|
| `POST` | `/session` | Create session `{cwd?, model{provider,id}?, name?}` → `{id}` |
| `GET` | `/session` | List sessions `{sessions:[{id,name,model,created_at,head_seq,cost_usd}]}` |
| `GET` | `/session/{id}` | Get transcript |
| `DELETE` | `/session/{id}` | Delete session (+ blob refcount) |
| `POST` | `/session/{id}/lease` | Acquire lease |
| `DELETE` | `/session/{id}/lease` | Release lease |
| `POST` | `/session/{id}/prompt` | Send prompt `{text, images?}` (requires lease) |
| `POST` | `/session/{id}/steer` | Steering message between tool batches |
| `POST` | `/session/{id}/abort` | Cancel current turn (`Cancel` token) |
| `POST` | `/session/{id}/approve` | Resolve `ApprovalRequest` `{id, decision, scope?}` |
| `GET` | `/session/{id}/events?since=seq` | SSE stream (replay from `seq`, deduplicated `seq` gap check) |
| `GET` | `/attach?session={id}` | SSE live attach (increments `attached_clients`, keepalive heartbeat) |
| `GET` | `/blob/{hash}` | Blob fetch (`Cache-Control: immutable`, `ETag`) |
| `GET` | `/models` | Model catalog |
| `POST` | `/cost` | Cost rollup query |
| `POST` | `/stop` | Graceful shutdown |
| `GET` | `/health`, `GET` | `/pref`, `POST /plugin/{name}/reload` | Misc / plugin hot-reload |

**SSE:** `Event` JSON with `{"kind":"snake_case_variant", …}` (`serde(tag = "kind")`), `seq` present iff durable. `live_messages` are sent on attach for mid-stream clients. Keepalive `sse::heartbeat_interval()` (default 15 s, `KN9T_SSE_HEARTBEAT_MS` override) — write failure detects dead clients for `idle-exit`.

**Idle-exit:** grace-on-last-disconnect (default 5 s, `[server] idle_exit_secs`, `0` disables) + keepalive ping sweep.

---

## Providers & prompt caching

Provider trait (`kn9t-core`):

```rust
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn stream(&self, req: &Request, cancel: &Cancel)
        -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr>;
}
pub enum Chunk { Text{idx,delta}, Thinking{idx,delta}, ToolCall{idx,id,name},
                 ToolArgs{idx,delta}, Usage(Usage), Stop(StopReason) }
```

* **Retry lives inside `stream()`** — before the first chunk only (connect + HTTP `429`/`5xx`). Mid-stream failure is a hard turn error; a decorator cannot retry after 300 deltas already hit the sink.
* **Quirks are config data, never URL-sniffed** — `max_tokens_field`, `system_role`, `reasoning`, `tool_result_name`, `usage_in_stream`, `finish_reason`, etc., declared explicitly per provider and overridable per model (`[model.quirks]` must be the last block of that `[[model]]`). See `DESIGN.md §8.2` table.
* **`kn9t-provider-core` owns the shared work** — `sse_lines`, `assemble` (verbatim `args_json` concat + `ToolCall` JSON parse at end), `with_retry`, `CancellableReader` (abort < 1 ms via `Cancel` check in `Read`). Providers implement only wire mapping (bidirectional encode + decode is ~400 lines before quirks; measured `openai 725`, `anthropic 547` — `DESIGN §2.1`).
* **Replay provider** (`kn9t-provider-replay`) — raw-byte fixtures captured via `--record`, replayed offline. Enables `G1` (full loop vs replay, no network/spend) and offline testing of truncation ladder, compaction, classifier.

### Prompt caching (§8.4)

The single largest cost lever. Four breakpoints, two anchored and two sliding — priority order, not positional:

```rust
pub enum Cache { System, AfterMessage(usize) }
pub enum CacheMode { Explicit{max_breakpoints:u8, min_tokens:u32}, Automatic, None }

pub fn breakpoints(messages: &[Message], mode: &CacheMode) -> Vec<Cache> {
    // candidates in priority order: [System, last_user, second_last, last]
    // deduplicate, cap at max_breakpoints
}
```

```
turn 2   [Sys ①] [User ②] [Asst] [Tool ③④] [Asst …]
turn 3   [Sys ①] [User ②] [Asst] [Tool ③ ] [Asst] [Tool ④] [Asst …]
turn 4   [Sys ①] [User ②] [Asst] [Tool   ] [Asst] [Tool ③] [Asst] [Tool ④] …
```

* `System` is the stable anchor (~25 K tokens) — written once per session, read every call. Nothing normal invalidates it (not conversation growth, not compaction). Two hazards that do: editing the system `.md` mid-session, or unstable `tools` serialization (`HashMap` random seed) which invalidates all three hierarchy levels at once (`GI-3`).
* Rolling pair `③`/`④` progressively caches the tool loop transcript.
* Prefix hierarchy `tools → system → messages` — a change at one level invalidates only that level and below.
* Billing is tiered: `cost = input*price_in + cache_read*price_cache_read (0.1x) + cache_write*price_cache_write (1.25x) + output*price_out`; context is `input + cache_read + cache_write`. Mixing them overcharges 10x.
* Per-model `min_tokens` (512 .. 4096), 20-block lookback, 5-min TTL (request-start, not response-end).

Providers encode breakpoints differently: Anthropic/Bedrock = message-level `cache_control: {type:"ephemeral"}`, OpenAI = `Automatic` (server-side, omit fields or 400), custom plugin = part-level.

---

## Plugins

Subprocess stdio, **not** `dylib` — isolation.

* **Host** (`kn9t-plugin`): spawns binaries, handshake (`hello` with `ToolSpec`s + hook list), dispatches hook calls as JSON over `stdin`/`stdout`, enforces per-hook timeouts, applies failure posture (3 `HookFailed` → unsubscribe), channels `PluginMsg::Event` → `EventBus` → SSE.
* **SDK** (`kn9t-plugin-sdk`): zero workspace deps. `Tool`/`Provider` traits, `SseReader`, `KvClient` (persistent `plugin_kv` table, `kv_get`/`kv_set`/`kv_del`/`kv_del_scope` via `PluginKv` trait on `SqliteStore`), `ChunkSender`/`CancelToken`, blocking throughout (`GI-5`). Wire: `HostMsg` / `PluginMsg` with `chunk`/`done`/`cancel`/`KvGet`/`KvSet`… streaming, ordering guaranteed.
* **Tool declaration:** `ToolSpec { name, description, schema: Value (hand-written json!()), parallel_safe, hidden }`. `hidden` enables lazy discovery — hidden tools are registered but not sent in the system prompt ( `visible_specs()` ), discoverable via meta-tools.

```
plugin ──hello──► host ──register tools──► ToolRegistry
        ◄──hook_call──  (before_tool_call, after_tool_call, before_request, … ×8)
        ──chunk/done──► host ──emit ToolProgress / assemble──► loop
        ──cancel──► host listener thread (never blocked by dispatch)
        ──KvGet/Set──► host reader thread ──► SqliteStore.plugin_kv ──► KvResult
```

**Authoring a plugin** (Rust example):

```rust
use kn9t_plugin_sdk::{Tool, ToolCtx, ToolOutput, Price, wire::ToolSpec};
use serde_json::{json, Value};

struct MyTool;
impl Tool for MyTool {
    fn spec(&self) -> ToolSpec { ToolSpec {
        name: "my_tool".into(), description: "does X".into(),
        schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
        parallel_safe: true, hidden: false,
    }}
    fn execute(&self, args: &Value, ctx: &ToolCtx, cancel: &kn9t_plugin_sdk::CancelToken) -> Result<ToolOutput, String> {
        ctx.progress.send("starting…"); // → ToolProgress SSE
        if cancel.is_cancelled() { return Err("cancelled".into()); }
        // ctx.kv.get("my-scope","key") / .set(...) — persistent across restarts
        Ok(ToolOutput { content: vec![], is_error: false })
    }
}
```

**Bundled vs external:**

* **Bundled** — `binary = "kn9t-anthropic"` (next to `kn9t-server`, sibling resolution). Stays bundled so both distribution paths are exercised.
* **External** — `binary = "/abs/path/to/binary"` (e.g. `plugins/kn9t-custom-provider/target/debug/kn9t-custom-provider`). Built outside the workspace (`[workspace]` in its own `Cargo.toml`), path dep on `kn9t-plugin-sdk` via `../../crates/kn9t-plugin-sdk`. `scripts/check-gi1.sh` scans `plugins/*/Cargo.toml` too.

Included plugins: `kn9t-agents-md` (Go, `after_tool_call` + `get_steering`, KV-backed `injected` set — never re-injects after restart), `kn9t-mcp` (Python, 2 visible meta-tools + 148 hidden MCP tools), `kn9t-custom-provider` (6 hazards documented in `spec/09a`), `kn9t-anthropic` (bundled).

See `spec/08-plugin.md`, `spec/08b-plugin-redesign.md`, `spec/09-anthropic.md`, `spec/09a-custom-provider.md`, `docs/adr/0001`..`0005`.

---

## Storage & sessions

* **Compaction** (`Store::plan_request` decides, `ReAct` executes): at `threshold × ctx_window` (default `0.80`), summarize oldest span via one extra provider call (`UsageKind::Compaction`), `append(UsageRecorded)` + `append(Compacted{replaced, summary})`, then `plan_request()` exactly once more — a second `compact: Some(..)` is a hard error, not a loop (billing guard). Never splits `ToolCall` from `ToolResult` (400 on orphan).

  ```rust
  pub trait Store: Send + Sync {
      fn plan_request(&self, session: &SessionId) -> Result<RequestPlan, StoreErr>;
      fn append(&self, session: &SessionId, event: Event) -> Result<u64, StoreErr>; // assigns seq in txn
      fn snapshot(&self, session: &SessionId) -> Result<SessionSnapshot, StoreErr>;
  }
  ```

* **Context accounting — no tokenizer:** true prompt size = last provider-reported `input_tokens` + `Σ len/4` for messages added since (estimate only the delta, not the whole window).

* **`kn9t cost` / `cost_rollup`:** `sum(cost_usd) GROUP BY model`; fork-aware (`inherited_cost_usd` + marginal `sum(cost_usd) WHERE session_id=X` + recursive family rollup).

* **Images:** `blobs {hash PK, mime, bytes_len, bytes}` — `Content::Image { sha256: "sha256:<hex>", mime }` in events; `resolve_image_blobs()` hydrates to `data:<mime>;base64,…` right before provider encode.

---

## Development

### Build order — never deviate

```
01 kn9t-core            types, Event, bus, all traits, breakpoints()
02 kn9t-provider-replay  raw-byte fixtures through the real parser  [offline tests]
03 kn9t-react + tools    loop, cancel/abort, read/write/edit/bash   [GATE G1]
04 kn9t-store            SQLite schema, projections, reproject        [GATE G2]
05 provider-core+openai  http/sse/assemble/retry, openai, litellm gateway
06 kn9t-server           http surface, SSE, leases, auth, spawn
07 kn9t-tui              ratatui client, links no workspace crate     [GATE G3]
08 kn9t-plugin           stdio host, 8 hooks, subagent spawn
08b kn9t-plugin-sdk + internal-plugins/kn9t-tools  SDK + default tools plugin
09 custom-provider + anthropic   external plugin (6 hazards), anthropic
10 bedrock-native + gemini       SigV4/eventstream, gemini            [v2]
```

Per-stage spec in `spec/NN-*.md`; `spec/README.md` holds ID scheme, keywords (`MUST`/`SHOULD`/`MAY`), and `SPEC-OPEN` interim values. **Build requirement-by-requirement in ID order; each MUST's named `cargo test <name>` must pass before the gate is green** (`AGENTS.md §4`, `§6`).

### Running tests

```bash
# Workspace
cmd.exe /c "cargo test --workspace"          # 385 passed (Windows)
# or
cargo test --workspace

# External plugin (standalone, not a workspace member)
cd plugins/kn9t-custom-provider && cargo test  # 26 passed

# Single crate
cmd.exe /c "cargo test -p kn9t-store -- stor::reproject_check_clean"
cmd.exe /c "cargo test -p kn9t-plugin -- plug::composition"

# Invariants
bash scripts/check-gi1.sh                    # GI-1 gate
cargo tree -p kn9t-tui | grep kn9t           # GI-6: must print nothing
grep -r "tokio\|async fn\|\.await" crates/   # GI-5
```

* A gate is not green until a real `cargo test` says so — `grep`/structural checks are never sufficient (`AGENTS.md §8.1`).
* `cargo check` on Windows **must** go through `cmd.exe /c`; calling `/mnt/c/.../cargo.exe` from WSL hands a `\\wsl.localhost\…` UNC path and misbehaves.

### Project layout

```
kn9t/
├─ Cargo.toml                    # [workspace] — 11 members
├─ Cargo.lock
├─ crates/
│  ├─ kn9t/                      # launcher (bootstrap + CLI dispatch)
│  ├─ kn9t-core/
│  ├─ kn9t-provider-{replay,core,openai}/
│  ├─ kn9t-react/
│  ├─ kn9t-store/
│  ├─ kn9t-server/
│  ├─ kn9t-tui/
│  ├─ kn9t-plugin/  +  kn9t-plugin-sdk/
├─ plugins/
│  ├─ kn9t-custom-provider/      # external Rust plugin (standalone [workspace])
│  ├─ kn9t-agents-md/            # Go plugin
│  └─ kn9t-mcp/                  # Python plugin
├─ spec/                         # stage specs 01..10 + README + SPEC-OPEN register
├─ docs/
│  ├─ adr/ 0001..0005             # ADRs (classifier, effects, dry-run, discovery, schema-first)
│  ├─ TUI-DESIGN.md  TUI_IMPROVEMENTS.md
├─ job/                          # phase index for the 4-phase cleanup (tracking.md + phase0..5)
├─ scripts/  check-gi1.sh
├─ AGENTS.md  DESIGN.md  API.md  CONTEXT.md  TRACKING.md  CHANGELOG.md  PLAN.md
└─ kn9t.toml.example
```

### How to implement a stage

1. Open `TRACKING.md` — confirm the previous gate is green.
2. Read `spec/NN-*.md` + `spec/README.md` (global invariants `GI-1`..`GI-6`).
3. Create crate(s) under `crates/` in the workspace.
4. Implement requirement-by-requirement (`R-<AREA>-<NNN>`); signatures/DDL/wire schemas are `MUST` — match exactly.
5. Write the named acceptance test; it must pass.
6. Run the stage gate (`R-*-900`).
7. Flip `TRACKING.md` and append `CHANGELOG.md`.

See `AGENTS.md §4`, `§6`, `§7` (gates are hard stops).

---

## Docs map

| Doc | What it is | When to read |
|---|---|---|
| `AGENTS.md` | Repo rulebook — how to proceed, rules, invariants, gates | Every session, first |
| `TRACKING.md` | Live scoreboard — stage progress, per-requirement test status, `SPEC-OPEN` register | Every session, second |
| `CHANGELOG.md` | Session narrative + discovered spec/design bugs | Every session; append as you work |
| `DESIGN.md` | The *why* — 18 sections, decisions + rejected alternatives + accepted costs | When a requirement is unclear |
| `spec/README.md` | Spec conventions — ID scheme, keywords, invariants, `SPEC-OPEN` | Before any stage |
| `spec/NN-*.md` | The *what* and *how* — per-stage requirements, DDL, wire schemas, acceptance tests | When implementing that stage |
| `API.md` | Server HTTP API reference (client implementors) | Building a client |
| `CONTEXT.md` | Domain glossary (27 terms, `DESIGN` pointers) | Lookup |
| `docs/adr/0001`..`0005` | ADRs — classifier in server, effects, dry-run, discovery, schema-first | Architecture changes |
| `docs/TUI-*.md` | TUI design + improvements roadmap | TUI work |
| `PLAN.md` | Post-v1 plan (`P1` bootstrap → `P6` instant-cut abort, incl. `CancellableReader` design) | Sequencing |
| `job/tracking.md` + `phase0`..`phase5` | 4-phase cleanup index (findings, decisions, per-step dispatch) | Parallel agent dispatch |

Rule of precedence: if spec and design disagree, **design wins, spec is the bug** — stop and flag it.

---

## Status

Live status lives in `TRACKING.md`; narrative in `CHANGELOG.md`. `AGENTS.md` never holds status.

| Stage | Crate(s) | Reqs | Gate | Status |
|---|---|---|---|---|
| 01 | `kn9t-core` | 36/36 | `R-CORE-900` | ☑ |
| 02 | `kn9t-provider-replay` | 8/9 | `R-RPLY-900` | ☑ |
| 03 | `kn9t-react` + tools plugin | 20/25 | `G1` | **✗** — bash classifier deleted in `5b65819` (`R-TOOL-070/080/090/095`, see `ADR-0001`, `job/findings.md`) |
| 04 | `kn9t-store` | 18/18 | `G2` | ☑ (`kill -9` + `reproject --check` clean, no tokenizer) |
| 05 | `kn9t-provider-core` + `-openai` | 22/22 | — | ☑ |
| 06 | `kn9t-server` | 13/13 | `R-SRV-900` | ☑ |
| 07 | `kn9t-tui` | 2/27 | `G3` | ▣ (7.3 K lines, 58 tests; most `R-TUI-*` have no test; manual gate deferred) |
| 08 | `kn9t-plugin` | 13/13 | `R-PLUG-900` | ☑ |
| 08b | `kn9t-plugin-sdk` + tools plugin v2 | 12/12 | `R-PLUG2-900` | ☑ |
| 09 | `kn9t-custom-provider` (external) + `kn9t-anthropic` (bundled) | 16/16 | `R-CP/ANTH-900` | ☑ |
| 10 | `bedrock-native` + `gemini` | 0/8 | — | ☐ v2 |

`cargo test --workspace` — **385 passed, 0 failed** (verified `2026-08-30` via `cmd.exe /c "cargo test --workspace"`). External `plugins/kn9t-custom-provider` — **26 passed** (`cd plugins/kn9t-custom-provider && cargo test`). Total **411**. `v1 e2e` (`kn9t chat` → server → ReAct → tools + custom-provider) verified `2026-08-27`.

`PLAN.md` post-v1 `P1`–`P4` complete (`P1-A` bootstrap, `P2-A` one-shot, `P2-B` REPL + `--continue`, `P2-C` approval, `P3-A` `sessions`, `P3-B` `history`, `P3-C` `attach`, `P4-A` real-subprocess plugin tests). `P5-A` `G3` deferred (needs human at terminal: 3 TUIs + screenshot). Architecture cleanup `2026-08-28` (`GI-1` enforced via `check-gi1.sh`, TUI manager composition, SSE dedup); `2026-08-30` review filed 5 ADRs + 4-phase cleanup (`job/`). Next: **Phase 1 — restore classifier in `kn9t-server`, wire `InteractivePolicy`, parse `[policy]`** (`ADR-0001` → `ADR-0002` effects).

`SPEC-OPEN` interim values (TTL, truncation ladder, compaction threshold `0.80×ctx`, lease 5 min, …) in `spec/README.md §7` + `TRACKING.md` register.

---

## License

MIT — see `Cargo.toml` (`[workspace.package] license = "MIT"`). No `LICENSE` file is shipped in this revision; the workspace declaration is authoritative.

---

*Built from `DESIGN.md` (18 sections) and `spec/` (stages 01–10). If you touch code, read `AGENTS.md` first, `TRACKING.md` second, then only the spec for your stage.*
