# 04 — `kn9t-store`

**Crate:** `kn9t-store`
**Depends on:** `kn9t-core` (GI-1, the one workspace dep) plus external crates `rusqlite`
(bundled SQLite) and `sha2`. GI-1 counts **workspace** dependencies; external crates are
governed by the §15 dependency budget, which lists both.
**DESIGN:** §6, §6.1, §6.2, §7, §7.1–7.5, §12.3, §18.4.
**Build order:** stage 4 of 10. Gate **G2**: kill -9 between turns, reload, state is exact;
`reproject --check` is clean (§16).

> **Note on the G1 dependency.** Stage 3's gate needed a `dyn Store`. Until this crate
> exists, stage 3 uses an in-memory test double implementing `Store`. This crate is the
> real durable implementation and MUST pass the identical loop tests plus the durability
> gates below.

---

## 1. Database and pragmas

> **R-STOR-010 → DESIGN §6**
> The store MUST open a single SQLite database at `~/.kn9t/kn9t.db` in **WAL** mode, with
> `synchronous = NORMAL`, `foreign_keys = ON`, and `busy_timeout ≥ 5000 ms` so a reader
> (`kn9t cost`) never fails while an agent writes. One DB, never per-session files (§6: cost
> queries span sessions).
> **Accept:** `cargo test stor::pragmas` — asserts `journal_mode=wal`, `foreign_keys=1`.

> **R-STOR-020 → DESIGN §6, §7.1**
> The store MUST allow many concurrent readers and exactly one writer. Subagent threads
> write to different `session_id`s concurrently; WAL handles this. No global in-process
> write lock beyond what the connection pool requires.

## 2. Schema (DDL)

> **R-STOR-030 → DESIGN §6, §12.3, §18.4, GI-4**
> The schema MUST be created exactly as below. `events` is append-only and canonical
> (GI-4); `messages`/`usage` are projections (§5); `live_messages` is non-canonical (§12.3);
> `blobs` is content-addressed with a refcount (README §6 decision).
> ```sql
> CREATE TABLE sessions (
>   id                   TEXT PRIMARY KEY,               -- ULID
>   created_at           INTEGER NOT NULL,
>   name                 TEXT,
>   cwd                  TEXT NOT NULL,
>   origin_session       TEXT REFERENCES sessions(id),   -- NULL if root
>   origin_seq           INTEGER,                        -- fork point in origin
>   fork_reason          TEXT,                           -- fork|rewind|subagent|tree
>   inherited_cost_usd   REAL    NOT NULL DEFAULT 0,
>   inherited_tokens_in  INTEGER NOT NULL DEFAULT 0,
>   inherited_tokens_out INTEGER NOT NULL DEFAULT 0,
>   inherited_ctx_tokens INTEGER NOT NULL DEFAULT 0,
>   budget_remaining_usd REAL,
>   model_at_fork        TEXT,
>   head_seq             INTEGER NOT NULL DEFAULT 0
> );
>
> CREATE TABLE events (
>   session_id TEXT    NOT NULL REFERENCES sessions(id),
>   seq        INTEGER NOT NULL,
>   ts         INTEGER NOT NULL,
>   kind       TEXT    NOT NULL,
>   payload    TEXT    NOT NULL,        -- JSON of the Event variant
>   PRIMARY KEY (session_id, seq)
> );
>
> CREATE TABLE messages (
>   session_id TEXT    NOT NULL REFERENCES sessions(id),
>   seq        INTEGER NOT NULL,
>   role       TEXT    NOT NULL,
>   content    TEXT    NOT NULL,        -- JSON Vec<Content>
>   est_tokens INTEGER NOT NULL,
>   PRIMARY KEY (session_id, seq)
> );
>
> CREATE TABLE usage (
>   session_id                 TEXT    NOT NULL REFERENCES sessions(id),
>   seq                        INTEGER NOT NULL,
>   ts                         INTEGER NOT NULL,
>   provider                   TEXT    NOT NULL,
>   model                      TEXT    NOT NULL,
>   kind                       TEXT    NOT NULL,   -- main|compaction|subagent|title
>   tokens_in                  INTEGER NOT NULL,
>   tokens_out                 INTEGER NOT NULL,
>   cache_read                 INTEGER NOT NULL,
>   cache_write                INTEGER NOT NULL,
>   reasoning                  INTEGER NOT NULL DEFAULT 0,
>   price_in_snapshot          REAL    NOT NULL,
>   price_out_snapshot         REAL    NOT NULL,
>   price_cache_read_snapshot  REAL    NOT NULL,
>   price_cache_write_snapshot REAL    NOT NULL,
>   cost_usd                   REAL    NOT NULL,   -- resolved at WRITE time
>   estimated                  INTEGER NOT NULL DEFAULT 0,
>   PRIMARY KEY (session_id, seq)
> );
>
> CREATE TABLE blobs (
>   hash       TEXT PRIMARY KEY,        -- sha256 hex
>   mime       TEXT    NOT NULL,
>   bytes_len  INTEGER NOT NULL,
>   bytes      BLOB    NOT NULL,
>   refcount   INTEGER NOT NULL DEFAULT 0,
>   created_at INTEGER NOT NULL
> );
>
> CREATE TABLE meta (
>   key   TEXT PRIMARY KEY,
>   value TEXT NOT NULL                 -- includes PROJECTION_VERSION
> );
>
> CREATE TABLE live_messages (          -- NON-canonical (§12.3)
>   session_id      TEXT PRIMARY KEY REFERENCES sessions(id),
>   msg_id          TEXT    NOT NULL,
>   role            TEXT    NOT NULL,
>   partial_content TEXT    NOT NULL,   -- JSON, NOT canonical
>   updated_at      INTEGER NOT NULL
> );
>
> CREATE INDEX usage_by_model    ON usage(model, kind);
> CREATE INDEX events_by_session ON events(session_id, seq);
> ```
> **Accept:** `cargo test stor::schema_matches` — creates a fresh DB and diffs
> `sqlite_master` against this DDL (normalized).

## 3. Append — the durable write

> **R-STOR-040 → DESIGN §3.1, §6, GI-4**
> `Store::append` MUST, in a **single** transaction: (1) read `sessions.head_seq`, (2)
> assign `seq = head_seq + 1`, (3) insert the row into `events`, (4) project the event into
> `messages`/`usage` as applicable (R-STOR-060), (5) `UPDATE sessions SET head_seq = seq`,
> (6) COMMIT, then return `seq`. Only after commit may the caller publish the event to the
> bus (§3.1). `seq` MUST come from `head_seq`, never an in-memory counter (§3.1 rationale).
> **Accept:** `cargo test stor::append_assigns_seq` — concurrent appends to two sessions
> produce gapless per-session sequences with no cross-contamination.

> **R-STOR-050 → DESIGN §3.1, GI-4**
> `append` MUST reject a non-durable event (`event.seq()` is `None`) with `StoreErr`
> — transient events never touch disk. `append` MUST NOT issue `UPDATE`/`DELETE` on
> `events`.
> **Accept:** `cargo test stor::append_rejects_transient`.

## 4. Projections

> **R-STOR-060 → DESIGN §6, §6.1**
> A single pure function `project(event) -> Vec<Row>` MUST be the **only** producer of
> `messages`/`usage` rows, used identically by the live writer (R-STOR-040) and by
> `reproject` (R-STOR-080). Mapping:
> - `MessageAppended` → one `messages` row (`est_tokens` via §7.4 estimate).
> - `UsageRecorded` → one `usage` row, with `cost_usd` and all four `price_*_snapshot`
>   filled (R-STOR-070) and `estimated` copied through.
> - `Compacted` → replaces the projected messages in `replaced` range with the summary
>   (projection reflects the compacted transcript).
> - `SessionForked`, `ModelChanged` → no projection row (they affect `sessions` columns,
>   set at session creation / on change).
> **Accept:** `cargo test stor::project_is_total` — every durable variant has a defined
> projection; feeding an event and reading the projection matches a golden row.

> **R-STOR-070 → DESIGN §6.1, §8.4.3**
> `usage.cost_usd` MUST be computed **at write time** as the tiered sum (§8.4.3), never at
> query time:
> ```
> cost = tokens_in   * price_in / 1e6
>      + cache_read  * price_cache_read  / 1e6
>      + cache_write * price_cache_write / 1e6
>      + tokens_out  * price_out / 1e6
> ```
> The four `price_*_snapshot` columns MUST record the prices used. Query-time cost from a
> current price table is forbidden (last month's numbers must not mutate).
> **Accept:** `cargo test stor::cost_tiered` — asserts a cached-heavy row costs far less
> than `(tokens_in+cache_read+cache_write) * price_in`, i.e. the 10x-overcharge bug is
> absent.

## 5. Reproject

> **R-STOR-080 → DESIGN §6.2**
> The store MUST implement `reproject`: DROP `messages`+`usage`, CREATE with the current
> schema, replay every event `ORDER BY session_id, seq` through `project` (R-STOR-060),
> write `PROJECTION_VERSION` to `meta`, all in one transaction. Startup MUST compare stored
> vs compiled `PROJECTION_VERSION` and auto-reproject on mismatch. `events` never migrates;
> an unknown `kind` on read is skipped with a warning.
> **Accept:** `cargo test stor::reproject_rebuilds` — corrupt a projection row, reproject,
> assert it is restored from `events`.

> **R-STOR-090 → DESIGN §6.2**
> `reproject --check` MUST project into temp tables and diff against live projections,
> reporting any difference as a writer/projector disagreement (a bug). It MUST NOT mutate
> live tables.
> **Accept:** `cargo test stor::reproject_check_clean` — after normal operation, `--check`
> reports zero diffs. **This is half of gate G2.**

## 6. `plan_request` and compaction

> **R-STOR-100 → DESIGN §7.5, §7.4, R-CORE-250**
> `plan_request` MUST: fold the session's durable events into the message list; compute the
> context estimate as `last_reported_input + Σ(len/4 for messages appended since)` (§7.4, no
> tokenizer); compute cache breakpoints via `kn9t_core::breakpoints` (it already holds
> `ModelSpec`); and decide compaction.
> **Accept:** `cargo test stor::plan_no_tokenizer` — no `tiktoken`/`tokenizers` dep in
> `cargo tree`.

> **R-STOR-110 → DESIGN §7.5**
> When the estimate ≥ threshold × `model.ctx_window` (threshold default **0.80**,
> **SPEC-OPEN** §7.5), `plan_request` MUST return `compact: Some(CompactSpan)` selecting the
> **oldest** span, with the boundary snapped so **no `ToolCall` is separated from its
> `ToolResult`** (hard invariant, §7.5). Otherwise `compact: None`. The store never calls a
> provider and never emits `UsageRecorded` (§3, §7.5); it only proposes the span.
> **Accept:** `cargo test stor::compact_boundary` — a span whose naive cut would orphan a
> tool call is snapped to include the pair.

> **R-STOR-115 → DESIGN §7.5, §9.1**
> Before computing breakpoints or the compact span, `plan_request` MUST close every
> `ToolCall` in the folded message list that has no matching `ToolResult`, by inserting a
> synthesized `ToolResult { is_error: true }` carrying the provider's verbatim `CallId`
> immediately after the message that opened the call. §9.1 has the loop synthesize these on
> abort, but that only covers aborts the loop survives: a killed process (`kill -9`, server
> restart, panic) leaves the assistant `MessageAppended` durable with no tool-role message
> after it, and every provider 400s on the orphan — permanently, since the log is
> append-only (GI-4) and the missing result can never be back-filled. The repair MUST be in
> the fold, not the log: `events` keeps the honest record that the call never answered.
> `seqs` MUST stay in step with the message list so `compact_span` still reports real
> `SeqRange`s. An already-answered call MUST NOT gain a second result.
> **Accept:** `cargo test stor_orphan_from_interrupted_tool_execution` — a transcript whose
> tool result was never persisted plans with no orphan, the synthesized result carries the
> original call id, and completed calls keep exactly one result.

> **R-STOR-117 → DESIGN §7.5, R-PCORE-050, R-CORE-062**
> `plan_request` MUST replace any `Content::ToolCall::args_json` that is not parseable JSON
> with `{}` before the message list is returned. R-PCORE-050 rejects an incomplete args
> concat at assemble time, so no new message can carry one; but a message persisted before
> that guard has the broken bytes durable in `events`, and append-only (GI-4) means they can
> never be rewritten — every later `plan_request` replays them and the provider rejects the
> whole request, bricking the session exactly as in R-STOR-115. The repair MUST therefore be
> in the fold, not the log. The call MUST be kept, not dropped: removing it would orphan its
> `ToolResult` (§7.5) and lose a turn the transcript already accounts for. A parseable
> `args_json` MUST be left byte-identical, preserving key order (R-CORE-062).
> **Accept:** `cargo test stor_plan_repairs_unparseable_tool_args` — a transcript holding one
> truncated and one valid `args_json` plans with every tool call parseable, the broken one as
> `{}`, the valid one byte-identical, and the pairing intact.

## 7. Sessions and forking

> **R-STOR-120 → DESIGN §7, §7.1**
> Each session is a linear log; every divergence (`fork`, `tree`, `rewind`, subagent) is a
> **new `sessions` row**. There are no lanes, no `parent_seq` on events. State
> reconstruction is `fold(events WHERE session_id=? ORDER BY seq)`.

> **R-STOR-130 → DESIGN §7.2, §7.3**
> Fork MUST copy `MessageAppended`, `ModelChanged`, and `Compacted` events into the new
> session with `seq` **renumbered contiguously** (and `Compacted.replaced` remapped to the
> new seqs). It MUST NOT copy `UsageRecorded` (§7.2: copying double-counts spend). The new
> session's seq 0 MUST be a `SessionForked` event carrying the `ForkSnapshot` (R-CORE-160),
> whose inherited totals are the origin's cumulative figures **up to `origin_seq`**, captured
> at copy time.
> **Accept:** `cargo test stor::fork_no_usage` — forked session has zero own `usage` rows
> and correct `inherited_*` columns; `cargo test stor::fork_renumber` — seqs are contiguous
> from 0 and `Compacted.replaced` is remapped.

## 8. Blobs and refcount GC

> **R-STOR-140 → DESIGN §12.7, §18.4 (decision: refcount, README §6)**
> The store MUST expose blob put/get:
> ```rust
> pub fn put_blob(&self, bytes: &[u8], mime: &str) -> Result<String, StoreErr>; // returns sha256 hex
> pub fn get_blob(&self, hash: &str) -> Result<Option<(Vec<u8>, String)>, StoreErr>;
> ```
> `put_blob` MUST be content-addressed: identical bytes stored twice keep one row. Hash is
> SHA-256 hex.
> **Accept:** `cargo test stor::blob_dedup` — same bytes twice → one row.

> **R-STOR-150 → DESIGN §18.4 (decision: refcount)**
> `blobs.refcount` MUST be incremented when a `MessageAppended` (or `live_messages`) content
> references a `sha256:` hash, and decremented when a session is deleted (R-STOR-160). A row
> reaching `refcount == 0` MUST be deleted. Reference counting happens inside the same
> transaction as the referencing write.
> **Accept:** `cargo test stor::blob_refcount` — a blob referenced by two sessions survives
> deletion of one and is removed on deletion of the second.

> **R-STOR-160 → DESIGN §12.1, §18.4**
> Session delete MUST, in one transaction: decrement `refcount` for every blob its messages
> reference (deleting rows that hit zero), then delete the session's `events`, `messages`,
> `usage`, `live_messages`, and the `sessions` row. Deleting a session that is an
> `origin_session` of a live fork MUST be rejected (the fork's inherited snapshot is a copy,
> but `origin_session` FK integrity is preserved) — **SPEC-OPEN**: alternatively null the FK;
> interim behavior is **reject with StoreErr**.
> **Accept:** `cargo test stor::session_delete_blobs`.

## 9. `live_messages` (mid-stream, non-canonical)

> **R-STOR-170 → DESIGN §12.3, §5.2, GI-4**
> The store MUST offer upsert/read/delete for `live_messages`, used by the server to expose
> in-flight assistant text to a late-attaching client. It is **not** canonical: `reproject`
> MUST ignore it, and it MUST be **truncated on startup**. A crash loses the partial (§5.2);
> this table is a display cache, not a resume mechanism.
> **Accept:** `cargo test stor::live_truncated_on_open` — a row present before reopen is
> gone after; `reproject` output is unaffected by its contents.

## 10. Cost analytics query

> **R-STOR-180 → DESIGN §6, §7.3**
> The store MUST support the analytics queries `GET /cost` (SRV) is built on: `sum(cost_usd)`
> grouped by model/kind/session over `usage`, and the three §7.3 figures (marginal =
> own `sum(cost_usd)`; effective = own + `inherited_cost_usd`; family = recursive rollup
> over `origin_session`).
> **Accept:** `cargo test stor::cost_rollup` — a 3-level fork family reports correct
> marginal, effective, and family totals.

## 11. Stage gate G2

> **R-STOR-900 → DESIGN §16 gate G2**
> Stage 4 is **done** when: the schema matches R-STOR-030; the ReAct loop (03) runs against
> the **real** store; **kill -9 between turns then reload reconstructs state exactly** (fold
> of `events` equals pre-crash in-memory state); **`reproject --check` reports zero diffs**;
> fork copies context but not usage; blob refcount GC reclaims correctly; `live_messages` is
> truncated on startup; and no tokenizer crate is linked. GI-1/GI-4 hold.
