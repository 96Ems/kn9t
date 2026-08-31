# ADR-0006: Policy is the single safety seam

## Status
Accepted

## Date
2026-08-31

## Context
Risk decisions for tools must not be duplicated. Early `kn9t-tools` (in-process) classified
`bash` commands itself, but after the migration to a subprocess plugin (`plugins/kn9t-tools`,
ADR-0004 phase 3) a plugin that classifies its own calls would be self-approving.

A careless or hostile plugin could mark every command `AllowReadOnly` (e.g. `sh -c 'rm -rf /'`
classified as safe) and the server would have no second gate.

The store owns token counts and persistence, the provider owns streaming, but none owns approval UI,
the write lease, or user config. All three are server concerns.

## Decision
**All risk decisions funnel through `Policy::check(call, cwd) -> Decision` on the server.**

- Tools declare `ToolSpec.effects: Vec<Effect{field, kind}>` where `kind` is `Shell|FsRead|FsWrite|Network`
  (ADR-0002). The plugin is authoritative about *what* it touches (field), the server about *whether*
  that touch is safe.
- `dispatch_effects` (`crates/kn9t-server/src/policy.rs:375`) is the sole combiner (`HardDeny > Ask > Allow`);
  `eval_effect` for `Shell` on `field="cmd"` delegates to `classify(cmd, Shell::Posix, &bash_policy)`
  (`crates/kn9t-server/src/classify.rs`, ADR-0001). `FsRead`→`Allow`, `FsWrite`/`Network`→`Ask`,
  unknown tool or empty `effects`→`Ask` (strict). This is the **only** call site for `classify`.
- `ConfigPolicy` (non-interactive, `-p`/CI) maps `Ask→Deny` instantly; `InteractivePolicy` emits
  `Event::ApprovalRequest` on the bus and blocks on a `Condvar` until `POST /approve {id, decision, scope}`
  resolves via `ApprovalRegistry` (command path, not bus). `HardDeny` never prompts and is never cached;
  `scope=session` is in-memory, `scope=always` persists under `[policy.approvals]` in `~/.kn9t/config.toml`.
- **No tool, plugin, provider, or TUI may duplicate this logic.** `kn9t-tools`'s `bash` impl is
  `Shell` with no local allowlist; `read` is `FsRead`, `write`/`edit` are `FsWrite`. The
  classifier does not live in the plugin (GI-1 would force `kn9t-plugin-sdk` to carry the 333-line
  two-grammar classifier) and the TUI's former `tools[i].enabled` toggle was dead code and has been
  removed (now `GET /tools` refresh).

## Consequences
- A plugin compromise or bug cannot auto-approve: the server still asks.
- Adding a new tool is adding an `effects` entry; the server's policy gains coverage without code change.
- `ApprovalCache` makes `once|session|always` a server concern, so every client (TUI, `kn9t -p`, browser)
  shares the same semantics.
- The invariant is mechanical: `grep -rn "classify(" crates --include="*.rs"` must return only
  `crates/kn9t-server/src/policy.rs` (plus `classify.rs` itself and tests). A second call site is a bug.
