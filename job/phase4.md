# PHASE 4 — missing endpoints, then decompose the TUI

**Goal:** add every capability the TUI currently fakes; then make `app.rs` testable so future
extensibility is possible.
**Depends on:** Phase 2 (endpoints must be schema-conformant), Phase 3 (`GET /tools` reflects
discovered plugins).
**Read first:** `job/findings.md` F8, F9; AGENTS.md §10 + §11; `spec/07-tui.md`.

**User decision:** TUI *plugins* are **deferred**. Do not add an extensibility seam to a
2,814-line god object. Decompose first (4.4), then revisit widgets (4.5).

---

## Step 4.1 — Add the missing endpoints

Per AGENTS.md §11: missing capability → **add the endpoint**, never a client workaround. No PATCH;
use action endpoints or full replacement.

| endpoint | why | notes |
|---|---|---|
| `GET /tools` | TUI **hardcodes** 4 tool names (`app.rs:188-192`) | registry already lives in `ServerState.tools`; **never exposed**. Plugins can register tools the TUI can never show — a correctness bug, not just cosmetics. After Phase 3 this must reflect discovered plugins. |
| `POST /session/{id}/compact` | `/compact` prints *"planned for a future release"* (`app.rs:2638`, `:2735`) | **the engine already exists**: `crates/kn9t-react/src/exec.rs:139` `run_compaction`. Only unreachable. Emits `Event::Compacted`. |
| `GET /session/{id}/export` | `/export` prints "planned" (`app.rs:2643`, `:2739`) | decide format(s) |
| `POST /session/{id}/rename` | no UI at all | AGENTS.md §11's own worked example. `sessions.name` column exists; `create` already writes it (`routes/session.rs:41-45`). Auto-title (R-SRV-100) must not clobber a manual rename — it already checks for an existing name (`turn.rs:174-176`). |

Also consider `POST /session/{id}/rewind` — DESIGN treats rewind as a first-class fork reason
(`ForkReason::Rewind`) and `POST /session/{id}/fork` already accepts it, but nothing calls it.

**Do NOT add** a server-side `/diff`. `/diff` shelling out to local `git` (`app.rs:2767`) is
arguably legitimate client-local behaviour — **but** it uses `env::current_dir()` instead of the
session's `cwd`, which is a real bug. Fix it to use the session `cwd` from the snapshot.

---

## Step 4.2 — Delete the hardcoded tool list and the dead toggle

- `crates/kn9t-tui/src/app.rs:188-192` — hardcoded `bash/read/write/edit`.
- `app.rs:1782` — clicking a tool flips `self.tools[i].enabled`, and **nothing ever reads
  `enabled`.** Pure theatre.

Drive the sidebar from `GET /tools`. If per-tool enable/disable is genuinely wanted, it needs a
server endpoint — otherwise **remove the toggle** rather than leave a control that lies.

---

## Step 4.3 — Wire up endpoints that already exist but are unused

The TUI never calls these, though `client.rs` has methods for some:
- `POST /session/{id}/steer` — **no client method at all.** Steering is core to DESIGN §9.
- `POST /session/{id}/fork` — no client method. Fork/rewind is core to DESIGN §7.
- `GET /cost`, `GET /budget` — no client methods; the TUI computes cost from `UsageRecorded`
  events instead (`token_tracker.rs`).
- `GET /health` — unused.
- `client.rs:180 upload_blob` and `client.rs:202 set_pref` exist; `upload_blob` appears **never
  called** (images go inline via `prompt`'s `images[]` instead). Confirm and either use it or
  remove it.

---

## Step 4.4 — Decompose `app.rs` — the main event

**The problem (F8):** `app.rs` is 2,814 lines / ~45 fields. The TUI crate is 13,935 lines — larger
than server + store + core + react **combined**. `handle_sse` (`app.rs:1886-2019`) is the most
important function in the crate and has **zero tests**; it can only be exercised by constructing
the whole `App`.

**4.4a — Extract a pure SSE reducer.** `(state, frame) -> state`, no `&mut self`, no terminal, no
server, no I/O. *The interface is the test surface* — this is the single highest-leverage change in
the whole cleanup. **A pure reducer would have caught F5 and F7 immediately.**

Handle the three frames currently **ignored**: `ThinkingDelta`, `ModelChanged`, `Compacted` have
no arm in `handle_sse` (only their `seq` is recorded). `Compacted` especially — the transcript
should reflect a compaction.

**4.4b — Split screen state.** `Welcome` vs `Chat` share one struct; `Overlay` is one field with 7
variants. Separate them.

**4.4c — Eliminate the six `pending_*` fields** (`app.rs:206-211`). AGENTS.md §10 names "pending
buffer" as a patch smell. They exist because keybinds mutate state the render loop later
reconciles; a reducer removes the need. Apply the **deletion test**: removing them should
*concentrate* complexity in the reducer, not scatter it.

**4.4d — Implement `R-TUI-230` SSE reconnect.** Today `client.rs:296-330` breaks out of the read
loop and **never retries**; `app.rs:636` prints `"Connection interrupted, reconnecting..."` and
lies. The spec requires reconnect from last-known `seq` — the machinery exists
(`session_manager.rs:115-122` already opens `?from=<seq>`).

**4.4e — Consider splitting the crate.** `ui/render.rs` (1,930) and `diff_viewer.rs` (1,310) are
plausible module boundaries. Judgement call; do not over-fragment (the skill warns against
shallow modules — bouncing between many small files is its own friction).

---

## Step 4.5 — Then revisit `R-TUI-220` SidebarWidget

`spec/07-tui.md` R-TUI-220 already specs the right shape — a data enum (`Section`, `KeyValue`,
`List`, `Toggle`, `Tree`, `Button`) where *"plugins return data, TUI renders"*, with the explicit
note **"No custom rendering."** Verified **not implemented** (no `SidebarWidget` type anywhere).

Once widgets arrive as server data (like `GET /tools`), plugin-contributed UI is a **schema
addition**, not a TUI rewrite — and **every** client benefits, not just the TUI. That is why this
comes last.

---

## Phase 4 exit criteria

- [ ] `GET /tools` exists and the sidebar reflects **discovered** plugins
- [ ] hardcoded tool list and the dead `enabled` toggle are **gone**
- [ ] `/compact` and `/export` do something real
- [ ] `POST /session/{id}/rename` works and auto-title does not clobber it
- [ ] pure SSE reducer exists with unit tests over recorded frame sequences — **first real tests
      for `app.rs` logic**
- [ ] `ThinkingDelta` / `ModelChanged` / `Compacted` are handled, not ignored
- [ ] zero `pending_*` fields remain
- [ ] `tui::sse_reconnect` passes; the "reconnecting..." message is no longer a lie
- [ ] `/diff` uses the session `cwd`, not `env::current_dir()`
- [ ] TRACKING Stage 07 table updated honestly against `spec/07-tui.md` (it was realigned in
      Phase 0 — keep it accurate)
