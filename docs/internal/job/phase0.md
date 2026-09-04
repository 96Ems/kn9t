# PHASE 0 — docs scaffolding — ☑ DONE

**Committed:** `4fc0a78` — *"docs: architecture review — CONTEXT.md, 5 ADRs, correct false-green
TRACKING rows"*

---

## What shipped

| file | change |
|---|---|
| `CONTEXT.md` | **new**, 34 lines — domain glossary, 27 terms, alphabetical, each with a `DESIGN §N` pointer |
| `docs/adr/0001-bash-classifier-lives-in-server.md` | new |
| `docs/adr/0002-plugins-declare-effects-server-decides-risk.md` | new |
| `docs/adr/0003-dry-run-is-preview-not-safety-input.md` | new |
| `docs/adr/0004-plugin-discovery-user-dir-only.md` | new |
| `docs/adr/0005-api-contract-schema-first.md` | new |
| `TRACKING.md` | corrected false greens + realigned Stage 07 + arch-review entry |
| `CHANGELOG.md` | session narrative + "Discovered bugs" table |
| `crates/kn9t-plugin-sdk/src/ctx.rs` | **+`pub fn KvClient::for_test()`** (fixes F11) |
| `plugins/kn9t-custom-provider/src/client.rs` | test helper uses `KvClient::for_test()` |

## TRACKING.md rows changed

| row | before | after |
|---|---|---|
| R-TOOL-070 | `☑` | `✗` |
| R-TOOL-080 | `☑` (test `tool::classify_posix, classify_pwsh`) | `✗` **DELETED — tests do not exist** |
| R-TOOL-090 | `☑` (test `tool::classify_pipeline`) | `✗` **DELETED — tests do not exist** |
| R-TOOL-095 | `☑` | `✗` |
| **GATE G1** (R-RCT-900/R-TOOL-900) | `☑` | `✗` |
| stage 03 progress | `24/25 · G1 · ☑` | `20/25 · G1 · ✗ (classifier deleted)` |
| stage 07 progress | `6/7 · G3 · ▣` | `2/27 · G3 · ▣ (most reqs have no test)` |
| Stage 07 table | 7 obsolete rows `R-TUI-010..070` | 27 rows realigned to `spec/07-tui.md` (R-TUI-010..240 + R-TUI-900) |
| test count | `285 passed` | `385 passed` ← **WRONG, see below** |

## ⚠ Known defect introduced by Phase 0

The `385 passed` figure I put in `TRACKING.md` was **my measurement error**. Real measured count
is **360**. Next actor must correct this — see `job/tracking.md` → "Test count" and
`job/findings.md` → F13 for the unresolved 3-failure question.

## Verification performed

- `cargo check -p kn9t-plugin-sdk` → clean
- `cd plugins/kn9t-custom-provider && cargo test` → **26 passed** (was: test target would not compile)
- `cargo test --workspace` → no failures observed at the time (count later found unreliable)
- pre-commit hook ran `scripts/check-gi1.sh` → GI-1 OK

## Not done / deliberately deferred

- The CRLF-churn files were left unstaged on purpose (see `job/tracking.md`).
- No `.rs` behaviour changed apart from the additive `KvClient::for_test()`.
