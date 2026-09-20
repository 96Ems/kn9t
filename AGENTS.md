# AGENTS.md — kn9t

Practical guide for contributing to kn9t.

---

## 1. Architecture at a glance

```
kn9t-core          → types, Event enum, traits (Provider, Tool, Store, Policy)
                     depends only on serde — everything else depends on this

kn9t-server        → HTTP API, SSE, plugin host
                     the ONLY crate that wires concrete implementations together

kn9t-tui           → terminal UI, talks HTTP only, never imports kn9t-core
                     if TUI needs something, add it to the server API

kn9t-plugin-sdk    → SDK for external plugins, zero workspace deps, publishable

plugins/kn9t-tools → default tools (bash, read, write, edit)
```

**Key rule:** if the TUI needs data or an action, add it to kn9t-server. The TUI must
never work around a missing API — that's how second code paths appear.

---

## 2. Where to put what

| You want to... | Put it in... |
|----------------|--------------|
| Add a new Event variant | `kn9t-core/src/event.rs` |
| Add a new HTTP endpoint | `kn9t-server/src/routes/` + `schema/http.json` |
| Add a new host-API op for plugins | `kn9t-server/src/host_api.rs` + `schema/plugin.json` |
| Add a TUI feature | `kn9t-tui/` — but if it needs server data, add the endpoint first |
| Add a tool | `plugins/kn9t-tools/` or write your own plugin |
| Change wire format | `schema/*.json` → run `cargo run -p xtask -- generate` |

---

## 3. Invariants (CI enforces these)

- **GI-1:** No crate except `kn9t-server` has >1 workspace dependency. This keeps the
  dependency graph flat. Check: `scripts/check-gi1.sh`
- **GI-2:** `kn9t-core` depends only on `serde`/`serde_json`. Events are pure data.
- **GI-5:** No `tokio`, no `async`, no `.await`. OS threads only.
- **GI-6:** `kn9t-tui` never imports `kn9t-core`. It talks HTTP. Check: `scripts/check-schema.sh`

---

## 4. Schema-first development

The API contract lives in `schema/`:
- `http.json` — HTTP endpoints
- `plugin.json` — plugin protocol
- `config.json` — config file format

**Workflow:**
```bash
# 1. Edit schema/*.json
# 2. Generate code
cargo run -p xtask -- generate

# 3. Commit both schema and generated files together
```

**Generated files (never edit by hand):**
- `crates/kn9t-server/src/api.rs`
- `crates/kn9t-tui/src/wire.rs`
- `API.md`
- `docs/CONFIG.md`
- `schema/generated/go_types.go`
- `schema/generated/python_types.py`

---

## 5. Adding an HTTP endpoint

```rust
// 1. Add to schema/http.json
// 2. Run: cargo run -p xtask -- generate
// 3. Add route in kn9t-server/src/routes/

pub fn handle_my_endpoint(req: &Request, ctx: &Context) -> Response {
    let body: MyRequest = parse_json_body(req)?;
    // ...
    json_response(&MyResponse { ... })
}

// 4. Register in kn9t-server/src/router.rs
```

---

## 6. Adding a host-API operation (for plugins)

```rust
// 1. Add to schema/plugin.json under "ops"
// 2. Run: cargo run -p xtask -- generate
// 3. Add handler in kn9t-server/src/host_api.rs

"my_new_op" => {
    let params: MyOpParams = serde_json::from_value(req.params)?;
    // do work...
    Ok(json!({ "result": ... }))
}
```

---

## 7. Events

Events are the truth. One `Event` enum is:
- the SSE payload to clients
- the SQLite row in `events` table
- the input to state reconstruction

```rust
// In kn9t-core/src/event.rs
pub enum Event {
    TextDelta { delta: String },
    ToolCallStart { id: String, name: String, args: Value },
    // ...
}
```

**Rules:**
- Events are past-tense facts, not commands
- Payloads are pure `Serialize + Deserialize` — no `Arc`, no handles
- All JSON uses `snake_case`: `#[serde(rename_all = "snake_case")]`
- The `events` table is append-only

---

## 8. TUI is Lua-owned

The TUI layout is defined in Lua (`crates/kn9t-tui/assets/tui/*.lua`).
Rust provides native views and renders them where Lua says.

**Native views:** `transcript`, `input`, `status`

**Plugin UI:** Plugins send Lua source via `ui_register_lua`, not Rust widgets.
No per-plugin code in kn9t-tui.

---

## 9. Code style

**Clippy:** Don't suppress `unwrap_used`/`expect_used` at file level. Use per-call:
```rust
#[allow(clippy::unwrap_used)] // mutex poisoned = fatal
let guard = self.inner.lock().unwrap();
```

**No patches:** If a bug reveals a design flaw, fix the design. Signs you're patching:
- Adding "pending" buffers for timing issues
- Fallback logic for mismatched field names
- Duplicating code for "old" and "new" formats

---

## 10. Running tests

```bash
cargo test --workspace
cd plugins/kn9t-tools && cargo test

# CI checks
bash scripts/check-ci.sh
```

---

## 11. Key docs

| Doc | What |
|-----|------|
| `DESIGN.md` | Why decisions were made |
| `docs/ARCHITECTURE.md` | How the code is built, known issues (§14) |
| `API.md` | HTTP + plugin protocol reference |
| `spec/*.md` | Per-stage requirements |
