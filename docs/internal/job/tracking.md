# JOB TRACKING — kn9t architecture cleanup

**Created:** 2026-08-30
**Purpose:** Handoff index for a multi-session architecture cleanup. Each phase has its own
file with per-step detail. This file is the map + live status only.

---

## How to use this directory

| file | contents |
|---|---|
| `job/tracking.md` (this) | phase index, live status, session ground rules |
| `job/findings.md` | **every verified defect** with `file:line`. Read before any phase. |
| `job/decisions.md` | the Q&A decisions that shaped the plan + the 5 ADRs |
| `job/phase0.md` | docs scaffolding — **DONE, committed `4fc0a78`** |
| `job/phase1.md` | restore classifier + approval loop — **IN PROGRESS, uncommitted** |
| `job/phase2.md` | schema-first API contract |
| `job/phase3.md` | all plugins external + auto-discovery |
| `job/phase4.md` | missing endpoints + TUI decomposition |

**Do not dispatch one agent at "do job/tracking.md".** Dispatch one agent per *step* inside a
phase file. Steps are sized to be individually reviewable and committable.

---

## Ground rules (learned the hard way this session)

1. **Subagents truncate large writes.** The provider caps output size and breaks mid-tool-call.
   Instruct agents to write files in **chunks** (append successive edits), or do large writes
   yourself. This killed one dispatch already.
2. **cargo must run via `cmd.exe /c "cargo ..."`** from bash (AGENTS.md §8.1). There is no Linux
   cargo. Never call the `.exe` by its `/mnt/c/...` path.
3. **`/mnt/c/_ddm/...` in WSL is `C:\_ddm\...` on Windows** — the same files. A git worktree
   placed anywhere under `/mnt/c` IS reachable by Windows cargo.
4. **Never `cargo test --workspace` to check one thing.** It is slow (~10 min) and its output is
   easy to misread. Use `cargo test -p <crate>` or `--test <file>`. See "test count" below for
   why aggregate counts misled me.
5. **AGENTS.md §6:** a requirement is done only when its named acceptance test passes.
   "Implemented but untested" is `▣`, never `☑`.
6. **AGENTS.md §10:** no patches. If a bug reveals a design flaw, fix the design. Signs you are
   patching: pending buffers, `or_else` field-name fallbacks, dual old/new format handling.
7. **AGENTS.md §11:** the TUI is the API proving ground. Missing server capability → add the
   endpoint, never a client-side workaround. No PATCH endpoints; use action endpoints.
8. **AGENTS.md §12:** all JSON is `snake_case`, fields and enum variants.

### Files with pre-existing CRLF-only churn — DO NOT TOUCH, DO NOT COMMIT

These four are modified in the working tree with **500 insertions / 500 deletions of identical
content** (a Windows editor flipped them to CRLF). Leave them unstaged so real diffs stay
readable:

```
SPLIT.md
crates/kn9t-tui/Cargo.toml
crates/kn9t-tui/src/keybind.rs
crates/kn9t-tui/src/lib.rs
```

---

## Phase status

| phase | scope | status |
|---|---|---|
| 0 | CONTEXT.md, 5 ADRs, correct false-green TRACKING rows, SDK `KvClient::for_test()` | ☑ committed `4fc0a78` |
| 1 | Restore bash classifier; `Ask`/`HardDeny`; Config+Interactive policy; parse `[policy]`; wire `/approve` scope; `effects` on ToolSpec | ▣ **step 1.1 done but UNCOMMITTED** |
| 2 | One JSON Schema → generate server types, TUI `wire.rs`, API.md, Go/Python stubs; CI drift gate | ☐ |
| 3 | Delete `crates/internal-plugins/`; all plugins in `plugins/`; auto-discover `~/.kn9t/plugins/`; `POST /plugin/{name}/reload` | ☐ |
| 4 | `GET /tools`, `/compact`, `/export`, `/rename`; delete hardcoded tool list; pure SSE reducer; SSE reconnect | ☐ |
| 5 | Revise DESIGN §11/§15 schema decision ("revisit if this bites" — it bit twice) | ☐ |

---

## UNCOMMITTED WORK IN PROGRESS — read before starting

Phase 1 step 1.1 is **complete and passing but not committed.** Working tree:

```
?? crates/kn9t-server/src/classify.rs     (NEW, 333 lines — restored classifier)
?? crates/kn9t-server/tests/classify.rs   (NEW, 88 lines — 3 acceptance tests)
 M crates/kn9t-server/src/lib.rs          (added `pub mod classify;` after `pub mod bus;`)
```

**Verified passing:**
```
cmd.exe /c "cargo test -p kn9t-server --test classify"
→ 3 passed: classify_posix, classify_pwsh, classify_pipeline
```
This includes the `sh -c 'rm -rf /'` and `iex 'Remove-Item x'` bypass assertions and both
`HardDeny` cases.

Next actor should: review these two files, commit them, then proceed to step 1.2 in
`job/phase1.md`.

---

## Test count — READ THIS, my earlier numbers were wrong

- I reported "385 passed" early in the session. **That was wrong** — I grepped in a way that
  missed failures.
- Current measured: **360 passed** via
  `cargo test --workspace 2>&1 | grep -oE "^test result: ok\. [0-9]+" | grep -oE "[0-9]+" | paste -sd+ | bc`
- `TRACKING.md` says 285 (stale) and Phase 0 changed it to 385 — **that 385 is my bad number and
  should be corrected to the real one once someone measures it cleanly.**
- **OPEN / UNRESOLVED:** a full-workspace run produced 2 grep hits for `FAILED`. In a clean
  worktree the failures were `kn9t-react`'s `hook_posture`, `turn_sequence`, `parallel_order`,
  all panicking at `crates/kn9t-react/tests/support/mod.rs:323` with *"kn9t-tools binary not
  found … Run `cargo build -p kn9t-tools-plugin` first."* Running `cargo test -p kn9t-react`
  alone passes (12 passed) because the binary exists in the main repo's `target/`.
  → **These 3 tests depend on build order, not on source correctness.** They are a latent
  fragility that Phase 3 (auto-discovery) touches directly. Someone should confirm whether the
  main working tree currently has 0 or 3 failures, and record the honest number.

---

## Session provenance

Everything in `job/findings.md` was verified by reading source at the cited `file:line` during
this session — not inferred. Where I could not verify, it says so. Two claims I made and then
corrected are flagged in that file so they are not re-propagated:

1. "`plugins/kn9t-custom-provider` does not compile" — **wrong.** `cargo build` succeeds; only the *test*
   target failed. Fixed in Phase 0.
2. "385 tests pass" — **wrong**, see above.
