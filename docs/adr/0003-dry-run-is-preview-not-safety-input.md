# ADR-0003: Dry-run is a preview mechanism, never a safety input

## Status

Accepted

## Date

2026-08-30

## Context

It was proposed that the server dry-run a tool call to discover its "blast radius" before
deciding whether to allow, ask, or deny. The idea: instead of statically analyzing
`bash "rm -rf /tmp/build"`, actually invoke the tool in dry-run mode and observe what it
*would* do.

## Decision

**Rejected as a safety mechanism.**

Dry-run is accepted only as a **diff preview** in the approval overlay (spec/07-tui.md
R-TUI-130 requires a diff preview for edits). It is never an input to the allow/deny
decision.

### Reasons

1. **You cannot dry-run destruction to learn it is destructive.**
   Determining the blast radius of `rm -rf /` requires examining the filesystem the same
   way `rm` would. This is equivalent to executing — you cannot "safely preview" deletion
   without listing what would be deleted, which is the dangerous part. Static analysis
   (the classifier) is the only option for shell commands.

2. **TOCTOU — the dry-run result is a claim by the plugin.**
   A plugin may behave differently in dry-run than in the real call. If the server trusts
   the dry-run output, a malicious plugin reports "I would only read foo.txt" and then
   executes `rm -rf ~`. This reintroduces self-approval (ADR-0002's non-solution 1).

3. **Doubles latency and side-effect risk on every gated call.**
   Even if dry-run were trustworthy, running every bash command twice (once dry, once
   real) doubles wall-clock time and doubles the chance of hitting rate limits, network
   failures, or other side effects. For `curl`, `git push`, or any network command, a
   dry-run *does* have side effects.

### What dry-run IS useful for

The approval overlay can show a diff preview for `edit` and `write`: "here is what the
file would look like." This is display-only — the user sees the diff, decides, and the
real write happens only after approval. The diff is computed by the tool, not trusted as
a safety input.

## Consequences

- Future architecture reviews should not re-propose dry-run-as-gate.
- The TUI's approval overlay may call a preview endpoint for display purposes, but the
  decision still comes from the classifier and user consent.
- The plugin protocol does not gain a `dry_run` flag with safety semantics.
