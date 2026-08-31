# ADR-0008: Policy is a plugin, not a core concern

## Status

Accepted

## Date

2026-08-31

## Supersedes

- **ADR-0001** (bash classifier lives in kn9t-server) — reversed: the classifier leaves the
  server entirely.
- **ADR-0006** (Policy is the single safety seam) — reversed: the seam moves to the
  `before_tool_call` hook.
- **ADR-0002** (plugins declare effects, server decides risk) — partially: `ToolSpec.effects`
  survives as *declaration* (documentation, and input a policy plugin may read), but the
  server no longer *judges* from it.

## Context

ADR-0006 funnelled every risk decision through `Policy::check(call, cwd) -> Decision` on the
server, with `dispatch_effects` combining per-`Effect` verdicts and delegating `Shell` to
`classify()` (ADR-0001, `crates/kn9t-server/src/classify.rs`, 333 lines, two shell grammars).

That design has held for one release and shown a structural problem: **it puts a judgement in
a layer that only has a declaration.**

`ToolSpec.effects: Vec<Effect{field, kind}>` says *what a tool touches* — `bash` touches
`Shell` via `field="cmd"`. It cannot say *whether this particular touch is acceptable*. So
`classify()` was invented to bridge the gap, and it must reconstruct intent from a command
string with no context. Consequences observed:

- **Not customizable per user.** `[policy.bash]` in `~/.kn9t/config.toml` offers
  `allow_read`/`always_ask`/`never` pattern lists. Anything not expressible as a flat pattern
  list — "allow `git status` but ask for `git push`", "allow writes under `./target` only",
  "never touch prod after 18:00" — is not expressible at all.
- **Requires a recompile.** Changing a rule beyond the pattern lists means editing Rust and
  rebuilding.
- **Edge cases bloat the core.** Command splitting (`;`, `&&`, `||`, `|`), subshells,
  backticks, `sh -c`, `Invoke-Expression`, two grammars (POSIX + PowerShell). All of it lives
  in the core of a project whose first design goal is *minimal*, and none of it is
  context-aware.
- **The decision is not the core's to make.** Whether `rm -rf ./build` is acceptable depends
  on the user, the repo, and the moment. kn9t cannot know. Encoding a guess in Rust makes it
  both wrong and expensive to change.

The `before_tool_call` hook (R-RCT-100, DESIGN §13.3) already sits at exactly the right point
in the flow and already receives `(tool, args, cwd)`. It was however **strictly less
expressive than `Decision`**: it could answer `Allow`, `Deny{reason}` or `Replace{args}`, but
**not `Ask`** — it had no way to escalate to the user. A policy plugin therefore had to answer
`allow` in order to mean "ask", and let the Rust policy prompt as a side effect:

```python
elif r == "ask":
    result = {"action": "allow"}  # pass to kn9t's Policy
```

That is the tell: the hook was the right seam with an incomplete vocabulary, and the Rust
policy was compensating.

## Decision

**`before_tool_call` is the single risk seam. The core has no policy.**

1. `HookVeto` gains `Ask { reason }`, making the hook's vocabulary a superset of the old
   `Decision`. The plugin decides `Allow` / `Ask` / `Deny` / `Replace`; the core routes.

2. **The server keeps the approval *mechanism*, not the approval *decision*.**
   `ApprovalRegistry`, `ApprovalCache`, `Event::ApprovalRequest`, `POST /approve` and the
   `once|session|always` scopes stay exactly where they are. They are the *executor* of an
   `Ask`: bus fan-out, a `Condvar` to block the turn, a write lease, and persistence to
   `~/.kn9t/config.toml`. None of that can move into a plugin — a subprocess cannot own the
   approval UI of every client, and GI-1 forbids `kn9t-react` from calling the server
   directly. So `Ask` is *decided* by the plugin and *served* by the server.

3. **Deleted from the core:** `classify.rs` (both grammars, command splitting), the
   `dispatch_effects`/`eval_effect` combiner, the `ConfigPolicy`/`AskOnMutation` decision
   modes, and the `[policy.bash]` config section. `ToolSpec.effects` remains as a
   declaration.

4. **Composition across plugins is strictest-wins:** `Deny > Ask > Allow`. Not
   first-deny-wins, so plugin load order cannot change the outcome.

5. **No policy plugin installed means every tool call executes.** This is deliberate, not an
   oversight, and kn9t does not warn about it. kn9t is a developer tool assembled from
   plugins; the user composes the behaviour they want. The core's job is to leave the door
   open (a hook, and the APIs a policy needs), not to install a guard.

## Consequences

### Accepted costs

- **A plugin can auto-approve.** This is the exact inverse of ADR-0006's central claim ("a
  plugin compromise or bug cannot auto-approve: the server still asks"). Accepted: a plugin is
  user-installed code running as the user, already able to run tools; pretending the server
  could contain a hostile plugin was a weak guarantee. It is now explicit rather than implied.
- **A stock install runs unguarded.** No policy plugin ⇒ `NoopHookHost`/`has_hook == false` ⇒
  `Allow`. See decision 5.
- **Python (or any interpreter) is on the safety path** for users who install a scripted
  policy. That is a runtime dependency of *their* configuration, not of kn9t, and stays
  outside the DESIGN §15 budget — but it does mean a policy can fail for reasons kn9t cannot
  diagnose.
- **`before_tool_call` is now on the hot path of every tool call**, with a 30 s timeout
  (`crates/kn9t-plugin/src/host.rs:40`). A slow policy plugin slows every call. Note the
  human wait for an `Ask` happens *server-side* after the hook returns, so a user thinking for
  a minute does not trip the hook timeout.

### Gains

- ~1400 lines leave the core (`classify.rs` + the policy decision machinery), against DESIGN
  §15's dependency/complexity budget.
- Rules become user-editable without a rebuild, arbitrarily context-aware (cwd, time, repo
  state, anything the script can read), and shareable as files.
- `grep -rn "classify(" crates` returning nothing is the new mechanical invariant; ADR-0006's
  "exactly one call site" check is retired.

### Failure posture

A hook that errors or times out yields `Deny` (`host.rs`, DESIGN §13.5) — unchanged, and
correct: a policy that cannot answer must not be read as permission. Distinct from *no policy
installed*, which is `Allow` by decision 5. A policy plugin whose own rule file fails to load
should answer `deny`/`ask` rather than `allow`, so that a typo in a rules file does not
silently disarm the rules; that is the plugin's responsibility, not the core's.

## Notes

`Decision`, `Policy`, `InteractivePolicy`, `ApprovalRegistry` and `ApprovalCache` are **not**
deleted — they are the `Ask` executor described in decision 2. What is deleted is every code
path that *derives* a `Decision` from tool arguments inside kn9t.
