# docs/internal

Working notes, not user documentation. Kept because they explain *why* parts of
the codebase look the way they do; nothing here is a supported interface.

| path | what it is |
|---|---|
| `job/` | a finished multi-session architecture cleanup (phases 0–5, all landed) |

`job/findings.md` is the most useful file: every defect it lists was verified
against source at the cited `file:line`, and several are cited by ADRs. The
phase files are the plan those findings produced.

Two things to know before trusting a number in here:

- Some notes are in French; the `CHANGELOG.md` narrative is too.
- Status lines are frozen at the time of writing (`IN PROGRESS`, test counts).
  Live status is `TRACKING.md` at the repo root; the narrative is `CHANGELOG.md`.

For the release-facing docs see [`../ARCHITECTURE.md`](../ARCHITECTURE.md),
[`../adr/`](../adr), [`../../API.md`](../../API.md) and [`../../spec/`](../../spec).
