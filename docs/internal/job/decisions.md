# DECISIONS — what was chosen and why

Records the decisions taken during the 2026-08-30 review session, so no later agent
re-litigates them. Five are formalized as ADRs in `docs/adr/` (committed `4fc0a78`).

---

## User-stated requirements (verbatim intent)

1. **"internal plugin bash must be deleted. no more internal plugins. everything land in
   plugins folder even tools."**
2. **"true external tools. auto parse kn9t plugin folder for every plugin. tools delivered by
   default in this folder. config to override auto discovering."**
3. **"the sdk must be improved and api doc also. maybe we could improve mechanisme for every
   next clients?"**
4. **"maybe improve server handling to throw errors to every clients and plugin when not
   matching the required api doc?"**
5. **"people would like to be able to tune and tweaks approvals."**
6. **"for the tui having a god app.rs object. maybe we need to revise tui architecture to follow
   same principles as server ones to make it expandable via plugins respecting an sdk."**
7. **"missing endpoint for clients needs to add them all and remove hardcoded tool name in tui
   doing nothing."**
8. On DESIGN's rejections: **"maybe this is not rejected for good reasons we can revise the
   design. design was the FIRST document of the code base. things have evolved and there is no
   user yet so we can revise."**

---

## Q&A decisions

### Approval risk mechanism → **`effects` on ToolSpec** (ADR-0002)

The server can no longer introspect plugin tool arguments — it sees an opaque
`{"tool":"x","args":{...}}`. Options considered:

- ~~Plugin declares `needs_approval`~~ — rejected: **self-approval hazard.** A careless or
  hostile plugin marks everything safe.
- ~~Server keys checkers off tool *name*~~ — rejected: hardcodes knowledge of specific names;
  any third-party tool is unclassifiable forever.
- **CHOSEN:** `ToolSpec` gains `effects: [{field, kind}]`, `kind ∈ {Shell, FsWrite, FsRead,
  Network}`. Server maps kind → checker. Plugin **describes**; server **decides**; user **tunes**
  via `[policy]` + approval scope. A tool declaring **no** effects falls to the strictest
  `[policy] mode` default — so lying is never profitable; a plugin can only *widen* what the
  server inspects, never narrow it.

Breaking plugin-protocol change. Acceptable **now**, while `proto == 1` and no external plugin
authors exist.

### Dry-run → **preview only, never a safety input** (ADR-0003)

The user asked whether dry-run could discover a tool's blast radius. Rejected as a *gate*:
1. You cannot dry-run `rm -rf /` to learn it is destructive — determining blast radius is
   equivalent to executing.
2. TOCTOU: a plugin may behave differently in dry-run than in the real call, so the result is a
   **claim by the plugin** — trusting it reintroduces self-approval.
3. Doubles latency and side-effect risk on every gated call.

**Accepted** as an approval-overlay diff preview — `spec/07-tui.md` R-TUI-130 already requires
"for edits: diff preview". Good UX, not a decider.

### Approval scope → **full DESIGN §10: once | session | always, with config writeback**

`once` → this call. `session` → in-memory `ConfigPolicy` overlay. `always` → write
`~/.kn9t/config.toml`. This is the tune-and-tweak loop the user asked for, and it fixes F4.

### API contract → **schema-first; API.md becomes generated** (ADR-0005)

Generation runs **schema → code**, not code → schema.

**Why not `schemars`** (currently rejected in DESIGN §15): it generates JSON Schema *from Rust
types* — the wrong direction. It could never serve the existing **Go** and **Python** plugins,
which hand-roll the wire format today (`plugins/kn9t-agents-md/main.go:44` literally carries a
comment warning about the flattened-body quirk). So schemars stays rejected, **for a better
reason than dep budget**, and the schema-first approach needs **zero new runtime deps** — DESIGN
§15's budget is untouched.

**Important correction:** schemars would *not* have fixed the API drift anyway. API.md rotted
because it is hand-written prose with nothing checking it. The fix is the **CI drift gate**, not
the reflection library.

**DESIGN §15/§11 must still be revised** — but on the recorded trigger, not on vibes: §15 says of
the hand-written-schema decision *"revisit if this bites."* §11 states the accepted cost as
*"renaming a param field silently breaks the schema until an integration test catches it"* — and
there is no such integration test. **It has bitten twice** (F5, F7). That is a decision with a
documented trigger condition that has now fired. → Phase 5.

### Plugin bootstrap → **auto-discover `~/.kn9t/plugins/`, config overrides** (ADR-0004)

**Security constraint, non-negotiable:** scan **`~/.kn9t/plugins/`** (user-owned, privileged)
and **never** a project-relative `plugins/`. `spec/08-plugin.md` R-PLUG-100 exists precisely so
that *"`git clone` then `kn9t` must not be code execution"* — it forbids honoring `[[plugin]]`
from a project-local `.kn9t.toml`. Scanning a project-relative directory for executables
re-opens exactly that hole by a different route.

The repo's `plugins/` is **build source**; `~/.kn9t/plugins/` is the **install target**.
Bootstrap installs default tools there on first run.

### TUI extensibility → **defer plugins; decompose `app.rs` first**

The user chose this over making the TUI a second plugin host. Rationale: do not add an
extensibility seam to a 2,814-line god object. Build the pure SSE reducer and split screen state
first; **then** plugin-contributed UI becomes a schema addition that benefits *every* client, not
a TUI rewrite.

Note `spec/07-tui.md` R-TUI-220 already specs a `SidebarWidget` data enum ("plugins return data,
TUI renders") — verified **not implemented** (no `SidebarWidget` type exists anywhere). Phase 4.5
revisits it as a server-driven capability.

### Phase order → **safety → contract → plugins → TUI**

Phase 1 closes a live security hole and depends on nothing else. Phase 2 precedes 3 and 4 so
every *new* endpoint (`/tools`, `/compact`, `/plugin/reload`) is born schema-conformant instead of
needing retrofit. Phase 3 changes spawn paths that Phase 4's `/tools` relies on. Phase 4 is last —
the only phase with no external contract surface.

---

## The five ADRs (committed in `docs/adr/`)

| ADR | title | one-line |
|---|---|---|
| 0001 | Bash classifier lives in kn9t-server, not the tool plugin | server owns approval UI, lease, config; plugins must not self-approve |
| 0002 | Plugins declare argument EFFECTS; the server decides risk | plugin describes, server decides, user tunes; no-declaration → strictest default |
| 0003 | Dry-run is a preview mechanism, never a safety input | blast radius ≈ execution; TOCTOU; result is a plugin's claim |
| 0004 | Plugin discovery scans `~/.kn9t/plugins/` only | R-PLUG-100: `git clone` must never be code execution |
| 0005 | API contract is schema-first; API.md becomes generated | one schema → server types + wire.rs + docs + Go/Py stubs; CI drift gate |

---

## Architectural framing (from the `improve-codebase-architecture` skill)

Vocabulary to use in later phases — `CONTEXT.md` for domain terms, this for architecture:

- **Seam** — where an interface lives; a place behaviour can change without editing in place.
- **One adapter = hypothetical seam. Two adapters = real seam.**
- `Policy` is currently a **hypothetical** seam: `AllowPolicy` is its only impl. Phase 1 adds
  `ConfigPolicy` + `InteractivePolicy`, making it **real**. The seam itself is already correctly
  placed — `crates/kn9t-react/src/exec.rs:352` calls `policy.check()` and blocks, exactly as
  DESIGN §10 intends, so **ReAct needs no change**.
- **The interface is the test surface.** This is why `handle_sse` must become a pure reducer
  (F8): its current interface (`&mut self` on a 45-field struct) is untestable, which is why the
  F5 and F7 bugs survived.
- **Deletion test** — imagine deleting a module. If complexity vanishes, it was a pass-through.
  The six `pending_*` fields pass this test: deleting them concentrates complexity into the
  reducer where it belongs.

---

## Missing project scaffolding (partly addressed)

- `CONTEXT.md` — **created** in Phase 0 (27 domain terms, DESIGN §refs).
- `docs/adr/` — **created** in Phase 0 (5 ADRs).
- Domain vocabulary was previously scattered across 113 KB `DESIGN.md` + 151 KB `CHANGELOG.md`
  with no single reference. That is a large part of why this drift stayed invisible.
