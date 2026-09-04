# kn9t — Implementation Specification

This directory is the implementation-leading specification for kn9t. It is derived from,
and subordinate to, `../DESIGN.md`. Where this spec and the design disagree, the design's
*decisions* win and this spec is the bug. Where the design is silent, this spec is
authoritative and records the choice with a decision note or a `SPEC-OPEN` marker.

Read `../DESIGN.md` first for the *why*. This spec is the *what* and *how* — precise
enough that an engineer or an implementing agent can build each crate without
re-deriving a decision.

---

## 1. How to read this spec

Files are numbered by the build order in DESIGN §16. Build strictly in order: each
stage's acceptance gate depends on the previous stage existing.

| file | crate(s) | DESIGN | scope |
|---|---|---|---|
| `01-core.md` | `kn9t-core` | §4, §5, §3, §7.5, §8, §8.4 | v1 |
| `02-replay.md` | `kn9t-provider-replay` | §8.5, §16 | v1 |
| `03-react-tools.md` | `kn9t-react`, `kn9t-tools` (integration harness) | §9, §9.1, §10, §10.1, §11, §11.1, §11.2 | v1 |
| `04-store.md` | `kn9t-store` | §6, §6.1, §6.2, §7, §7.2–7.5, §12.3 | v1 |
| `05-provider-core-openai.md` | `kn9t-provider-core`, `kn9t-provider-openai` | §2.1, §8.1–8.4, §8.7 | v1 |
| `06-server.md` | `kn9t-server` | §12 | v1 |
| `07-tui.md` | `kn9t-tui` | §12.8 | v1 |
| `08-plugin.md` | `kn9t-plugin` | §13, §18.2 | v1 |
| `08b-plugin-redesign.md` | `kn9t-plugin` (updated), `kn9t-plugin-sdk`, `plugins/kn9t-tools` (external, auto-discovered) | §13.7–13.9 | v1 |
| `09-anthropic.md` | `plugins/kn9t-anthropic` (external, standalone) | §8.5, §8.4.4 | v1 |
| `10-bedrock-native-v2.md` | `kn9t-provider-bedrock`, `kn9t-provider-gemini` | §8.5, §8.4.4 | v2 |

---

## 2. Requirement ID scheme

Every normative statement carries a stable ID. IDs never change once assigned; a
withdrawn requirement is marked `WITHDRAWN` in place rather than deleted or renumbered,
so external references never dangle.

```
R-<AREA>-<NNN>
```

| AREA | file | subject |
|---|---|---|
| `CORE` | 01 | types, events, bus, traits, breakpoints |
| `RPLY` | 02 | replay provider, fixtures |
| `RCT` | 03 | ReAct loop, cancellation, hooks integration |
| `TOOL` | 03 | tool trait, read/write/edit/bash, risk seam (`HookVeto`) |
| `STOR` | 04 | schema, projections, reproject, sessions, blobs |
| `PCORE` | 05 | provider-core: http, sse, assemble, retry, cache encode |
| `OAI` | 05 | openai/LiteLLM provider |
| `NBED` | 05 | LiteLLM gateway (openai-compat) |
| `SRV` | 06 | server, http surface, sse, leases, lifecycle |
| `TUI` | 07 | terminal client |
| `PLUG` | 08 | plugin host, hooks, subagent spawn |
| `PLUG2` | 08b | protocol v2 (chunk/done/cancel), SDK, internal tools plugin |
| `CP` | 09 | kn9t-custom-provider external custom plugin |
| `ANTH` | 09 | anthropic provider |
| `BEDN` | 10 | native bedrock provider (v2) |
| `GEM` | 10 | gemini provider (v2) |

`NNN` is zero-padded and assigned in reading order within a file, in tens (010, 020,
030…) so an inserted requirement gets an intermediate number (015) without a renumber.

Each requirement links its justifying design section, e.g. `R-CORE-062 → DESIGN §4.1`.
The final-pass check (task 12) asserts every ID is unique and every cited design section
exists.

---

## 3. Requirement keywords

Interpreted per RFC 2119/8174.

- **MUST** / **MUST NOT** — absolute. A violation is a defect that blocks the stage gate.
- **SHALL** — synonym for MUST, used for behavioral sequences.
- **SHOULD** — strong default; deviation requires a recorded reason in code review.
- **MAY** — genuinely optional.

A requirement stated as a Rust signature, DDL, or wire schema is **MUST** by default: the
implementation matches it exactly unless prose says otherwise.

---

## 4. What a requirement looks like

Each numbered requirement has, at minimum, a normative statement and a design link. Where
it defines an interface it carries the exact signature/DDL/schema. Where it is testable it
names an acceptance check the stage gate runs.

> **R-CORE-000 → DESIGN §4** *(illustrative format only)*
> The crate MUST expose type `X` with exactly this signature:
> ```rust
> pub struct X { pub a: u32 }
> ```
> **Accept:** `cargo test core::x_layout` — a round-trip serde test asserts the field
> set and JSON shape.

Acceptance checks are concrete `cargo test` / CLI invocations, not aspirations. A stage is
**done** only when every MUST in its file has a passing acceptance check.

---

## 5. Global invariants

These hold across every file and are not restated per requirement. Violating one is a
defect regardless of which crate introduced it.

- **GI-1 → DESIGN §2.** No crate except `kn9t-server` has more than one workspace
  dependency. CI asserts this by parsing each `Cargo.toml`. Scope: **`[dependencies]`
  only** — what a crate links at build time. Test-only siblings in
  `[dev-dependencies]` are exempt, since they cannot create a runtime coupling or a
  cycle in the shipped artifact; `scripts/check-gi1.sh` lists the ones it skips so the
  exemption stays auditable (96E-32).
- **GI-2 → DESIGN §1, §4.** `kn9t-core` depends only on `serde`/`serde_json`. Every event
  payload is `Serialize + Deserialize` with no `Arc`, file handle, `&dyn`, or closure.
- **GI-3 → DESIGN §8.4.2.1.** No `HashMap` is ever serialized into a request or a cached
  prefix. Tool registries and JSON Schemas use `Vec` or `BTreeMap`; `serde_json`'s
  `preserve_order` feature is off. CI greps for `preserve_order` and fails if present.
- **GI-4 → DESIGN §6.** `events` is append-only. No code path issues `UPDATE` or `DELETE`
  against it. The only mutable-in-place table is `live_messages`.
- **GI-5 → DESIGN §1.** No `tokio`, `async`, or `.await` anywhere. All I/O is blocking on
  OS threads. CI greps for `async fn`/`.await` and fails if present.
- **GI-6 → DESIGN §12.8, §2.** `kn9t-tui` does not depend on `kn9t-core`. It speaks HTTP +
  SSE only.

---

## 6. Interface decisions made by the spec

The design left these open (DESIGN §18); the spec closes them so implementation is not
blocked. Each is expanded in its stage file.

| topic | DESIGN | decision | where specified |
|---|---|---|---|
| shell target | §18.6 | cross-platform: `bash` runs the host shell; risk judgement is the policy plugin's, not kn9t's (ADR-0008 superseded the in-tree POSIX/PowerShell classifiers) | 03, §TOOL bash |
| subagent spawn | §18.2 | spawn/return mechanism + budget-cap fully specified; child tool subset is a **config list**, no hardcoded default | 08, §PLUG spawn |
| blob GC | §18.4 | **refcount** column on `blobs`, decremented on session delete, row dropped at zero | 04, §STOR blobs |
| session titling | §18.3 | **auto-title** after first assistant turn, best-effort, silent on failure, `UsageKind::Title` | 06, §SRV titling |

---

## 7. Tunable constants (SPEC-OPEN)

These are numbers the design left unset (DESIGN §18) that do **not** change any interface,
schema, or code path — only a value. Each ships with the default below and is marked
`SPEC-OPEN` at its use site, meaning: change freely, no rewrite required. They are
collected here so they are not scattered.

| constant | default | DESIGN | file |
|---|---|---|---|
| cache TTL | 5 min (request-start) | §8.4.2.2 | 05 |
| server idle-exit | 30 min, ≥ cache TTL | §18.14 | 06 |
| truncation give-up count | 4 attempts | §18.9 | 03 |
| truncation reminder ladder | 150/100/50/25/10 lines | §18.9 | 03 |
| compaction threshold | 0.80 × ctx_window | §7.5 | 04 |
| lease idle timeout | 5 min | §12.6 | 06 |
| LDAP check TTL | 12 h (`check_ttl_secs`) | §8.7.2 | 05 |
| connect timeout | 20 s (`connect_timeout_ms`) | §8.6 | 05 |
| plugin failure-unsubscribe | 3 consecutive `on_event` failures | §13.3 | 08 |

---

## 8. Deferred to v2

Fully specified in `10-bedrock-native-v2.md` but out of the v1 acceptance gates:

- native Bedrock provider (SigV4 + `vnd.amazon.eventstream` binary framing) — DESIGN §8.5
- Gemini provider (separate cached-content resource) — DESIGN §8.4.4
- Anthropic 1-hour cache TTL opt-in — DESIGN §18.12
- Anthropic top-level automatic `cache_control` — DESIGN §18.13

Reachable interim: the LiteLLM gateway (`05`) handles Bedrock models server-side, so
native Bedrock is a performance/independence upgrade, not a capability gate.

---

## 9. Open items still open after this spec

Items from DESIGN §18 that remain genuinely undecided and are **not** closed here, each
tracked as `SPEC-OPEN` at the relevant site with the stated interim behavior:

- **compaction prompt text** (§18.1) — **CLOSED 2026-09-02 (96E-17)** — the fixed template was deleted; compaction is delegated to a `compactor`-capability plugin (see §08b `compactor_compact`), fail-closed when no plugin is installed.
- **custom provider model catalog disk cache** (§18.7) — interim: fetch per process, no cache.
- **budget drift warning** (§18.8) — interim: `GET /budget` reports both figures, no warn.
- **cache-hit reporting in `kn9t cost`** (§18.11) — interim: not surfaced.
- **compaction-boundary snapping to preserve ② breakpoint** (§18.10) — interim: no snap.

These are measurement-dependent per the design and must not be guessed.
