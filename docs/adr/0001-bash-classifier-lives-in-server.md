# ADR-0001: The bash safety classifier lives in kn9t-server, not in the tool plugin

## Status

Superseded by ADR-0008 (2026-08-31) — the classifier leaves the server entirely; risk
decisions move to a user-installed policy plugin via the `before_tool_call` hook.

## Date

2026-08-30

## Context

Stage 08b (plugin protocol v2) moved the default tools (`bash`, `read`, `edit`) to an
external subprocess plugin (`internal-plugins/kn9t-tools`). Commit 5b65819 deleted the
entire `crates/kn9t-tools/src/classify.rs` (323 lines) — the bash safety classifier that
implemented spec requirements R-TOOL-080/R-TOOL-090 (the Ask/AllowReadOnly/HardDeny
decision pipeline). The commit message claimed "migrated to internal-plugins," but the
classifier was never reimplemented.

Consequences of the deletion:

- `AllowPolicy::check()` in `kn9t-server/src/state.rs:33-37` returns `Decision::Allow`
  unconditionally. It is the **only** `Policy` impl in the workspace.
- Nothing anywhere emits `Event::ApprovalRequest`. The TUI's approval overlay
  (`kn9t-tui/src/app.rs:1994`) is dead code.
- `static APPROVALS` in `kn9t-server/src/turn.rs:30` is declared, inserted into at line 72,
  but **never read**.
- The `sh -c 'rm -rf /'` bypass that R-TOOL-090 rule 5 exists to close is currently open.

The question is: where should the classifier live?

**Option A: In the tool plugin.** The plugin already owns `bash` execution. But:
- A plugin declaring its own "needs_approval" would be self-approval — a careless or
  malicious plugin marks everything safe.
- The server cannot trust any safety claim from a subprocess it does not control.
- The plugin does not have access to user config (`[policy]` in `~/.kn9t/config.toml`).

**Option B: In the server.** The server owns:
- The approval UI (SSE `ApprovalRequest`, `/approve` route).
- The write lease (only the lease holder can approve).
- The user's config file (trusted, privileged).
- The decision whether to even show a prompt (vs. hard deny).

## Decision

The bash safety classifier lives in **kn9t-server**, not in the tool plugin.

The server already has the `Policy` trait and the plumbing for `ApprovalRequest` events
and the `/approve` route — they are just dead code because the only `Policy` impl
(`AllowPolicy`) unconditionally allows. Restoring the classifier means:

1. Parsing the `[policy]` section from config (currently documented in DESIGN §10.1 but
   never parsed).
2. Implementing a `ConfigPolicy` that runs the classifier (DESIGN §10.1 decision pipeline).
3. Implementing an `InteractivePolicy` that emits `ApprovalRequest` and blocks on a
   condvar until `/approve` arrives.
4. Wiring one or the other based on config / mode (`-p` → `ConfigPolicy`, interactive →
   `InteractivePolicy`).

## Consequences

- The `Policy` trait gains a real second and third adapter (currently it has exactly one,
  making it a hypothetical seam). The trait becomes load-bearing.
- The server must be able to inspect tool arguments to run the classifier. For internal
  tools this is trivial (the server dispatches them). For plugin tools, this motivates
  ADR-0002: plugins must declare argument effects so the server knows *what* to inspect.
- TRACKING.md's R-TOOL-070/080/090/095 rows must be flipped from `☑` to `✗` until the
  classifier is restored.
- Gate G1 is no longer green for the classifier requirements.
