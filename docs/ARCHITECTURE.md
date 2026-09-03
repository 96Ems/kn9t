# kn9t — Architecture

**Status:** reflects the tree as built (73 commits, stages 01–09 gates green).
**Scope:** the *as-is* structure — processes, crates, data flow, invariants, and where
they are enforced. This is a map, not a rulebook (`AGENTS.md`) and not a rationale
(`DESIGN.md`). Where the shipped code diverges from the docs, this file says so and
`§14 Findings` records it.

Measured 2026-09-03: 11 workspace crates (~42.5 KLOC Rust), 1 xtask, 8 out-of-workspace
plugins in 4 languages, 437 workspace tests + 26 external.

---

## 1. One picture

```
 ┌──────────── user machine, single user, loopback only ────────────┐
 │                                                                  │
 │  kn9t-tui            kn9t (CLI)          any HTTP client         │
 │  ratatui client      chat/sessions/…     curl, script            │
 │      │                    │                    │                 │
 │      └──── HTTP + SSE ────┴──── Bearer token ──┘                 │
 │                           │  127.0.0.1:<port from ~/.kn9t/port>  │
 │              ┌────────────▼─────────────┐                        │
 │              │      kn9t-server         │  tiny_http,            │
 │              │  router / lease / auth   │  thread-per-connection │
 │              │  SSE fan-out / host API  │  ← the only crate that │
 │              └──┬────────┬──────────┬───┘    names concrete types│
 │                 │        │          │                            │
 │        ┌────────▼──┐  ┌──▼──────┐  ┌▼─────────────┐             │
 │        │kn9t-react │  │kn9t-    │  │ kn9t-plugin  │             │
 │        │ReAct loop │  │store    │  │ stdio host   │             │
 │        │dyn traits │  │SQLite   │  │              │             │
 │        └────┬──────┘  └──┬──────┘  └──┬───────────┘             │
 │             │            │            │ NdJSON over stdin/stdout │
 │      ┌──────▼──────┐  ┌──▼────────┐   │                          │
 │      │provider-core│  │~/.kn9t/   │   │  subprocesses:           │
 │      │ ureq + SSE  │  │ kn9t.db   │   ├─ kn9t-tools     (Rust)   │
 │      └──────┬──────┘  │ (WAL)     │   ├─ kn9t-anthropic (Rust)   │
 │             │         └───────────┘   ├─ kn9t-agents-md (Go)     │
 │      provider-openai                  ├─ kn9t-mcp       (Python) │
 │             │                         ├─ kn9t-policy    (Python) │
 │             ▼                         ├─ kn9t-compactor (TS)     │
 │      api.openai.com / LiteLLM …       ├─ kn9t-subagent  (TS)     │
 │                                       └─ kn9t-ask-user  (TS)     │
 └──────────────────────────────────────────────────────────────────┘
```

Three hard facts shape everything below:

1. **No async.** OS threads and blocking I/O throughout (GI-5). No tokio, no `async fn`,
   no `.await` — verified: the only matches in `crates/` are doc comments saying so.
2. **Events are the wire, the log, and the truth.** One `Event` enum is the SSE payload,
   the SQLite row, and the input to state reconstruction.
3. **One vocabulary crate.** `kn9t-core` is the only crate every other names; no crate
   depends on a sibling except `kn9t-server` (GI-1).

---

## 2. Processes

| Process | Binary | Lifetime | Notes |
|---|---|---|---|
| server | `kn9t-server` | auto-spawned, idle-exits after 5 s with no client and no turn | owns the DB, the plugins, the leases |
| TUI | `kn9t-tui` | user session | ratatui; HTTP + SSE only, never links `kn9t-core` (GI-6) |
| CLI | `kn9t` | per command / REPL | `chat`, `sessions`, `history`, `attach`, `cost`, `models`, `tools`, `status`, `stop` |
| plugin ×N | any executable | spawned at server start, killable/reloadable | NdJSON on stdin/stdout; `POST /plugin/{name}/reload` respawns |

**Startup handshake** (`crates/kn9t-server/src/spawn.rs`, `bootstrap.rs`): a client takes
an exclusive lock on `~/.kn9t/spawn.lock` (flock on Unix, exclusive-create `.held` marker
spin on Windows), reads `~/.kn9t/port`, probes `GET /health`, spawns the server if dead,
then releases. Two clients racing cannot double-spawn.

`~/.kn9t/` is the single runtime root — `config.toml`, `token` (0600 where the OS
supports it), `port`, `spawn.lock`, `kn9t.db` (+ `-wal`, `-shm`), `server.log`,
`plugins/`. `KN9T_HOME` overrides it, which is how tests get isolation.

**Trust boundary:** loopback only, mandatory `Authorization: Bearer`, and *any* request
carrying an `Origin` header is 403'd before routing so a webpage `fetch` cannot drive the
agent (`router.rs:52`). The token is possession-based, not crypto-grade — a splitmix64
over wall-clock nanos plus a stack address (`auth.rs:38`). Adequate for a 0600 file on
loopback; documented as such.

---

## 3. Crate graph (as built)

```
                        kn9t-core  (serde + serde_json only, GI-2)
                        ├── event.rs      Event / LiveEvent — the wire+log
                        ├── traits.rs     Store, Tool, Approver, Compactor, PluginKv
                        ├── hook.rs       HookHost, HookVeto (8 hooks)
                        ├── bus.rs        bounded ring, drop-oldest, never blocks
                        └── cache.rs      breakpoints() — prefix cache planning
                             │
        ┌────────────┬───────┴────────┬──────────────┐
        ▼            ▼                ▼              ▼
  provider-core   kn9t-store     kn9t-plugin    (kn9t-plugin-sdk)
  ureq/TLS,       SQLite,        subprocess     zero workspace deps,
  sse_lines,      projections,   stdio host,    publishable to crates.io
  assemble,       reproject,     RemoteTool/    → the only dep an external
  retry, pricing  blobs, kv      Provider/      plugin may take
        │                        Compactor
        ├──────────────┐
        ▼              ▼
  provider-openai  provider-replay
  OpenAI-compat    raw-byte fixtures through
  + LiteLLM        the *genuine* parser
        │              │
        └──────┬───────┴──────────┬──────────┬──────────┐
               ▼                  ▼          ▼          ▼
                          kn9t-server  ◄── react, store, plugin, core
                          (GI-1 exception: 6 workspace deps)
                               │
                    HTTP + SSE │  (no shared types, no shared code)
                               ▼
                          kn9t-tui   ──── kn9t (CLI, hand-rolled TcpStream)
```

**Verified dependency counts** (`[dependencies]` only, per `scripts/check-gi1.sh`):

| crate | workspace deps | GI-1 |
|---|---|---|
| `kn9t-core` | 0 | ✅ vocabulary root |
| `kn9t-plugin-sdk` | 0 | ✅ publishable |
| `kn9t-store`, `kn9t-plugin`, `kn9t-provider-core` | 1 (`kn9t-core`) | ✅ |
| `kn9t-provider-openai`, `kn9t-provider-replay`, `kn9t-react` | 1 (`kn9t-provider-core`) | ✅ via re-export |
| `kn9t-tui`, `kn9t` | 0 | ✅ GI-6 — HTTP only |
| `kn9t-server` | 6 | ✅ documented exception |

`kn9t-provider-core` re-exports 40+ `kn9t-core` types (`lib.rs:19`) precisely so
`react`/`openai`/`replay` can swap their dep and stay at one. Deliberately *not*
re-exported: `kn9t_core::Quirks`, because it collides with provider-core's own HTTP
`Quirks` — those three crates take `kn9t-core` as a **dev**-dependency instead, which
GI-1 does not count. That is a real hole in the invariant, not a violation of it
(`§14 F2`).

---

## 4. The event model — two tiers, one enum

`crates/kn9t-core/src/event.rs` (414 lines) is the centre of the system. Everything else
is plumbing around it.

```rust
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event { … }        // 23 variants, 6 durable
```

**Tier is encoded in the type, not in a flag.** A variant with a `seq: u64` field is
durable; everything else is transient. `Event::is_durable()` is literally
`self.seq().is_some()`.

| tier | variants | path to a client | lossy? |
|---|---|---|---|
| **durable** (6) | `SessionForked`, `MessageAppended`, `ModelChanged`, `Compacted`, `Handoff`, `UsageRecorded` | `Store::append` → txn commit → `after_append` hook → bus → SSE | **never** |
| **transient** (17) | `TextDelta`, `ThinkingDelta`, `ToolArgsDelta`, `ToolStarted/Progress/Finished`, `TurnStarted/Ended/Status`, `ApprovalRequest`, `InteractionRequest`, `UiDirective`, `RetryAttempt`, `HookFailed`, `TitleChanged`, `Error`, `PluginNotification` | `EventSink::emit` → bus → SSE | yes, by design |

### 4.1 The `LiveEvent` split is the good bit

There is a second enum, `LiveEvent`, holding exactly the 17 transient variants.
`EventSink::emit` accepts **only** `LiveEvent`:

```rust
pub trait EventSink: Send + Sync { fn emit(&self, e: LiveEvent); }
```

So "someone published a durable event on the bus without persisting it" is a **compile
error**, not a code review. `From<LiveEvent> for Event` widens on the way out. This is
the single best structural decision in the codebase — it makes GI-4 (append-only log)
unbypassable from the loop and the tools.

Cost: the 17 variants are declared twice (`event.rs:308–414`). A macro would remove the
duplication and the type-level guarantee with it. Correct trade.

### 4.2 `seq` is stamped by the store, not the caller

Callers construct durable events with `seq: 0` and `Store::append` overwrites it via
`Event::with_seq` inside the transaction, so `events.payload` and every projection row
carry the same true, gapless seq. `head_seq` on `sessions` is the high-water mark and the
SSE cursor.

### 4.3 One wire casing, enforced by serde

`#[serde(rename_all = "snake_case")]` on every enum. Rust stays PascalCase internally;
JSON is snake_case in SSE frames, HTTP bodies, `events.payload`, and the plugin protocol.
One documented collision: `Event::UsageRecorded.kind` clashes with the enum's own `kind`
tag, so the field is `#[serde(rename = "usage_kind")]` (spec bug DB-01, recorded in the
source).

---

## 5. Storage — `kn9t-store` (3.0 KLOC, 12 files)

SQLite via bundled `rusqlite` (no system library). WAL on. **11 tables.**

```
sessions ──┬── events           (session_id, seq)  APPEND-ONLY, the truth
           ├── messages         (session_id, seq)  projection
           ├── usage            (session_id, seq)  projection
           ├── live_messages    (session_id)       MUTABLE — in-flight partial
           └── live_tool_calls  (session_id, call_id)  MUTABLE — tool progress
blobs      (hash)  content-addressed + refcount GC
plugin_kv  (plugin, scope, key)  plugin state, not events
meta       (key)   projection_version = "2"
```

### 5.1 Event sourcing with a narrow mutable island

`events` is the only source of truth. `messages` and `usage` are **derived** — `project()`
(`project.rs:84`) folds one durable event into rows, and `reproject`/`reproject --check`
rebuild them from scratch and diff. That is gate G2, and it is real: the check writes to
`temp.chk_messages`/`temp.chk_usage` and compares.

GI-4 verified: no `UPDATE events` or `DELETE FROM events` anywhere in the crate. The only
`DELETE FROM events` sits in `session_delete.rs:53`, deleting a whole session in one
transaction with blob refcount decrement — which is destruction, not mutation.

The two `live_*` tables are the deliberate exception and are marked non-canonical:

* `live_messages` — the in-flight partial assistant message. **Truncated on open**
  (`db.rs:260`): if the process died mid-stream, the partial is garbage.
* `live_tool_calls` — accumulated `ToolProgress` per call. **Deliberately NOT truncated**,
  because its whole purpose is to outlive the process that wrote it. A tool call the
  process never lived to answer still reports what it had produced, via synthesis in
  `plan_request`. `reproject` ignores it: losing it degrades detail, never correctness.

### 5.2 Money is integers

96E-14: `cost_micros: i64` (1e6 micros = 1 USD) is the source of truth. The `REAL`
`cost_usd` columns survive for reading old rows and wire compat but are **derived** from
micros on write (`project.rs:104`). Prices are snapshotted into the usage row at record
time, so a later price change cannot rewrite history.

### 5.3 Concurrency: one mutex, on purpose

```rust
pub(crate) conn: Mutex<Connection>,   // db.rs:20
```

Documented at the top of `db.rs` (96E-13): WAL is for crash safety and external
`sqlite3` readers, **not** in-process concurrency. There is no pool and no separate reader
connection. `read_attach_snapshot` holds the mutex across payloads *and* `head_seq`
because the 96E-7 bug was exactly that interleaving. Honest and simple; the ceiling is
one writer, and for a single-user localhost agent that ceiling is far away.

One real hazard is handled explicitly rather than by luck: `plan_request` scopes the lock
to the query and **releases it before** `resolve_image_blobs`, because `get_blob` needs
the same lock (`plan.rs:26`, `plan.rs:45`). A comment says "conn lock released here".
That is a self-deadlock avoided by discipline, not by the type system — a reentrant-lock
guard or a `&Connection`-taking inner API would make it structural.

### 5.4 `plan_request` is where reads become honest

`Store::plan_request` is the one call that turns the log into a provider request. It
folds, in order: unparseable `args_json` repair, orphan-tool-call closure (with salvaged
progress), image blob → base64 inline resolution, compaction-span selection at 0.80 ×
`ctx_window`, and `breakpoints()` cache planning. The log stays honest; the *read* is
made usable. That is the right place for it — the alternative is every provider
re-deriving it.

---

## 6. The ReAct loop — `kn9t-react` (3.1 KLOC)

The loop owns **only trait objects**. `loop_.rs:88`:

```rust
pub struct ReactLoop {
    pub provider:  Arc<dyn Provider>,
    pub store:     Arc<dyn Store>,
    pub approver:  Arc<dyn Approver>,
    pub tools:     ToolRegistry,
    pub hooks:     Arc<dyn HookHost>,
    pub bus:       Arc<dyn EventSink>,
    pub compactor: Option<Arc<dyn Compactor>>,
}
```

No concrete `Provider`, `Tool`, `Store`, or `Approver` is named anywhere in the crate —
that is the GI-1 payoff, and it is why the whole loop is testable against
`kn9t-provider-replay` with no network and no spend (gate G1).

Per-run state lives in `RunParams` (model, thinking, cwd, cancel, read-hash map), not in
the struct, so a `prepare_next_turn` hook can evolve the model mid-run without mutating
shared state.

### 6.1 One turn

```
run() ──► TurnStarted
          │
          ├─ execute_turn ──► attempt loop ──────────────────────────────┐
          │                   │                                          │
          │                   ├─ provider_attempt → Completed            │
          │                   ├─                  → AbortedInStream ─────┤ record usage,
          │                   │                                          │ discard partial,
          │                   ├─                  → Truncated ───► ladder│ go idle
          │                   │                    150,100,50,25,10 lines│
          │                   │                    (max 4, then give up) │
          │                   └─                  → ContextOverflow ─────┤ compaction
          │                                        (exactly 1 re-plan)   │ re-plan
          │                                                              │
          ├─ append MessageAppended + UsageRecorded(Main)   ◄─────────────┘
          │
          ├─ tool calls?  no ─► should_stop_after_turn hook ─► get_followup ─► idle|continue
          │                yes
          ├─ authorize each call (hook veto → approver) → run_tool_batch
          ├─ append tool results **in the model's call order**
          ├─ cancelled? → keep transcript consistent, go idle (no rollback)
          └─ get_steering ─► prepare_next_turn ─► continue
```

Four things worth calling out:

**Money is confined.** Provider calls and `UsageRecorded` happen *here and only here*.
Compaction uses `UsageKind::Compaction`, titling `Title`, subagents `Subagent` — so cost
attribution is a partition, not a guess.

**Abort keeps the transcript consistent.** On cancel mid-batch the loop still appends the
assistant message and *all* results, synthesizing errors for the aborted ones
(`exec.rs:487 synth_error`), then goes idle. It does **not** roll back. A model resuming
that session sees a well-formed call/result pairing, which is the thing providers reject
if you get it wrong.

**Malformed tool args fail closed.** `authorize()` on unparseable `args_json` returns deny
and emits `Event::Error` (test `p1_96e8_authorize_malformed_json_is_deny`). Earlier it was
silently swallowed.

**Compaction is fail-closed.** 96E-17 removed the hardcoded inline-prompt fallback: with
no compactor plugin installed, context exhaustion ends the turn with
`ReactError::CompactionUnavailable` and persists nothing. A session simply cannot continue
past its window without a compactor. That is a deliberate, documented capability
regression in exchange for not shipping a second, worse compaction implementation inside
the core.

### 6.2 Truncation ladder and re-plan budget

`ReactConfig` — values, not interfaces, so they are tunable: 4 truncation attempts,
ladder `150,100,50,25,10` write-lines, and `max_context_replans = 1`. A second compaction
demand in one turn is `ReactError::CompactionLoop`, not an infinite loop.

---

## 7. Providers

```
kn9t-provider-core (1.1 KLOC)  ── the four things nobody should reimplement
  http.rs      ureq, blocking, AuthScheme
  sse.rs       sse_lines() — 34 lines, the whole SSE framing
  assemble.rs  delta accumulation + partial-JSON tool-arg buffering
  retry.rs     pre-stream only: 429/5xx/connect, backoff, RetryAttempt events
  abort.rs     CancellableReader — cancel lands mid-stream, not after
  pricing.rs   lookup_price
  quirks.rs    per-model HTTP eccentricities
```

A provider implements **wire mapping only**. Measured: `provider-openai` 1.4 KLOC
(encode 358 + decode 150 + provider 204 + cache), `kn9t-anthropic` 623 lines as a
subprocess plugin.

**Retry is pre-stream only.** Once bytes are flowing, a failure is a stream failure — it
cannot be retried without double-charging or duplicating output. Correct and unusual;
most clients get this wrong.

**`kn9t-provider-replay` is built before any real network call** (stage 02). It replays
**raw provider bytes** through the *genuine* parser, so fixtures exercise
`sse_lines` + `assemble` + decode for real. `.gitattributes` pins `fixtures/** -text` so
line endings are never converted — on a Windows dev box that is the difference between a
meaningful fixture and a lie.

**Plugin providers cannot link `provider-core`** (GI-1: their one workspace dep is
`kn9t-plugin-sdk`), so `kn9t-anthropic` brings its own `ureq`. The SSE parser that was
duplicated byte-identically in two plugins now lives in `kn9t-plugin-sdk/src/sse.rs`.
The shared-layer saving therefore applies fully only to in-process providers — DESIGN §2.1
states this honestly rather than pretending the ~250-line estimate held.

---

## 8. Server — `kn9t-server` (9.2 KLOC, 36 files)

The GI-1 exception, and the only place concrete types are named: `SqliteStore`,
`ServerHostApi`, `InteractiveApprover`, the provider registry.

```
main.rs ─► ServerHandle::spawn
             ├─ TcpListener bind 127.0.0.1:0 → tiny_http::from_listener
             ├─ idle-exit watchdog thread (200 ms tick)
             └─ accept loop ─► thread per connection ─► router::handle
                                                          │
   ┌──────────────────────────────────────────────────────┘
   │  1. idle.touch()
   │  2. Origin present?        → 403  (before anything else)
   │  3. Bearer token match?    → 401
   │  4. GET .../events | /attach → hijack socket, SSE, never returns a Reply
   │  5. lease-required route?  → X-Lease check → 409 session_busy
   │  6. route() → typed api::* body (deny_unknown_fields) → routes::*
   └─────────────────────────────────────────────────────────────────────
```

**33 routes**, grouped in `routes/`: `session.rs` (573 lines — create/list/snapshot/fork/
delete/lease/prompt/steer/abort/model/approve/rename/compact/export), plus `blob`, `cost`,
`models`, `pref`, `policy`, `plugin`, `tools`, `interaction`.

### 8.1 No PATCH — action endpoints only

Verified: the router matches `Post`, `Get`, `Put`, `Delete` and nothing else. Partial
update is expressed as an action (`POST /session/{id}/rename`, `POST /{id}/compact`) or a
full replacement (`PUT /pref/{key}`). This falls straight out of event sourcing — an event
is an atomic fact, and there is no merge semantics to argue about.

### 8.2 The SSE attach race, solved in the right order

`sse.rs` is the most carefully-reasoned file in the server. The order is the whole point:

```
1. subscribe FIRST            ← buffer everything from now on
2. read durable rows > from, up to head_seq → emit
3. read live_messages → emit live_partial frame
4. flush the buffer, DISCARDING anything with seq <= head_seq
5. go live
```

Read-then-subscribe is explicitly forbidden: a durable event committed in the window
between read and subscribe would be lost, and transient self-healing does not cover
durable events. There is a deterministic regression test using `KN9T_SSE_TEST_DELAY_MS` to
widen the window artificially (96E-7), and `scripts/check-sse-race.sh` asserts the test is
actually *run* rather than silently `#[ignore]`d. That is the correct paranoia: the bug
class is "the test existed but never executed".

`build_attach_prelude` is factored to be pure over store reads + an already-taken
subscription, so the ordering is unit-testable **without a socket**. Good seam.

### 8.3 Durable events reach the bus by an after-commit hook

Because `EventSink` is transient-only (§4.1), the loop physically cannot publish
`MessageAppended`. So the store grew one observer:

```rust
pub(crate) after_append: Mutex<Option<Arc<dyn Fn(&SessionId, &Event) + Send + Sync>>>,
```

The server installs it at startup; it fires once per durable append, **outside** the
connection lock, with the seq-stamped event (96E-18). This is the clean resolution of a
real tension — the alternative would have been widening `EventSink` and losing the
compile-time guarantee. Fix the architecture, not the symptom.

### 8.4 Leases: one writer per session, N readers

`POST /session/{id}/lease` mints a holder token; writes carry `X-Lease`. Reads never need
one, so 3 TUIs can watch one session while one drives it (gate G3). Release on explicit
`DELETE`, on holder disconnect, or after 5 min idle. `?takeover=1` force-steals — needed
because a hard-killed client cannot release its own lease.

`POST /approve` has no session in its path, so it requires `X-Lease` **plus**
`X-Lease-Session`. A small wart of the "approvals are global, sessions are not" shape.

### 8.5 Idle exit

`IdleTracker`: exit when *no attached client* and *no running turn* for 5 s. `GET /attach`
is a per-client-process heartbeat stream — it is the thing that increments
`attached_clients`; per-session SSE deliberately does not, so opening a second view of a
session does not extend server life. Heartbeat period is `KN9T_SSE_HEARTBEAT_MS`
(default 15 s), shared by both attach paths — it was previously hardcoded to 30 s in one
of them, making dead-client detection untestable.

---

## 9. Policy and approvals — ADR-0008, the sharpest call in the project

The original design put a **shell command classifier in the server** (ADR-0001):
cross-platform pwsh + POSIX grammars, deciding whether `rm -rf /` behind `sh -c` was
dangerous. ADR-0008 deleted it — 333 lines of `classify.rs` plus its tests — and moved
judgement into a **user-installed plugin** (`plugins/kn9t-policy.py`).

What is left in the server is only the part a subprocess cannot own:

```
tool call
   │
   ├─► HookHost::before_tool_call  ──► every policy plugin votes
   │      HookVeto::{Allow, Ask{reason}, Deny{reason}, Replace{args}}
   │      composition: STRICTEST wins (Deny > Ask > Allow), not first-deny
   │
   ├─ Allow  → dispatch
   ├─ Deny   → synthesized error result
   ├─ Replace→ dispatch with rewritten args (short-circuits: later plugins
   │            would otherwise judge stale input)
   └─ Ask    → Approver::request
                 │  needs the session bus, the write lease, and config.toml
                 ├─ ApprovalCache hit (scope session|always)? → Allow
                 ├─ emit Event::ApprovalRequest → block on Condvar
                 ├─ POST /approve {id, decision, scope} → resolve
                 └─ no interactive client? → DenyAllApprover: deny, honestly
```

**Strictest-wins, not first-wins, is the correct choice** and the reasoning is recorded:
with `Ask` in the vocabulary, short-circuiting would make the outcome depend on plugin
*load order* — an early `Ask` masking a later `Deny`. So every plugin is consulted.

**Three consequences, stated plainly:**

1. **With no policy plugin installed, every tool call is allowed.** The hook layer
   fail-opens to `HookVeto::Allow`. Safety is now opt-in via installation. ADR-0008
   decision 5 accepts this: kn9t is not a sandbox, and a fake classifier that `sh -c`
   defeats is worse than an honest absence.
2. `Approver` keeps `Ask`/`HardDeny` variants that nothing derives any more — they exist
   for the wire (`POST /approve`) and for old replayable events.
3. **The spec is stale.** R-TOOL-070/080/090/095 still describe a classifier that no
   longer exists. `TRACKING.md` flags this as SPEC-STALE; the tests those requirements
   name are gone. See `§14 F1`.

`policy.rs` carries `#![deny(clippy::unwrap_used)]` and `scripts/check-unwrap-trend.sh`
hard-fails if a single bare `.unwrap()` reappears there. Enforcing an invariant on the
one file where panicking means "deny turned into crash" is proportionate.

The per-turn session sink is threaded to the globally-shared approver through
**thread-local storage** (`POLICY_SINK`, `POLICY_SESSION`), because `Approver::request`
has no session parameter. It is honest about being a workaround for a trait signature.
The trait should take a sink; TLS is invisible coupling that will bite whoever moves
approval off the turn thread.

---

## 10. Plugins — the extension model

Everything user-facing is a plugin. **Tools are not built in**: `bash`, `read`, `edit`,
`write` live in `plugins/kn9t-tools` (Rust, 1.1 KLOC), a separate crate outside the
workspace, spawned as a subprocess. A server with an empty plugin dir starts, warns, and
serves — degraded, not crashed.

### 10.1 Wire: NdJSON, one JSON object per line

`kn9t-plugin/src/codec.rs`. Symmetric, trivially debuggable with `cat`.

```
HostMsg   → Hello, Hook{id,hook,payload}, Event, Cancel{id}, Shutdown,
            KvResult{id,…}, ApiResult{id,…}
PluginMsg → Hello{name,capabilities,hooks,tools,events,provider},
            Result{id,…}, Chunk{id,…}, Done{id,…}, Event,
            KvGet/KvSet/KvDel/KvDelScope{id,…},
            Request{id,op,payload}          ← the host API, §10.3
```

Capability-negotiated at handshake: a v1 plugin declaring neither `streaming` nor
`cancelable` uses only `Hook`/`Result`; `Chunk`/`Done`/`Cancel` are sent only to plugins
that asked for them. Backward compatibility by declaration rather than version sniffing.

### 10.2 The 8 hooks and their composition classes

`ComposedHookHost` is explicit about *how* multiple plugins combine, which is the part
most plugin systems leave implicit:

| class | hooks | rule |
|---|---|---|
| veto | `before_tool_call` | strictest wins (`Deny > Ask > Allow`); `Replace` short-circuits |
| pipeline | `after_tool_call`, `before_request`, `prepare_next_turn` | B sees A's output |
| collect | `get_steering`, `get_followup` | concat in declared order, host queue first |
| any-says-stop | `should_stop_after_turn` | logical OR |
| first-non-null | `get_api_key` | first plugin that answers wins |

Failure posture is fail-open with per-hook timeouts, and a failure emits
`Event::HookFailed` rather than dying quietly. `NoopHookHost` in core means the loop runs
with zero plugins, which is what makes the react crate testable standalone.

### 10.3 Host API — kn9t does not embed subagents, it opens a door

This is the most interesting design move in the plugin layer. Rather than building
subagents, compaction, and MCP into the core, the host exposes 10 operations that plugins
call *back* into:

```
session_read        read the transcript by seq range
session_prompt      run a real turn
session_fork        fork (fork_reason = subagent)
provider_complete   one LLM call with the session's model/credentials/cache
                    (usage recorded as UsageKind::Subagent — cost stays attributed)
tool_list           what tools exist
tool_execute        run a tool through the normal policy path
interaction_request block until the user answers (generic, opaque payload)
ui_declare_page / ui_write_placeholder / ui_clear_page   plugin-declared TUI pages
```

So `kn9t-subagent` (TypeScript, ~200 lines) runs its own agent loop using kn9t's
providers and store. `kn9t-compactor` (TypeScript) *is* compaction. `kn9t-mcp` (Python)
bridges MCP servers. None of that is core code.

Each request is dispatched **on a worker thread per request** (96E-9) so a slow op cannot
block the plugin's reader thread — the classic stdio-host deadlock, handled.

The `HostApi` trait lives in `kn9t-plugin` so that crate stays GI-1 clean (it only names
`serde_json::Value`); the implementation is the server's business
(`kn9t-server/src/host_api.rs`, 414 lines).

### 10.4 Four languages prove the protocol

| plugin | lang | role |
|---|---|---|
| `kn9t-tools` | Rust | `bash`/`read`/`edit`/`write` — the default toolset |
| `kn9t-anthropic` | Rust | provider plugin, own `ureq` |
| `kn9t-test-plugin` | Rust | real-subprocess test fixture (P4-A) |
| `kn9t-agents-md` | Go | discovers and injects `AGENTS.md`, KV-backed |
| `kn9t-mcp` | Python | MCP bridge (stdio + HTTP) |
| `kn9t-policy` | Python | **the safety policy** (ADR-0008) |
| `kn9t-compactor` | TypeScript | compaction |
| `kn9t-subagent` | TypeScript | subagents, re-entrant |
| `kn9t-ask-user` | TypeScript | user interaction via `interaction_request` |

Go and Python type stubs are generated from `schema/plugin.json` into
`schema/generated/`. Four languages is a genuine protocol test — an internal-only
convention would have drifted long ago.

`kn9t-plugin-sdk` (2.2 KLOC, zero workspace deps) is publishable to crates.io so a
third-party Rust plugin needs nothing from this repo.

**Discovery is user-dir only** (ADR-0004): `~/.kn9t/plugins/` and pinned `[[plugin]]`
config entries. Never a project-relative `plugins/`, because then cloning a repo would be
code execution. The repo's `plugins/` is *build source*; `~/.kn9t/plugins/` is the
*install target*. This distinction is load-bearing and easy to get wrong.

---

## 11. TUI — `kn9t-tui` (14.7 KLOC, the largest crate)

Links **no** `kn9t-*` crate. Verified: zero references to `kn9t_core` or `kn9t_server`,
and `check-schema.sh` greps `Cargo.toml` for `^\s*kn9t-` to keep it that way.

```
main.rs ─► terminal setup (raw, alt screen, bracketed paste, mouse)
           ├─ input thread   (crossterm)
           ├─ tick thread    (80 ms, only while streaming)
           └─ SSE thread     (spawned on session select)
                 │ frames
                 ▼
           reducer.rs (805 lines)  SseFrame → State      ← pure, 157 unit tests
                 │
           app.rs (2769 lines)     App: 32 fields, composed from
                 │                 SessionManager / ModelSelector / TokenTracker /
                 │                 Transcript / SlashState / SearchState / …
                 ▼
           ui/render.rs (2347)  +  diff_viewer (1343), markdown (542), search (528),
                                   latex (392), which_key (408), syntax, theme, …
```

**The reducer split is what makes this testable.** `reduce(&mut State, SseFrame)` is a pure
function, so 157 tests cover live event paths with no terminal and no server. Golden
snapshots (96E-19) cover rendering.

`wire.rs` (253 lines) is **generated** from `schema/http.json` — GI-6-clean serde mirrors.
The TUI and the server therefore agree by construction rather than by review. This
directly fixed a three-way drift where API.md, the server, and `wire.rs` disagreed on
nearly every route, and where the TUI sent `decision: "always"` while the server checked
`== "allow"`, silently recording "always" as a **deny**. That class of bug is now
structurally impossible.

**The TUI is treated as the API proving ground**: a missing capability becomes a server
endpoint, not a client workaround. `POST /{id}/rename`, `/compact`, `/export`, and
`GET /tools` all exist because the TUI needed them. That discipline is why there are no
PATCH routes and no client-side state reconstruction.

`app.rs` at 2769 lines and `render.rs` at 2347 are the two files that will need splitting
next; the manager extraction (2026-08-28) already removed ~835 lines of dead duplicate
state, so the pattern is established.

### 11.1 Plugin-declared UI (96E-23…27)

A plugin can declare a page with typed placeholders (`text|number|bar|list`), write
placeholder values, and clear it; the host validates placeholder existence and value kind,
and the TUI renders it. `UiDirective` events are **session-scoped, never broadcast** —
96E-21 fixed exactly that leak. This lets a plugin own screen real estate without the TUI
knowing what the plugin is.

---

## 12. Schema-first contract (ADR-0005)

`schema/http.json` + `schema/plugin.json` are the single source of truth. Five outputs are
**generated and committed**:

```
schema/http.json ──┬─► crates/kn9t-server/src/api.rs      typed reqs, deny_unknown_fields
                   ├─► crates/kn9t-tui/src/wire.rs        GI-6-clean mirrors
                   ├─► API.md                             human contract
schema/plugin.json ┼─► schema/generated/go_types.go
                   └─► schema/generated/python_types.py
```

Generation is **manual** (`cargo run -p xtask -- generate`), never a `build.rs`. The reason
is precise and worth preserving: `xtask` needs `preserve_order` (IndexMap) for stable
output, and Cargo feature unification would leak that into every runtime crate, violating
GI-3. `xtask/Cargo.toml:8` documents the deliberate omission.

Drift is caught at commit/CI, not at build — `cargo build` passes even when drifted, so
`scripts/check-schema.sh` runs `xtask --check` for a byte-identical comparison and refuses
the commit. `deny_unknown_fields` on every request struct means a mistyped field is a 400,
never a silent ignore.

---

## 13. Invariants and how each is actually enforced

The stated lesson in `TRACKING.md` is *"the invariant claim was untrue for an unknown
period because nothing checked it. Prefer a script over an assertion."* That lesson is
mostly applied:

| invariant | enforced by | status |
|---|---|---|
| GI-1 ≤1 workspace dep | `scripts/check-gi1.sh` | ✅ passes (see F2 for the dev-dep hole) |
| GI-2 core = serde only | Cargo.toml review | ✅ verified by inspection |
| GI-3 no `preserve_order` | `xtask` isolation + comment | ✅ verified: no runtime crate enables it |
| GI-4 append-only `events` | code review | ✅ verified: no `UPDATE`/`DELETE` on `events` |
| GI-5 no async/tokio | grep | ✅ verified: only doc comments match |
| GI-6 TUI links no core | `check-schema.sh` grep | ✅ verified: 0 references |
| snake_case wire | serde attributes | ✅ structural |
| durable ≠ transient | **the type system** (`LiveEvent`) | ✅ compile error |
| schema ↔ code | `check-schema.sh` | ⚠️ green only on LF checkouts — F3 |
| no bare unwrap in `policy.rs` | `check-unwrap-trend.sh` + `deny(clippy::unwrap_used)` | ✅ |
| SSE race test is run | `check-sse-race.sh` | ✅ |

Six guard scripts (`check-ci.sh` aggregates five) plus `pre-commit.hook`.

**Test posture:** 437 workspace tests + 26 external, verified this session:
`cargo test --workspace` → **436 passed, 1 failed**. The single failure is
`srv::plugin_reload`, which is a hardcoded `panic!("not supported on Windows in this
harness")` — a *declared* platform gap, not a regression. Confirmed pre-existing and
documented. Distribution is healthy: `kn9t-core` 48 inline + 24 integration, `kn9t-tui`
157 inline, `kn9t-server` 37 + 55.

---

## 14. Findings

Ordered by consequence. Nothing here blocks; several are cheap.

### F1 — Spec is stale where ADR-0008 deleted code (highest)

R-TOOL-070/080/090/095 describe a shell classifier that no longer exists, and name
acceptance tests (`tool::classify_*`) that are deleted. `TRACKING.md` flags it as
SPEC-STALE and `AGENTS.md §3` still lists the classifier in the build order. Because
"a requirement is done only when its named test passes", four requirements are currently
unverifiable by their own definition.

*Fix:* rewrite those four requirements to describe the `HookVeto` seam, or retire them and
add `R-POLICY-*` for the plugin contract. Documentation debt, not code debt — but it is the
kind that makes the next session distrust the tracker.

### F2 — GI-1 has a dev-dependency hole

`kn9t-react`, `kn9t-provider-openai`, and `kn9t-provider-replay` each take
`kn9t-provider-core` as their one `[dependencies]` entry **and** `kn9t-core` as a
`[dev-dependencies]` entry. `check-gi1.sh` counts `[dependencies]` only, so this passes.
It is defensible (tests need `kn9t_core::Quirks`, which provider-core cannot re-export
without a name collision) and it is *documented in the Cargo.toml itself* — good practice.

But the invariant's own stated purpose is "no crate names a sibling", and a dev-dep is a
named sibling. Either the invariant should say "`[dependencies]` only" explicitly in
`AGENTS.md §4`, or `Quirks` should be renamed (`ModelQuirks` / `HttpQuirks`) so the
re-export works and the hole closes. The rename is ~20 lines and removes an asterisk from
three crates.

### F3 — `check-gi1.sh` cannot run on this machine

`core.autocrlf=true` plus `* text=auto` means every `.sh` file is CRLF in the working tree.
Running `bash scripts/check-gi1.sh` fails immediately:

```
line 4: $'\r': command not found
line 12: syntax error near unexpected token `$'do\r''
```

All six guard scripts are affected. The pre-commit hook is also **not installed**
(`.git/hooks/` contains only `.sample` files), so *no* guard currently runs on this
checkout — including the schema drift gate.

**ADR-0007 predicted this exact bug class and then mis-prescribed the cure.** It opens by
naming the failure — *"an invariant claimed in docs that nothing enforced… breaks
`check-schema.sh`/`check-gi1.sh` drift gates"* — and its Consequences assert *"Windows
contributors can keep `core.autocrlf=true` locally."* They cannot. `* text=auto` normalizes
LF in the **index** but hands the working tree CRLF, and `bash` executes the working tree.
Verified:

```
$ git ls-files --eol scripts/check-gi1.sh
i/lf    w/crlf    attr/text=auto    scripts/check-gi1.sh
```

The diagnosis was right, the remedy was one attribute short. That is the highest-leverage
fix in this list, because it is the mechanism every other invariant depends on:

```
# .gitattributes
*.sh                      text eol=lf
scripts/pre-commit.hook   text eol=lf
```

then `git add --renormalize .` and install the hook. ADR-0007 should be superseded rather
than edited, since its Consequences section is now known-false.

### F4 — `xtask --check` reports drift that is pure line-endings

Related to F3 and worth separating. Running `cargo run -p xtask -- --check` reports **all
five** generated files as drifted. They are not: `git diff --ignore-cr-at-eol` shows zero
content difference. The generator writes `\n`; the checkout has `\r\n`; `check()` compares
strings byte-for-byte (`xtask/src/main.rs:128`).

Consequence: on Windows the gate is permanently red, which trains whoever sees it to
ignore it — the exact failure mode these scripts exist to prevent. And running `generate`
to "fix" it rewrites all five files to LF, producing a 5-file diff with no semantic change.

*Fix:* normalize before comparing, e.g. `have.replace("\r\n", "\n") == want`, or add the
generated paths to `.gitattributes` with `eol=lf`. One line either way.

### F5 — Mojibake guard is itself mojibake'd

`scripts/check-mojibake.sh:12` holds a `PATTERN=` line listing eight alternatives, spelled
with literal U+00C2 / U+00E2 / U+00C3 lead bytes (shown here as codepoints, so that *this*
document does not trip the very guard it describes):

```
U+00C2 U+00A7 | U+00E2 U+20AC U+201D | U+00E2 U+20AC U+2122 | U+00E2 U+20AC U+0153
             | U+00E2 U+20AC | U+00C3 U+00A9 | U+00C3 U+00A8 | U+00C3 SPACE
```

The script is correct *by construction* — it must contain the byte patterns it hunts. But
the same file's own comments were double-encoded, and `check-mojibake.sh` excludes itself
from the grep, so it can never report its own corruption.

Meanwhile `CHANGELOG.md` contains ~25 genuinely mangled sequences (`rAcponse`,
`dAcfaut`, `A?` for `À`) from a French-language session — which the guard does not catch
because that is a *different* mis-decoding (UTF-8 → CP1252-ish), not double-UTF-8. The
guard's scope (`crates/ docs/ spec/`) also excludes `CHANGELOG.md` entirely.

*Fix:* widen the scope to root `*.md`, and add the observed `[A-Za-z]Ac` / `A\?` patterns.
Low severity, but a guard that cannot see the corruption in the repo it guards is worse
than no guard, because it certifies cleanliness.

### F6 — TLS-threaded session sink in the approver

`POLICY_SINK` / `POLICY_SESSION` thread-locals exist because `Approver::request(&self,
call, cwd, reason)` has no session parameter. The comment is honest about it. The coupling
is invisible: move approval off the turn thread and it silently emits to nobody.

*Fix:* add the sink to the trait signature. It is a 1-arg change to one trait with two
implementors. Do it before something else needs approval off-thread.

### F7 — Five hand-rolled HTTP clients in `kn9t`

`chat.rs`, `cmd_cost.rs`, `cmd_history.rs`, `cmd_models.rs`, `cmd_status.rs`,
`cmd_sessions.rs`, `cmd_tools.rs` each define their own `get_json` over a raw `TcpStream`,
each formatting `Bearer` headers by hand. The CLI has **zero** dependencies, which is
presumably the point — but GI-6 only forbids depending on `kn9t-*`, and `ureq` is already
in the lock file via the TUI and provider-core.

*Fix:* one `http.rs` module inside `kn9t`, ~60 lines, or take the `ureq` dep the rest of
the tree already pays for. Seven copies of response parsing is where a subtle
chunked-encoding bug will eventually live.

### F8 — Smaller items

* **Debug `eprintln!` in library code.** 6 in `kn9t-plugin/src/composed.rs`, 3 in
  `kn9t-server/src/turn.rs`, tagged `[DEBUG …]`. The server has a `log!` macro; the plugin
  crate writes to the same stderr the plugin protocol shares. Route them through `log!` or
  delete.
* **`crossterm` ×3 in the lock file** (0.27 CLI, 0.28 transitive, 0.29 git-patched for the
  Windows bracketed-paste fix). Understandable — the patch is upstream PR #1030 — but three
  terminal libraries in one binary tree is a real cost. Track the upstream merge.
* **Doc comments truncated at the first line.** ~158 `///` blocks begin with a lowercase
  continuation word (`/// and the …`, `/// then …`), meaning a first line was stripped at
  some point. The remaining prose is excellent; the summary line is missing, so
  `cargo doc` reads oddly. Mechanical to spot, tedious to fix.
* **README describes a tree that has moved on.** It references
  `plugins/kn9t-custom-provider` (28 mentions across docs — the directory does not exist)
  and `internal-plugins/` (also absent), and its Status and Architecture tables have empty
  bodies where content was cut. `spec/09` still points at `internal-plugins/kn9t-anthropic`.
* **Untracked reorganization in flight:** `plugins/kn9t-policy.py` is deleted and
  `plugins/kn9t-policy/kn9t-policy.py` is untracked. Harmless, but commit it — the policy
  plugin is the safety mechanism and should not be in limbo.
* **Two Go binaries committed** (`kn9t-agents-md` + `.exe`, 5.1 MB total). `.gitignore`
  covers `target/` but not built plugin binaries.
* **233 `.unwrap()` / 85 `.expect()`** across `crates/`. The trend script guards only
  `policy.rs` and `host.rs`. Most of the rest are in tests or genuinely-infallible spots,
  and lock `.expect("… poisoned")` is a defensible convention — but the number only moves
  in one direction without a broader baseline.

---

## 15. What this architecture gets right

Worth stating, because the findings list is longer than the praise list and that inverts
the actual ratio.

1. **`LiveEvent` makes an invariant unbypassable.** Most systems document "don't publish
   unpersisted events"; this one makes it not compile.
2. **`ReactLoop` owns only `dyn` traits.** The entire agent loop runs against replayed
   raw bytes with no network and no spend. That is why stage 02 comes before stage 05.
3. **Tools, policy, compaction, subagents, and MCP are all out-of-process.** The core does
   not grow features; it grows *seams*. Nine plugins in four languages, and the host API is
   10 operations.
4. **The SSE attach order is reasoned, tested, and guarded** — including a script that
   verifies the test actually runs.
5. **Generated contract, committed and drift-checked.** The three-way API drift that
   silently turned "always" into "deny" cannot recur.
6. **Deletion is treated as a legitimate fix.** ADR-0008 removed 333 lines of classifier
   rather than patching it; 96E-17 removed the fallback compactor rather than keeping a bad
   one. `AGENTS.md §10` ("no patches, fix the architecture") is visibly obeyed in the
   commit history, not just asserted.
7. **The costs are written down.** DESIGN §2.1 corrects its own line-count estimate by 3×
   rather than leaving an aspiration; the single-mutex store says exactly why it is not a
   pool; GI-1's dev-dep exception is justified in the Cargo.toml where it happens.

The dominant risk is not structural, it is **enforcement**: F3 + F4 mean none of the guard
scripts run on the primary development machine, and F1 means the spec no longer describes
the code in the one area that governs whether `rm -rf /` executes. Both are cheap. Fix
those two and the invariant discipline this project is built on becomes real again rather
than nominal.

---

## Appendix — file map by size

| crate | files | lines | heaviest |
|---|---|---|---|
| `kn9t-tui` | 36 | 14 674 | `app.rs` 2769, `ui/render.rs` 2347, `diff_viewer.rs` 1343 |
| `kn9t-server` | 36 | 9 191 | `config.rs` 942, `policy.rs` 692, `tools.rs` 674, `routes/session.rs` 573 |
| `kn9t-react` | 8 | 3 132 | `exec.rs` 634, `hooks.rs` 180, `turn.rs` 170 |
| `kn9t-core` | 19 | 3 079 | `event.rs` 414, `bus.rs` 331, `ids.rs` 293 |
| `kn9t-store` | 15 | 3 029 | `db.rs` 460, `session.rs` 268, `plan.rs` 258 |
| `kn9t-plugin` | 11 | 2 982 | `host.rs` 983, `remote_provider.rs` 230, `codec.rs` 203 |
| `kn9t-plugin-sdk` | 9 | 2 169 | `ctx.rs` 464, `plugin.rs` 360, `subagent.rs` 274 |
| `kn9t` (CLI) | 10 | 2 100 | `chat.rs` 686, `bootstrap.rs` 516, `main.rs` 353 |
| `kn9t-provider-openai` | 7 | 1 387 | `encode.rs` 358, `provider.rs` 204 |
| `xtask` | 6 | 1 319 | generators |
| `kn9t-provider-core` | 9 | 1 074 | `pricing.rs` 154, `abort.rs` 153 |
| `kn9t-provider-replay` | 6 | 912 | fixtures + parser drive |

Tests: 8 004 lines across 22 integration files, plus inline modules.



