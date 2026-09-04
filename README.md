# kn9t

**A minimal, modular coding agent in Rust. OS threads, no async. Events are the wire, the log, and the truth.**

[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![No async](https://img.shields.io/badge/tokio-free-brightgreen)](#design-principles)

kn9t is a coding agent built for auditability. One `Event` enum is simultaneously the SSE
payload, the SQLite row, and the input to state reconstruction. Tools, providers, policy,
compaction, and subagents are all **subprocess plugins** — the core grows seams, not
features.

```
┌─ kn9t-tui / kn9t CLI / curl ─┐   HTTP + SSE, Bearer token, loopback only
└──────────────┬───────────────┘
        ┌──────▼──────┐
        │ kn9t-server │  tiny_http, thread-per-connection
        └──┬───┬───┬──┘
     react │   │   │ plugin host ──► NdJSON over stdin/stdout ──► plugins
      store │   │                     (Rust, Go, Python, TypeScript)
   provider │   └──► ~/.kn9t/kn9t.db  (SQLite, WAL, append-only events)
            └──► OpenAI-compatible / Anthropic / LiteLLM
```

---

## Why

Most agents accrete a second wiring path: an interactive mode and an RPC mode that drift
apart. kn9t forbids it structurally.

- **One vocabulary crate.** `kn9t-core` owns every type, trait, and the bus. It depends on
  `serde` + `serde_json` and nothing else. No other crate may depend on a sibling.
- **The TUI cannot reach into the core.** It talks HTTP only, so it can never become that
  second path.
- **Durable and transient events are different types.** Publishing an unpersisted durable
  event is a *compile error*, not a code review comment.
- **No async runtime.** ~6 modules exchanging messages around one active provider stream.
  `async` would infect every trait signature and buy nothing.

Full rationale in [`DESIGN.md`](DESIGN.md). Structure and known issues in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

---

## Install

Requires Rust 1.94+.

```bash
git clone https://github.com/96Ems/kn9t
cd kn9t
cargo build --release

# default tools ship as a separate plugin crate
cd plugins/kn9t-tools && cargo build --release && cd ../..

# optional: the Go plugin builds independently (add .exe on Windows)
cd plugins/kn9t-agents-md && go build -o kn9t-agents-md . && cd ../..
```

First run bootstraps `~/.kn9t/` — `config.toml` from a commented template, a `token`, and
a `port`. Drop plugin binaries in `~/.kn9t/plugins/`; they are auto-discovered at startup.

> On Windows, invoke cargo through `cmd.exe /c "cargo build"` when working from WSL.
> See [`AGENTS.md §8.1`](AGENTS.md).

---

## Configure

Add at least one provider and one model to `~/.kn9t/config.toml`:

```toml
[[provider]]
id     = "anthropic"
kind   = "plugin"              # subprocess; "openai" for in-process OpenAI-compatible
binary = "kn9t-anthropic"      # bare name resolves next to kn9t-server

[provider.anthropic.env]
ANTHROPIC_API_KEY = "sk-ant-..."

[[model]]
provider  = "anthropic"
id        = "claude-sonnet-4-6"
default   = true
ctx       = 200000
max_out   = 64000
price_in  = 3.0                # USD per 1M tokens
price_out = 15.0
price_cache_read  = 0.30
price_cache_write = 3.75
```

Prices are hand-written on purpose — cost is frozen into each usage row at write time, so
a later price change cannot rewrite history. There is no generated 400-model registry.

Provider eccentricities are declared as config, never sniffed from a URL:

```toml
[provider.my-gateway.quirks]
max_tokens_field = "max_tokens"        # or "max_completion_tokens"
system_role      = "system"            # or "developer"
reasoning        = "reasoning_effort"  # or "budget_tokens" | "none"
usage_in_stream  = true
```

---

## Use

```bash
kn9t                          # TUI (starts the server if needed)
kn9t chat "add a test for parse_url"
kn9t chat                     # REPL
kn9t chat --continue          # resume the most recent session
kn9t chat --json "..."        # JSONL on stdout, one event per line
kn9t sessions                 # list
kn9t history [id]             # transcript
kn9t attach [id]              # observe + write via lease
kn9t cost --group-by model
kn9t stop
```

`--json` makes kn9t scriptable. stdout is one JSON object per line; stderr stays human.

```bash
kn9t chat --json "list files" | jq -r 'select(.kind=="text_delta") | .delta'
```

---

## Plugins

Nothing user-facing is built in. `bash`, `read`, `edit`, `write` are a plugin. So is the
safety policy, compaction, subagents, and the MCP bridge.

| Plugin | Language | Role |
|---|---|---|
| `kn9t-tools` | Rust | default toolset — `bash` / `read` / `edit` / `write` |
| `kn9t-policy` | Python | approves, denies, or escalates every tool call |
| `kn9t-anthropic` | Rust | Anthropic provider |
| `kn9t-compactor` | TypeScript | context compaction |
| `kn9t-subagent` | TypeScript | nested agent loops |
| `kn9t-mcp` | Python | bridges MCP servers, lazy tool discovery |
| `kn9t-agents-md` | Go | discovers and injects `AGENTS.md` |
| `kn9t-ask-user` | TypeScript | interactive prompts |

**Protocol:** newline-delimited JSON on stdin/stdout. One object per line, debuggable with
`cat`. Capabilities are negotiated at handshake, so a plugin that declares neither
`streaming` nor `cancelable` simply never receives those messages.

**Host API:** rather than embedding subagents, the host exposes 10 operations plugins call
back into — `session_read`, `session_prompt`, `session_fork`, `provider_complete`,
`tool_list`, `tool_execute`, `interaction_request`, and three for plugin-declared UI pages.
`provider_complete` uses the session's own model and credentials, and records usage as
`UsageKind::Subagent`, so cost attribution stays correct.

Writing one in Rust:

```rust
use kn9t_plugin_sdk::{Tool, ToolCtx, ToolOutput, CancelToken, wire::ToolSpec};
use serde_json::{json, Value};

struct MyTool;

impl Tool for MyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "my_tool".into(),
            description: "does X".into(),
            schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            parallel_safe: true,
            hidden: false,
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCtx, cancel: &CancelToken)
        -> Result<ToolOutput, String>
    {
        ctx.progress.send("starting…");          // streams as ToolProgress
        if cancel.is_cancelled() { return Err("cancelled".into()); }
        ctx.kv.set("my-scope", "key", &json!(1))?;  // persists across restarts
        Ok(ToolOutput { content: vec![], is_error: false })
    }
}
```

`kn9t-plugin-sdk` has zero workspace dependencies and is publishable, so a third-party
plugin needs nothing from this repo. Go and Python type stubs are generated into
[`schema/generated/`](schema/generated).

> Plugins are discovered from `~/.kn9t/plugins/` only — never from a project directory.
> Cloning a repo must not be code execution ([ADR-0004](docs/adr/0004-plugin-discovery-user-dir-only.md)).

### Safety model

kn9t is **not a sandbox**. Risk judgement lives in a policy plugin, which answers every
tool call with `Allow`, `Deny`, `Ask`, or `Replace`; when several plugins vote, the
strictest answer wins. `Ask` blocks the turn until the user answers via `POST /approve`,
with `once` / `session` / `always` scope.

**With no policy plugin installed, every tool call is allowed.** That is deliberate: a
shell classifier that `sh -c` defeats is worse than an honest absence
([ADR-0008](docs/adr/0008-policy-is-a-plugin.md)).

---

## HTTP API

Base `http://127.0.0.1:<port>` from `~/.kn9t/port`, `Authorization: Bearer <token>` from
`~/.kn9t/token`. Any request carrying an `Origin` header is rejected with 403, so a web
page cannot drive the agent.

```
POST   /session                      create
GET    /session                      list
GET    /session/{id}                 snapshot + transcript
POST   /session/{id}/prompt          run a turn            [lease]
POST   /session/{id}/steer           inject mid-turn       [lease]
POST   /session/{id}/abort           cancel                [lease]
POST   /session/{id}/fork            branch
GET    /session/{id}/events?from=N   SSE: replay then live
POST   /approve                      resolve an approval   [lease]
GET    /tools  /models  /cost  /budget  /health
```

**Single writer per session.** `POST /session/{id}/lease` mints a holder token passed as
`X-Lease` on writes; reads never need one. So three TUIs can watch one session while one
drives it. `?takeover=1` steals a lease from a dead client.

**SSE replays without gaps or duplicates.** Attach subscribes *first*, then reads durable
rows past your cursor, then discards buffered events at or below the watermark. There is a
deterministic regression test for the race, and a script that verifies the test actually
runs.

There are no `PATCH` routes. Partial update is an action endpoint (`/rename`, `/compact`)
or a full replacement (`PUT /pref/{key}`).

Full reference: [`API.md`](API.md), generated from [`schema/http.json`](schema/http.json).

---

## Design principles

1. **Rust, OS threads, no async.** Blocking I/O throughout. No `tokio`, no `.await`.
2. **One vocabulary crate that knows nobody.** `kn9t-core` — `serde` only.
3. **The bus carries facts; traits carry calls.** Events are past-tense, fan out to N
   subscribers, get no reply, and never block the publisher. Anything needing an answer is
   a `&dyn Trait` call. Cancellation is a shared token, never a message.
4. **Events are the wire, the log, and the truth.** Payloads are pure data — no `Arc`, no
   handles, no closures.
5. **Minimal means a dependency budget, not fewer features.**

Enforced mechanically, because an unchecked invariant is a wish:

| | Rule | Checked by |
|---|---|---|
| GI-1 | No crate except `kn9t-server` has >1 workspace dep | `scripts/check-gi1.sh` |
| GI-2 | `kn9t-core` depends only on `serde` / `serde_json` | review |
| GI-3 | No `HashMap` serialized into a cached prefix | `xtask` isolation |
| GI-4 | `events` is append-only | review |
| GI-5 | No `tokio`, no `async fn`, no `.await` | grep |
| GI-6 | `kn9t-tui` does not depend on `kn9t-core` | `scripts/check-schema.sh` |

All JSON is `snake_case`, on the wire and in the database.

---

## Contributing

```bash
cargo test --workspace
cd plugins/kn9t-tools && cargo test        # plugins are separate crates

bash scripts/install-hooks.sh              # once per clone: pre-commit gates
bash scripts/check-ci.sh                   # all invariant gates
cargo run -p xtask -- generate             # after editing schema/*.json
```

`schema/http.json` and `schema/plugin.json` are the source of truth for the API. Five
files are generated from them and committed: `api.rs`, `wire.rs`, `API.md`, and the Go and
Python stubs. Run `generate` after any schema edit and commit the results together —
`check-schema.sh` fails the build on drift.

Two rules worth knowing before you send a patch:

- **A change is done when its test passes.** Implemented-but-untested is not done.
- **Fix the architecture, not the symptom.** If a bug reveals a design flaw, change the
  design. Pending buffers, `unwrap_or` fallbacks for mismatched field names, and duplicated
  old/new format handling are all signs of patching instead of fixing.

Working on this repo with an AI agent? [`AGENTS.md`](AGENTS.md) is the operating guide.

---

## Documentation

| Doc | What it is |
|---|---|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | How the system is built, and what is wrong with it |
| [`DESIGN.md`](DESIGN.md) | The *why* — decisions, rejected alternatives, accepted costs |
| [`API.md`](API.md) | HTTP + plugin protocol reference |
| [`docs/adr/`](docs/adr) | Architecture decision records |
| [`spec/`](spec) | Per-stage requirements and acceptance tests |
| [`CONTEXT.md`](CONTEXT.md) | Glossary |

---

## Status

Stages 01–09 implemented; v1 end-to-end verified. Stage 10 (native Bedrock, Gemini) is v2.
Live status in [`TRACKING.md`](TRACKING.md), narrative in [`CHANGELOG.md`](CHANGELOG.md),
known issues in [`docs/ARCHITECTURE.md §14`](docs/ARCHITECTURE.md).

Not yet stable: expect breaking changes to the config format and the plugin protocol
before 1.0.

## License

MIT — see [`LICENSE`](LICENSE), also declared in `Cargo.toml`
(`[workspace.package] license = "MIT"`).
