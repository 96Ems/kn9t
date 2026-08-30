# PLAN — kn9t next steps

Sequenced implementation plan. Each item is a self-contained unit of work with clear
acceptance criteria. Read `TRACKING.md` for current gate status before starting any item.

---

## P1 — Bootstrap (do first — unblocks daily use)

### P1-A: `~/.kn9t/config.toml` auto-create on first run

On first launch, if `~/.kn9t/config.toml` does not exist:
- Create `~/.kn9t/` directory.
- Write a commented template config with a `kind = "plugin"` provider block, token
  placeholder, and default model entry.
- Generate a random `~/.kn9t/token` (UUID) for server auth if none exists.
- Print a one-time setup message explaining what was created and what the user must fill in.

The server must not crash on a missing or empty config — it already starts without
providers; bootstrap just ensures the file exists so the user has something to edit.

**Files:** `crates/kn9t/src/bootstrap.rs` (new), called from `ensure_server()` in
`main.rs` before the server is launched.

**Accept:** `kn9t` run from a clean home with no `~/.kn9t/` creates the directory, config
template, and token without error.

---

## P2 — `kn9t chat` modes

Two distinct modes, both go through the server (same path as the TUI).

### P2-A: One-shot mode (current, keep as-is)

```
kn9t chat <prompt>
kn9t chat --model provider/id <prompt>
```

Sends one prompt, streams the response, exits. Good for scripting and automation.
No stdin loop. Already working — just ensure it stays clean as REPL is added.

### P2-B: REPL mode

```
kn9t chat              ← no prompt → enter REPL
kn9t chat --continue   ← attach to latest session, enter REPL
```

Behaviour:
- If no prompt words given: create a new session, enter the REPL loop.
- `--continue` (no id required): query `GET /session`, pick the session with the highest
  `head_seq` (most recently active). Retry `POST /session/{id}/lease` with backoff until
  granted (no takeover — another client may be mid-turn). Attach SSE, enter REPL.
- REPL loop: print `> ` prompt, read one line from stdin, send as `POST /session/{id}/prompt`,
  stream events until `TurnEnded`, print `> ` again. Repeat.
- `Ctrl-D` (EOF on stdin) sends `DELETE /session/{id}/lease` and exits cleanly.
- Empty line is ignored (re-prompts without sending).

**Files:** `crates/kn9t/src/chat.rs` — add `repl_loop(session_id, port, auth)`, called
from `run()` when no prompt words detected or `--continue` passed.

**Accept:** `kn9t chat` enters REPL, sends two sequential prompts, both stream correctly,
`Ctrl-D` exits without error. `kn9t chat --continue` resumes the most recent session's
transcript context.

### P2-C: Approval flow

`ApprovalRequest` SSE events are currently silently dropped. The server blocks the turn
waiting for `POST /approve`. 

Behaviour:
- On `ApprovalRequest` event: flush current text output, print to stderr:
  ```
  [approval] bash
             $ rm -rf /tmp/foo

           ❯ [ No  ]   [ Yes ]
             ←/→ to choose · Enter to confirm
  ```
- Render an inline selector. `←`/`→` (or `h`/`l`) moves the highlight. Default: `No`.
- `Enter` confirms. No `y/n` text input — fully keyboard-navigated selector.
- On confirm: `POST /approve { "id": "...", "decision": "allow"|"deny" }`.
- Resume SSE streaming.

**Implementation note:** the CLI is not a full TUI, so use `crossterm` for raw mode
during the selector only (enter raw, draw the two options, exit raw on confirm).
`crossterm` is already a dependency of `kn9t-tui`; add it to `kn9t/Cargo.toml`.

**Constraint:** SSE reader is on a background thread. Use a `mpsc` channel to signal the
main thread to handle approval, pause SSE forwarding, then resume.

**Files:** `crates/kn9t/src/chat.rs` — `handle_approval()`.

**Accept:** a tool call that triggers `ApprovalRequest` pauses the stream, prompts, and
resumes correctly on both allow and deny.

---

## P3 — Session management subcommands

All read-only except `attach` which acquires a lease.

### P3-A: `kn9t sessions`

```
kn9t sessions
```

`GET /session` → print a table to stdout:

```
ID                          NAME          MODEL                                    AGE
01M112X1QJQWMJDSMZFDGMGHPP  (unnamed)     anthropic::claude-sonnet-4-6-latest      2m ago
01M113135ZF5ZMJXK5G8BF4SB3  hello-world   anthropic::claude-sonnet-4-6-latest      5m ago
```

**Files:** `crates/kn9t/src/cmd_sessions.rs` (new).

### P3-B: `kn9t history <session-id>`

```
kn9t history <id>
kn9t history           ← uses latest session
```

`GET /session/{id}` → print the full transcript to stdout: user messages, assistant text,
tool calls (name + args), tool results (truncated at 40 lines). Same display helpers as
`chat.rs`.

**Files:** `crates/kn9t/src/cmd_history.rs` (new).

### P3-C: `kn9t attach [session-id]`

```
kn9t attach            ← latest session
kn9t attach <id>       ← specific session
```

Equivalent to `kn9t chat --continue` against a specific or latest session. Opens SSE
immediately (no lease needed to observe). Acquires the write lease with silent backoff
retry before each prompt send, releases it after `TurnEnded`. This is the "second client
attaches to running session" path — the primary multi-client use case.

**Files:** dispatch in `main.rs`, implementation reuses `chat::repl_loop()`.

**Accept:** two terminals both run `kn9t attach` against the same session; both see all
events in real time; prompts from either client are serialised transparently.

---

## P4 — Stage 08 test debt (R-PLUG-900)

Four acceptance tests were marked done on stub implementations. Fix them honestly.

### P4-A: `plug::composition` and `plug::timeout`

Build a `kn9t-test-plugin` binary under `crates/internal-plugins/kn9t-test-plugin/`
using `kn9t-plugin-sdk`. It accepts env vars to configure its behaviour:
- `TEST_PLUGIN_HOOK` — which hook to implement.
- `TEST_PLUGIN_REPLY` — JSON to reply with.
- `TEST_PLUGIN_SLEEP_MS` — sleep before replying (for timeout test).

Rewrite `plug::composition` and `plug::timeout` to spawn this binary via
`PluginHost::spawn()` with appropriate env vars. No more in-process channel stubs.

**Files:** `crates/internal-plugins/kn9t-test-plugin/` (new),
`crates/kn9t-plugin/tests/acceptance.rs`.

**Accept:** `cargo test plug::composition plug::timeout` — both pass against real
subprocess binaries.

### P4-B: `spawn` built-in tool (R-PLUG-110/120/130)

Implement the `spawn` tool in `kn9t-react`: creates a child session (`fork_reason =
subagent`), runs a nested ReAct loop on a blocking OS thread, returns the child's final
message as a `ToolResult` to the parent. Enforces `budget_usd` cap from `ForkSnapshot`.

Then rewrite `plug::spawn_session`, `plug::spawn_toolset`, `plug::spawn_budget` against
the real implementation.

**Files:** `crates/kn9t-react/src/spawn_tool.rs` (new), `crates/kn9t-plugin/tests/acceptance.rs`.

**Accept:** `cargo test plug::spawn_session plug::spawn_toolset plug::spawn_budget`.

---

## P5 — G3 gate (stage 07 TUI)

### P5-A: G3 gate run

Before any TUI redesign, run G3 manually to know what is actually broken:
1. `cargo build` all binaries.
2. Start one server.
3. Open 3 TUI instances against the same session.
4. Verify lease handoff (only one TUI can write at a time).
5. Paste a screenshot image — verify it renders in a Kitty-capable terminal.
6. Record what passes, what fails.

**Accept:** written report in `CHANGELOG.md` of exactly what G3 state is.

### P5-B: TUI redesign (design-first) — COMPLETED

**Design session completed 2026-08-27.** See `docs/TUI-DESIGN.md` for full spec.

Summary of key decisions:
- **Architecture:** ratatui + crossterm, pure event-driven (block on recv, zero polling)
- **Layout:** 3-column (left sidebar sessions, transcript, right sidebar context)
- **Sidebars:** hover-to-expand, configurable on/off
- **Mouse:** full support (hover, click, drag selection)
- **Input:** `Enter` = send, `Shift/Ctrl/Alt-Enter` = newline
- **Approval:** blocking overlay, cannot type until resolved
- **Scroll:** auto-scroll with escape, `[u`/`]u` jump user msgs, `[a`/`]a` assistant
- **Tool cards:** collapsible, lazy-load data on expand
- **Virtual scroll:** last 50 messages, "load earlier" button
- **Keybinds:** vim-style default, fully customizable, leader key support
- **Theming:** auto light/dark + user color overrides
- **Git:** right sidebar shows branch/changes, click file → side-by-side diff viewer with comments
- **Spinner:** animated braille + rotating fun phrases (configurable)
- **Errors:** inline cards, persisted in DB, copyable
- **File mentions:** `@path` syntax with autocomplete
- **Images:** placeholder only, send base64 to model

Spec file: `spec/07-tui.md` (25 requirements, R-TUI-010 through R-TUI-900).

### P5-C: TUI API compliance (R-TUI-012)

TUI wire types MUST match the server API exactly. `API.md` is the documentation;
the server is authoritative if they disagree.

Key constraints:
- SSE events use `#[serde(tag = "kind")]` discriminator (not `"type"`)
- Event variant names are PascalCase (`TextDelta`, `MessageAppended`, etc.)
- Lease response reads `body["lease"]` (not `body["holder"]`)
- Only process `KeyEventKind::Press` to avoid double-character input bug

If `API.md` is found to be inaccurate, update `API.md` to match actual server behavior.

**Accept:** TUI connects, receives SSE stream, displays streaming text and tool cards.

---

## Sequence

```
P1-A  bootstrap config          ← immediate, 1-2h
P2-A  one-shot (already done)   ← verify stays clean
P2-B  REPL + --continue         ← 2-3h
P2-C  approval flow             ← 2h
P3-A  kn9t sessions             ← 1h
P3-B  kn9t history              ← 1h
P3-C  kn9t attach               ← 1h (reuses REPL)
P4-A  composition + timeout     ← 2h (test binary)
P5-A  G3 gate run               ← manual, 30min
P5-B  TUI mockup + redesign     ← design session first, then impl (large)
P4-B  spawn tool                ← large, after P4-A
```

P1 → P2 → P3 can run in sequence immediately.
P4-A and P5-A are independent of P2/P3 and can be done in parallel.
P5-B and P4-B are the large items — plan them separately when the smaller items are done.

---

## Design decisions (resolved)

| # | decision |
|---|---|
| Q-A | `--continue` picks the session with the highest `head_seq` — most recently active, not most recently created. |
| Q-B | Multiple clients on one session is a first-class use case. All clients see server events in real time via SSE (read-only, never lease-gated). Lease acquisition is transparent — client retries `POST /session/{id}/lease` with exponential backoff (max ~2s) until granted, silently. Server serialises writes. No conflict prompt exposed to the user. **No server changes needed** — this is a client-side retry policy in `chat.rs` and the TUI. |
| Q-C | `Enter` sends the prompt. `Shift+Enter` inserts a newline (multiline input). |
| Q-D | Approval uses an arrow-key selector rendered inline — `[ Yes ]  [ No ]` with highlight, `←`/`→` or `↑`/`↓` to move, `Enter` to confirm. No `y/n` text input. Default highlight is `No` (safe). |
| Q-E | TUI session switching: left sidebar (hover-to-expand), instant switch, turn continues in background. |
| Q-F | TUI: `ratatui` for v1 (spec-mandated). Native GUI (egui/tauri) is a separate v2 decision. |
| Q-G | TUI right sidebar: context panel with model/cost/tokens, tools toggle, git status. Click file → diff viewer. |
| Q-H | TUI diff viewer: side-by-side, click any line to comment, comments append to input on close. |
| Q-I | TUI theming: auto light/dark detection + user CSS-like color overrides in config. |
| Q-J | TUI keybinds: vim-style default, fully customizable via `[keybinds]` in config, leader key support. |
| Q-K | TUI virtual scroll: last 50 messages on load, "load earlier" button, lazy-load tool data on expand. |

---

## P6 — Instant-cut abort

**Problem:** User presses ESC → sees "Aborting..." but the current tool/LLM call finishes
before actually stopping. Current design checks `Cancel` only at loop boundaries, but
blocking I/O (`read()` on socket, subprocess pipe) continues until natural completion.

**Goal:** < 1ms abort — ESC kills the stream/subprocess immediately.

### Design decision: CancellableReader

The original idea was to `shutdown()` the TCP socket directly, but ureq v3 doesn't expose
the underlying `TcpStream`. The chosen solution wraps the response body reader:

```
Cancel fired → CancellableReader.read() checks flag → returns Err(Interrupted)
            → sse_lines sees EOF → assemble() terminates → loop sees AbortedInStream
```

**Latency:** < 1ms in practice (SSE lines are ~100 bytes; next `read()` arrives in µs).

### P6-A: CancellableReader in kn9t-provider-core

**File:** `crates/kn9t-provider-core/src/abort.rs` (new)

```rust
use kn9t_core::Cancel;
use std::io::{self, Read};

/// Wraps a `Read` stream; returns `Err(Interrupted)` when cancelled.
pub struct CancellableReader<R> {
    inner: R,
    cancel: Cancel,
}

impl<R> CancellableReader<R> {
    pub fn new(inner: R, cancel: Cancel) -> Self {
        Self { inner, cancel }
    }
}

impl<R: Read> Read for CancellableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        self.inner.read(buf)
    }
}
```

**Also:** re-export from `lib.rs`.

**Accept:** `cargo test pcore::cancel_reader_interrupts`

### P6-B: http.rs accepts Cancel

**File:** `crates/kn9t-provider-core/src/http.rs`

Change `send()` signature:

```rust
pub fn send(
    req: HttpRequest,
    connect_timeout: Duration,
    cancel: Option<Cancel>,   // ← ADD
) -> Result<HttpResponse, ProvErr> {
    // ... existing code ...
    let body_reader = resp.into_body().into_reader();
    let body: Box<dyn Read + Send> = match cancel {
        Some(c) => Box::new(CancellableReader::new(body_reader, c)),
        None    => Box::new(body_reader),
    };
    Ok(HttpResponse { status, headers, body })
}
```

Update `send_get()` similarly if used.

**Accept:** existing tests still pass.

### P6-C: OpenAiProvider passes cancel

**File:** `crates/kn9t-provider-openai/src/provider.rs`

```rust
fn attempt(&self, req: &Request<'_>, model_ref: ModelRef, cancel: Cancel) -> ... {
    let resp = send(http_req, timeout, Some(cancel))?;
    // ...
}

impl Provider for OpenAiProvider {
    fn stream(&self, req: &Request, cancel: &Cancel) -> ... {
        let cancel = cancel.clone();  // ← use instead of _cancel
        with_retry(3, Backoff::default(), || {
            self.attempt(req, model_ref.clone(), cancel.clone())
        })
    }
}
```

**Also:** add cancel check in `with_retry()` loop to avoid retrying a cancelled request.

**Accept:** `cargo test` on kn9t-provider-openai.

### P6-D: PluginHost polling with cancel

**File:** `crates/kn9t-plugin/src/host.rs`

Add `wait_for_streaming_cancellable()`:

```rust
pub fn wait_for_streaming_cancellable(
    &self,
    expected_id: u64,
    cancel: &Cancel,
    timeout: Duration,
    mut on_chunk: impl FnMut(serde_json::Value),
) -> Result<Value, String> {
    let rx = self.response_rx.lock().unwrap();
    let deadline = std::time::Instant::now() + timeout;
    
    loop {
        if cancel.cancelled() {
            self.cancel_call(expected_id);  // send HostMsg::Cancel to plugin
            return Err("cancelled".to_string());
        }
        
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("plugin '{}' timed out", self.declaration.name));
        }
        
        let poll_dur = remaining.min(Duration::from_millis(10));
        match rx.recv_timeout(poll_dur) {
            Ok(ReaderMsg::Chunk { id, body }) if id == expected_id => on_chunk(body),
            Ok(ReaderMsg::Final { id, body }) if id == expected_id => return Ok(body),
            Ok(ReaderMsg::Err { id: 0, reason }) => return Err(reason),
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("plugin '{}' disconnected", self.declaration.name));
            }
        }
    }
}
```

**Accept:** `cargo test plug2::cancel_in_flight`

### P6-E: RemoteTool cancel listener

**File:** `crates/kn9t-plugin/src/remote_tool.rs`

```rust
fn execute(&self, args: &Value, ctx: &ToolCtx, cancel: &Cancel) -> ... {
    let id = self.host.next_id.fetch_add(1, Ordering::Relaxed);
    
    // Cancel listener thread
    let host = self.host.clone();
    let cancel_clone = cancel.clone();
    let listener = std::thread::spawn(move || {
        while !cancel_clone.cancelled() {
            if cancel_clone.wait_timeout(Duration::from_millis(10)) {
                host.cancel_call(id);
                break;
            }
        }
    });
    
    let result = self.host.call_with_id_streaming(id, "tool_call", payload, ...);
    let _ = listener.join();
    // ...
}
```

**Requires:** add `call_with_id_streaming()` to PluginHost (accepts pre-assigned id).

**Accept:** `cargo test plug2::bash_streams_progress` with cancel mid-execution.

### P6-F: Verify turn.rs (no change needed)

`abort()` already calls `cancel.cancel()`. With P6-A through P6-E, this automatically:
- Makes `CancellableReader.read()` return `Interrupted`
- Makes `wait_for_streaming_cancellable()` send `HostMsg::Cancel` and return

**Accept:** manual test — ESC aborts within 50ms.

### P6-G: Acceptance test

**File:** `crates/kn9t-provider-core/tests/abort_test.rs` (new)

```rust
#[test]
fn abort_interrupts_sse_stream_quickly() {
    // Fake SSE server on localhost that sends one event then sleeps 10s
    // Fire cancel after 50ms
    // Assert total time < 200ms (not 10s)
}
```

**Accept:** `cargo test abort_interrupts_sse_stream_quickly`

### P6 implementation order

```
P6-A  CancellableReader         ← 30min, simple wrapper
P6-B  http.rs cancel param      ← 30min, additive
P6-C  OpenAiProvider            ← 30min, mechanical
P6-D  PluginHost polling        ← 1h, medium complexity
P6-E  RemoteTool listener       ← 1h, thread lifecycle
P6-F  turn.rs verify            ← 15min, just confirm
P6-G  acceptance test           ← 1h, fake TCP server
```

Total: ~5h

### P6 risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| ureq 4KB buffer → max delay = time to fill buffer | Low | SSE lines short; < 5ms WAN |
| `with_retry` retries after cancel | Medium | Add cancel check in retry loop (P6-C) |
| Cancel listener thread leak if tool finishes first | Low | Thread exits on next poll (10ms) |
