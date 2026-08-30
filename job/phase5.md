# PHASE 5 — revise DESIGN, record outcomes

**Goal:** bring `DESIGN.md` in line with what was learned, on the strength of its own recorded
trigger conditions — not on preference.
**Depends on:** Phases 1–4 (revise once the outcomes are known).
**Read first:** `job/decisions.md` → "API contract"; DESIGN §11, §15.

---

## Why DESIGN may be revised

The user's standing position: *"design was the FIRST document of the code base. things have
evolved and there is no user yet, so we can revise."*

That is legitimate, but the **strongest** argument is DESIGN's own text — revise on a fired
trigger, not on taste:

- **DESIGN §15** on the hand-written-schema decision: *"Rejected `schemars` (~10 crates) on
  dep-budget grounds; **revisit if this bites**."*
- **DESIGN §11**: *"Schemas are hand-written `json!({...})` literals — zero deps. **Accepted
  cost:** renaming a param field silently breaks the schema until an integration test catches
  it."*

**There is no such integration test, and it has bitten twice** — F5 (`created_at` ms-vs-seconds,
year 57668) and F7 (`CreateSessionReq.model` wrong type, silently ignored). Plus F11
(`KvClient` — a `pub` field with a non-`pub` constructor broke every external Rust plugin's
tests).

That is a decision with a documented trigger condition that has now fired. Revise §11 and §15 on
that basis.

**Keep `schemars` rejected** — but fix the *reason*. It generates schema **from** Rust types,
the wrong direction, and could never serve the existing **Go** and **Python** plugins. Phase 2's
schema-first approach needs **zero new runtime deps**, so the §15 budget survives with a better
justification than before.

---

## Tasks

1. **Rewrite DESIGN §11's "accepted cost" paragraph** — the cost was not merely accepted, it was
   *realized*. Point at the schema-first approach (ADR-0005) and the CI drift gate.
2. **Update DESIGN §15's rejection list** — restate why `schemars` is still out (direction +
   polyglot plugins), and note the `xtask` generator is a dev-time tool, not a runtime dep.
3. **Update DESIGN §10 / §10.1 if Phase 1 deviated** in any way from the specified
   `ConfigPolicy` / `InteractivePolicy` split or the `once|session|always` scope semantics.
4. **Document the `effects` mechanism in DESIGN §10.1** — ADR-0002 is the decision record, but
   DESIGN is the rationale document and §10.1 is where command classification is explained. The
   classifier now runs server-side against a *declared field* rather than against a known `bash`
   tool.
5. **Update DESIGN §16 build order** — `S8b` node says `internal-plugins/kn9t-tools`; after
   Phase 3 there is no such thing.
6. **Reconcile `spec/07-tui.md` with reality** — many R-TUI requirements have no test and some
   describe behaviour that was never built (R-TUI-220, R-TUI-230). Decide per requirement: build
   it, or amend the spec. Per AGENTS.md §9, an unimplementable MUST is a **spec bug to record**,
   not to silently ignore.
7. **Final TRACKING pass** — every `☑` must have a genuinely passing named test. Re-assess **G1**
   (currently `✗`) and **G3** (manual, deferred: 3 TUIs / 1 server / 1 lease / screenshot paste).
8. **Fix the test count** — `TRACKING.md` currently says `385 passed`, which was **my measurement
   error**. Measure honestly and resolve F13's build-order failures first.

---

## Consider adding

- An ADR on **`Policy` as the safety seam** — that all risk decisions funnel through
  `Policy::check()` and must never be duplicated in a tool or plugin. Worth recording so nobody
  re-adds a second gate elsewhere.
- An ADR on **the CRLF situation** if it keeps recurring — `.gitattributes` currently only pins
  the replay fixtures (`-text`). Four files have 500-line phantom diffs from a Windows editor. A
  `* text=auto` rule would stop this class of noise permanently.

---

## Phase 5 exit criteria

- [ ] DESIGN §11 and §15 reflect the fired trigger and the schema-first decision
- [ ] DESIGN §10.1 documents server-side classification via declared `effects`
- [ ] DESIGN §16 has no `internal-plugins` reference
- [ ] every `☑` in TRACKING maps to a real passing test
- [ ] G1 honestly re-assessed and green (or explicitly red with a reason)
- [ ] test count in TRACKING is measured, not guessed
- [ ] all spec bugs found across phases 1–4 are in CHANGELOG's "Discovered bugs" table
