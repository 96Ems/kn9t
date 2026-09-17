# docs/dev

Development-process documentation. **Not an interface.** It records how the project was
built and why it changed; nothing here is user-facing.

| path | what it is |
|---|---|
| `CHANGELOG.md` | session narrative — what changed and why, plus discovered spec/design bugs |
| `TRACKING.md` | live scoreboard — stage progress, per-requirement test status, registers |
| `PLAN.md` | post-v1 work plan (epics P1–P7); §P7 holds the TUI-polish decisions D1–D21 |
| `tui-lua-cleanup.md` | plan for the Lua-owned TUI (Lua decides, Rust renders) |
| `job/` | a finished multi-session architecture cleanup (phases 0–5, all landed) |

`job/findings.md` is the most useful of the archived notes: every defect it lists was
verified against source at the cited `file:line`, and several are cited by ADRs.

Two things to know before trusting a number in here:

- The `job/` notes are frozen at the time of writing (`IN PROGRESS`, line counts, test
  counts). `TRACKING.md` is live; the archived notes are not.
- Some archived notes are in French, and `CHANGELOG.md` is partly French too.

For the release-facing docs see [`../ARCHITECTURE.md`](../ARCHITECTURE.md),
[`../adr/`](../adr), [`../../API.md`](../../API.md) and [`../../spec/`](../../spec).
