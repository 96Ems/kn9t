# PHASE 1 — restore the classifier and the approval loop

**Goal:** close the open `sh -c 'rm -rf /'` bypass and make approvals real and tunable.
**Depends on:** nothing. **Blocks:** nothing (Phase 2 is independent).
**Read first:** `job/findings.md` F1, F3, F4; `docs/adr/0001`, `0002`, `0003`; DESIGN §10 + §10.1.

---

## Step 1.1 — Restore `classify.rs` into `kn9t-server` — ▣ DONE, UNCOMMITTED

**Already done by me. Review and commit before starting 1.2.**

Working tree:
```
?? crates/kn9t-server/src/classify.rs    NEW 333 lines
?? crates/kn9t-server/tests/classify.rs  NEW  88 lines
 M crates/kn9t-server/src/lib.rs         added `pub mod classify;` after `pub mod bus;`
```

- Source recovered verbatim: `git show 5b65819^:crates/kn9t-tools/src/classify.rs` (323 lines).
- Only change vs original: module header gained a "Why this lives in kn9t-server (ADR-0001)"
  paragraph and an explicit R-TOOL-095 note that no downstream code may treat `AllowReadOnly` as
  a security guarantee. **Pipeline logic, tables, and public API are byte-identical in behaviour.**
- Public API: `Shell{Posix,PowerShell}`, `Classification{AllowReadOnly,Ask,HardDeny(String)}`,
  `BashPolicy{allow_read,always_ask,never,allow_read_sub}`, `classify(cmd,shell,policy)`.
- Tests recovered verbatim from `git show 5b65819^:crates/kn9t-tools/tests/acceptance.rs`
  (lines 231/241/257). All assertions preserved.

**Verified:**
```
cmd.exe /c "cargo test -p kn9t-server --test classify"
→ running 3 tests … test result: ok. 3 passed; 0 failed
```
Covers all 7 pipeline rules, both grammars, `sh -c 'rm -rf /'`, `bash -c`,
`iex 'Remove-Item x'`, `Invoke-Expression`, and both `HardDeny` cases.

**Test-name note for TRACKING.md:** the spec names these `tool::classify_*` but they now live in
the server crate. Real invocation is `cargo test -p kn9t-server --test classify`, test names
`classify_posix` / `classify_pwsh` / `classify_pipeline`. Record the real path, not the spec's.

**Commit suggestion:** `fix(server): restore bash safety classifier deleted in 5b65819`
Then flip TRACKING R-TOOL-080/090 to `☑` with the real test path. **Leave R-TOOL-070 and G1 at
`✗`** — 070 is "bash defers to policy", which is not true until step 1.3 lands.

---

## Step 1.2 — Widen `Decision` with `Ask` and `HardDeny`

**File:** `crates/kn9t-core/src/traits.rs:132-137`

Today:
```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum Decision { Allow, Deny { reason: String } }
```
DESIGN §10 needs a third state, and R-TOOL-090 rule 6 needs `HardDeny` distinct from an askable
deny (a `never` match must **not** be presented as an approval prompt).

- Add `Ask` and `HardDeny { reason: String }`.
- This is **core vocabulary** — it fans out to every `match` on `Decision`.
- **`kn9t-react` should need NO change.** Verify: `crates/kn9t-react/src/exec.rs:341-356`
  (`after_hook_policy`) matches on `Decision`; it already calls `policy.check()` and blocks,
  exactly as DESIGN §10 intends. Adding variants will make that `match` non-exhaustive — handle
  the new arms there, but **do not restructure the call site**; the seam is correctly placed.
- Keep `#[serde(rename_all = "lowercase")]` — AGENTS.md §12 requires snake_case on the wire.
- Update `AllowPolicy` (`crates/kn9t-server/src/state.rs:33-37`) only as needed to compile.

**Risk:** this is the widest-fanout change in Phase 1. Do it *after* 1.1's tests are green so a
regression is attributable.

**Verify:** `cargo check --workspace`, then `cargo test -p kn9t-core -p kn9t-react -p kn9t-server`.

---

## Step 1.3 — Implement `ConfigPolicy` and `InteractivePolicy`

DESIGN §10 specifies a two-impl table; **zero of the two exist.**

- **`ConfigPolicy`** — instant verdict from `[policy]` config (needs step 1.4). Used by `-p`/CI.
- **`InteractivePolicy`** — emits `Event::ApprovalRequest` to the bus, then **blocks on a
  condvar** until resolved.

**Critical constraint from DESIGN §10:** *"Resolution travels the **command** path, never the
bus. The bus stays reply-free and Principle 3 holds."* So `/approve` must resolve via a registry,
not by publishing a reply event.

Wire `InteractivePolicy` to call `classify::classify()` for the `Shell` effect. Where the
classifier says:
- `AllowReadOnly` → `Decision::Allow`
- `Ask` → emit `ApprovalRequest`, block
- `HardDeny(r)` → `Decision::HardDeny{reason:r}` — **never prompt**

This is what makes `Policy` a **real seam** (3 adapters instead of 1).

Replace `AllowPolicy` as the default in `ServerState` (`state.rs`) per `[policy] mode`.

**Verify:** new test `srv::approve_resolves_blocked_policy` — a turn blocks on an `Ask`, an
`ApprovalRequest` reaches an SSE subscriber, `POST /approve` unblocks it. Also assert a `never`
command yields `HardDeny` **without** emitting `ApprovalRequest`.

---

## Step 1.4 — Parse the `[policy]` config section

**File:** `crates/kn9t-server/src/config.rs` — currently has **no** policy section at all
(F3). The exact TOML shape is in DESIGN §10.1 (~line 1595). Reproduce it faithfully:

```toml
[policy]
mode = "ask_on_mutation"   # | "allow_all" | "deny_all" | "readonly"

[policy.bash]
allow_read    = [ ... ]
always_ask    = [ ... ]
never         = [ ... ]

[policy.bash.allow_read_sub]   # MUST come last — see DESIGN §10.1 comment
git   = [ ... ]
cargo = [ ... ]
npm   = [ ... ]
```

- `BashPolicy` already has exactly these four fields — map straight onto it.
- Absent config → `BashPolicy::default()` (already mirrors DESIGN's example).
- **Global config only** (`~/.kn9t/config.toml`). Project-local `.kn9t.toml` must **not** be able
  to widen policy — same reasoning as R-PLUG-100.
- Also seed the section into the first-run config template (P1-A bootstrap already writes one).

**Verify:** a test that a TOML `never = ["mytool"]` produces `HardDeny` for `mytool`.

---

## Step 1.5 — Wire `POST /approve` to actually resolve; honor `scope`

**Files:** `crates/kn9t-server/src/routes/session.rs:360-370`,
`crates/kn9t-server/src/turn.rs:30,39,71-73`

Today `record_approval` inserts into `static APPROVALS` that **nothing reads** (F1), and
`decision == "allow"` means the TUI's `"always"` is **silently a deny** (F4).

- Delete the dead `APPROVALS` map; replace with a resolution registry the blocked
  `InteractivePolicy` waits on (condvar / `SyncSender` keyed by `ApprovalId`).
- Parse `decision` **and** `scope`:
  - `once` → resolve this call only
  - `session` → resolve + cache in an in-memory `ConfigPolicy` overlay for that session
  - `always` → resolve + **write back to `~/.kn9t/config.toml`**
- Accept the TUI's existing three values. **Decide and document** the mapping: the TUI sends
  `"allow" | "always" | "deny"` as `decision` (`app.rs:1052,1057-1059`) with no separate `scope`
  field. Either (a) treat `"always"` as `decision=allow, scope=always`, or (b) change the TUI to
  send `{decision, scope}` properly. **(b) is more correct** and Phase 2's schema will force the
  issue — prefer (b), and note it so Phase 2 picks it up.
- Return a real error for an unknown `decision` rather than defaulting to deny. Today
  `unwrap_or("deny")` (`session.rs:362`) hides typos.

**Verify:** `srv::approve_always_writes_config`; a test that `"always"` does **not** deny.

---

## Step 1.6 — Add `effects` to `ToolSpec`

**Files:** `crates/kn9t-core/src/toolspec.rs:10-20`, `crates/kn9t-plugin-sdk/src/wire.rs:76-92`,
`crates/kn9t-server/src/tools.rs:166-179` (`extract_tools` builds the core `ToolSpec` from the
plugin's).

Per ADR-0002:
```rust
pub struct Effect { pub field: String, pub kind: EffectKind }
pub enum EffectKind { Shell, FsWrite, FsRead, Network }
```
- `bash` declares `{field:"cmd", kind:Shell}` → server runs `classify()` on that field.
- `write`/`edit` declare `{field:"path", kind:FsWrite}` → server checks path rules. DESIGN §10
  demands this: *"`write` and `edit` are gated exactly as hard as `bash`."*
- **A tool declaring no effects → strictest `[policy] mode` default.** This is the anti-lying
  property; do not weaken it to "allow".
- `#[serde(default)]` so older plugins still handshake, landing in the strict default.
- Note the two `ToolSpec` types are **separate** (SDK has no workspace deps — GI-1). Both need
  the field, and `extract_tools` must map it across.

**Also update:** `crates/internal-plugins/kn9t-tools/src/{bash,read,write,edit}.rs` to declare
their effects — but note Phase 3 **moves** these files, so coordinate ordering.

**Verify:** a tool with no declared effects gets `Ask`/deny under `ask_on_mutation`; `bash`'s
`cmd` field routes through the classifier.

---

## Step 1.7 — ADR-0003 is already written

`docs/adr/0003-dry-run-is-preview-not-safety-input.md` shipped in Phase 0. Nothing to do unless
implementing the *preview* feature, which belongs with the approval overlay in Phase 4.

---

## Phase 1 exit criteria

- [ ] `cargo test -p kn9t-server --test classify` → 3 passed
- [ ] `sh -c 'rm -rf /'` reaches an approval prompt end-to-end (not just a unit test)
- [ ] `sudo rm -rf /` is `HardDeny` and produces **no** prompt
- [ ] `[policy]` in `~/.kn9t/config.toml` demonstrably changes behaviour
- [ ] `"always"` no longer records a deny; `scope=always` persists to config
- [ ] `static APPROVALS` is gone
- [ ] TRACKING R-TOOL-070/080/090/095 honestly re-evaluated; **G1 re-assessed**
- [ ] CHANGELOG entry with any SHOULD-deviations recorded (AGENTS.md §6)
