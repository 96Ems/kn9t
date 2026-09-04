# PHASE 3 — all plugins external, auto-discovered

**Goal:** no `internal-plugins/`. Every plugin — including the core tools — is external and
auto-discovered from the user's plugin directory, with config able to override.
**Depends on:** Phase 2 (so new endpoints are schema-conformant). Coordinate with Phase 1 step
1.6, which edits files this phase **moves**.
**Read first:** `docs/adr/0004`; `spec/08-plugin.md` R-PLUG-100; `spec/08b-plugin-redesign.md`
R-PLUG2-100/110; `job/findings.md` F10, F13.

---

## ⚠ Security constraint — non-negotiable (ADR-0004)

Auto-discovery scans **`~/.kn9t/plugins/`** and **never** a project-relative `plugins/`.

`spec/08-plugin.md` R-PLUG-100 exists precisely so that *"a repo-committed file must not run
arbitrary binaries; `git clone` then `kn9t` must not be code execution."* It forbids honoring
`[[plugin]]` from a project-local `.kn9t.toml`. **Scanning a project-relative directory for
executables re-opens exactly that hole by a different route.**

- repo `plugins/` = **build source**
- `~/.kn9t/plugins/` = **install target**

---

## Step 3.1 — Move the three internal plugins out

Currently `crates/internal-plugins/` holds `kn9t-tools`, `kn9t-anthropic`, `kn9t-test-plugin`
(note `kn9t-custom-provider` already moved to `plugins/`).

- Move all three to `plugins/`.
- Remove from workspace members — `Cargo.toml:15-17`.
- Each becomes standalone like `plugins/kn9t-custom-provider` (own `Cargo.lock`, empty `[workspace]` stanza).
- They already depend only on `kn9t-plugin-sdk`, so GI-1 is satisfied; `scripts/check-gi1.sh`
  already globs `plugins/*/Cargo.toml`, so keep that working.

**Two consequences to handle:**
1. `crates/kn9t-react/tests/support/mod.rs:300-320` locates the tools binary by walking up to
   `kn9t/target/{debug,release}`. That breaks. **This is also F13** — those 3 tests
   (`hook_posture`, `turn_sequence`, `parallel_order`) already fail in a fresh worktree for this
   reason. Fix the lookup properly here rather than patching around it.
2. Nothing in the workspace build will produce these binaries any more — CI/dev docs need a
   build step. Decide and document how a dev gets them into `~/.kn9t/plugins/`.

---

## Step 3.2 — Auto-discovery

Delete `locate_tools_binary()` (`crates/kn9t-server/src/tools.rs:20-43`) and the
`spawn_builtin_tools()` special case (`tools.rs:45-64`). Today the server hardcodes a
`KN9T_TOOLS_BIN` env override → sibling-of-exe → PATH lookup, and always auto-spawns
`kn9t-tools`.

Replace with: scan `~/.kn9t/plugins/` at startup, handshake each discovered binary, merge tools
into one `ToolRegistry` (`spawn_all_plugins` at `tools.rs:191` already merges — reuse it).

- Soft-fail per plugin (existing behaviour at `tools.rs:210-215` — keep it).
- Bootstrap installs the default tools into `~/.kn9t/plugins/` on first run (P1-A already creates
  `~/.kn9t/` with a config template + token; extend it).
- Decide: what if the dir is empty? A server with **no** `read`/`write`/`edit`/`bash` is nearly
  useless. Fail loudly at startup, or warn and continue? R-PLUG2-110 currently says startup MUST
  fail if the tools binary is missing — see step 3.4.

---

## Step 3.3 — Config overrides discovery

`[[plugin]]` (already parsed — `RawPlugin` at `config.rs:77-86`: `name`, `cmd`, `env`) should be
able to:
- disable a discovered plugin
- pin an explicit path (overriding discovery)
- inject env vars

**Global config only.** R-PLUG-100 unchanged: a `[[plugin]]` in a project-local file is ignored
with a warning, and there is an existing test `plug::project_plugin_ignored` — keep it passing.

---

## Step 3.4 — Update the specs

`spec/08b-plugin-redesign.md` **R-PLUG2-110** currently mandates the exact mechanism being
deleted: *"kn9t MUST auto-spawn `kn9t-tools` at server startup… The binary path MUST be resolved
relative to the kn9t executable (same directory). If the binary is not found, startup MUST fail."*

Rewrite it for discovery. Per AGENTS.md §9, a spec that contradicts the new design is a **spec
bug to record**, not something to silently work around — put it in CHANGELOG's "Discovered bugs".

Also update:
- `spec/README.md:29-31` (the stage table references `internal-plugins/kn9t-tools`)
- `DESIGN.md` §16 build-order diagram node `S8b`
- `crates/kn9t-server/src/lib.rs:12-13` doc comment mentions the `kn9t-tools` plugin path

---

## Step 3.5 — Implement `POST /plugin/{name}/reload`

Specified in R-PLUG2-100 with an exact 5-step sequence; **verified absent** (F10). Discovery makes
it more valuable — drop a binary in, reload, no restart.

1. `{"t":"cancel","id":N}` for every in-flight call on that plugin
2. wait up to the `before_tool_call` timeout for `done` replies
3. `{"t":"shutdown"}`, close the write pipe
4. respawn from the same `cmd`
5. re-handshake; re-register tools, provider, hooks, event subscriptions

In-flight calls that miss step 3 get a synthetic error result; the model sees a retryable tool
error. Acceptance test named in the spec: `plug2::hot_reload_cancels_inflight`.

Add the route to the schema (Phase 2) so it is born conformant.

---

## Phase 3 exit criteria

- [ ] `crates/internal-plugins/` no longer exists
- [ ] `Cargo.toml` members list has no `internal-plugins` entries
- [ ] server discovers and spawns tools from `~/.kn9t/plugins/`
- [ ] a test proves a **project-relative** `plugins/` dir is **NOT** scanned (ADR-0004)
- [ ] `plug::project_plugin_ignored` still passes
- [ ] F13's three react tests pass from a **clean** `target/`
- [ ] `POST /plugin/{name}/reload` works; `plug2::hot_reload_cancels_inflight` passes
- [ ] R-PLUG2-110 rewritten; spec/README + DESIGN §16 updated; spec bug recorded in CHANGELOG
- [ ] `scripts/check-gi1.sh` still OK
