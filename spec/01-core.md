# 01 — `kn9t-core`

**Crate:** `kn9t-core`
**Depends on:** `serde`, `serde_json` — and nothing else (GI-2).
**DESIGN:** §1, §3, §3.1, §4, §4.1, §4.2, §5, §5.1, §7.5, §8, §8.4.
**Build order:** stage 1 of 10. Nothing else compiles until this is done.

This crate is the entire vocabulary of the system: every type on the wire, in the log,
and across every trait boundary. It contains **no I/O, no threads spawned, no provider,
no store** — only data types, trait definitions, the bus, and pure functions.

---

## 1. Crate-level requirements

> **R-CORE-010 → DESIGN §1, §4, GI-2**
> `kn9t-core`'s `Cargo.toml` MUST declare exactly these dependencies: `serde` (with
> `derive`), `serde_json`. No other `[dependencies]` entry is permitted. `serde_json`
> MUST NOT enable the `preserve_order` feature (GI-3).
> **Accept:** CI parses `Cargo.toml`; the dependency set equals `{serde, serde_json}` and
> no line matches `preserve_order`.

> **R-CORE-020 → DESIGN §1, GI-5**
> The crate MUST contain no `async fn`, no `.await`, and no dependency that pulls a `tokio`
> or `async-std` runtime.
> **Accept:** `rg 'async fn|\.await' crates/kn9t-core/src` returns nothing; `cargo tree`
> shows no `tokio`.

> **R-CORE-030 → DESIGN §4, Principle 4**
> Every type in this file that appears in an `Event` payload MUST derive
> `Serialize, Deserialize`, and MUST NOT contain any of: `Arc`, `Rc`, a raw pointer, a
> file/socket handle, a `&dyn`, or a closure.
> **Accept:** `cargo test core::payload_is_pod` — a compile-time test constructs each
> payload type behind `fn assert_serde<T: Serialize + DeserializeOwned>()`.

---

## 2. Identifiers

> **R-CORE-040 → DESIGN §4**
> The crate MUST define these newtypes. `SessionId` and `MsgId` wrap a ULID string;
> `CallId` wraps the provider's tool-call id **verbatim** (never regenerated);
> `ApprovalId` is a process-local monotonic `u64`.
> ```rust
> #[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] pub struct SessionId(pub String);
> #[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] pub struct MsgId(pub String);
> #[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] pub struct CallId(pub String);
> #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)] pub struct ApprovalId(pub u64);
> ```
> **Accept:** `cargo test core::id_serde` — each serializes to a bare JSON string / number,
> not a wrapper object.

> **R-CORE-045 → DESIGN §4**
> `SessionId` and `MsgId` MUST provide a `new() -> Self` that generates a fresh ULID.
> ULID (not UUID) because lexical order equals creation order, which the store relies on.
> **Accept:** `cargo test core::ulid_monotonic` — two `new()` calls 1ms apart produce
> lexically increasing strings.

---

## 3. Messages and content

> **R-CORE-050 → DESIGN §4**
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "lowercase")]
> pub enum Role { System, User, Assistant, Tool }
>
> #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
> pub struct Message {
>     pub id:      MsgId,
>     pub role:    Role,
>     pub content: Vec<Content>,
> }
> ```

> **R-CORE-060 → DESIGN §4, §4.1, §4.2**
> `Content` MUST be a single flat enum covering every provider's block types, tagged for
> stable JSON:
> ```rust
> #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(tag = "type", rename_all = "snake_case")]
> pub enum Content {
>     Text       { text: String },
>     /// Never inline bytes: a "sha256:<hex>" ref into `blobs` (STOR / §12.4).
>     Image      { sha256: String, mime: String },
>     /// `args_json` holds the provider's exact bytes; see R-CORE-062.
>     ToolCall   { id: CallId, name: String, args_json: String },
>     ToolResult { id: CallId, content: Vec<Content>, is_error: bool },
>     /// `signature` is opaque and provider-owned; see R-CORE-064.
>     Thinking   { text: String, signature: Option<String> },
> }
> ```
> **Accept:** `cargo test core::content_tag` — round-trips one value of each variant and
> asserts the `"type"` discriminant string.

> **R-CORE-062 → DESIGN §4.1**
> `Content::ToolCall::args_json` MUST store the raw concatenation of `Chunk::ToolArgs`
> fragments as received. No code path in any crate may parse it to a `serde_json::Value`
> and re-serialize it back into `args_json`; the stored bytes are emitted to the provider
> unchanged on replay. `Tool::execute` parses to a throwaway value only.
> *Rationale: re-serialization reorders keys and misses the message-level cache every
> tool-loop turn (§4.1).*
> **Accept:** `cargo test core::args_verbatim` — feed `{"b":1,"a":2}` (non-sorted keys)
> through append→plan→encode and assert the outgoing bytes are byte-identical.

> **R-CORE-064 → DESIGN §4.2**
> `Content::Thinking` MUST be durable (persisted), carrying `signature` unchanged. A
> per-provider `ThinkingReplay` quirk (R-CORE-095) decides whether it reaches the wire;
> the stored form is always verbatim.
> **Accept:** covered by provider tests (09); here `cargo test core::thinking_roundtrip`
> asserts `signature` survives serde unchanged including `None`.

---

## 4. Models, pricing, thinking

> **R-CORE-070 → DESIGN §4**
> ```rust
> #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
> pub struct ModelRef { pub provider: String, pub id: String }
>
> #[derive(Clone, Serialize, Deserialize)]
> pub struct ModelSpec {
>     pub r#ref:      ModelRef,
>     pub api_id:     String,     // may differ from ref.id, e.g. the ":1m" pair (NBED)
>     pub ctx_window: u32,
>     pub max_out:    u32,
>     pub price:      Price,
>     pub cache:      CacheMode,  // carries min_tokens
>     pub streaming:  bool,       // false ⇒ synthesize chunks (NBED §8.7.4)
>     pub quirks:     Quirks,
> }
> ```

> **R-CORE-080 → DESIGN §4, §6.1, §8.4.3**
> `Price` is USD per 1,000,000 tokens and MUST carry all four tiers, so the write-time
> cost projection (STOR §6.1) can compute each tier separately:
> ```rust
> #[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
> pub struct Price { pub input: f64, pub output: f64, pub cache_read: f64, pub cache_write: f64 }
> ```

> **R-CORE-090 → DESIGN §4**
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "lowercase")]
> pub enum Effort { Low, Medium, High }
>
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "snake_case")]
> pub enum Thinking { Off, Effort(Effort), Budget(u32) }
> ```

> **R-CORE-095 → DESIGN §4.2, §8.2, §8.3**
> `Quirks` holds the wire divergences that are config data (§8.2), never URL-sniffed. The
> full field set is enumerated in PCORE/OAI (05); `kn9t-core` MUST define at least
> `thinking_replay`, the one quirk core behavior depends on. `Quirks` MUST be
> constructible with all-default values.
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "lowercase")]
> pub enum ThinkingReplay { Verbatim, Strip }
> ```
> `Quirks` field ordering, when serialized, MUST be deterministic (struct field order),
> never a `HashMap` (GI-3).

---

## 5. Usage and stop reasons

> **R-CORE-100 → DESIGN §4, §8.4.3**
> `Tokens` is a **partition, not an overlap**: `input` counts only tokens after the last
> cache breakpoint. Total context is `input + cache_read + cache_write` (§8.4.3).
> ```rust
> #[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
> pub struct Tokens {
>     pub input: u32, pub output: u32,
>     pub cache_read: u32, pub cache_write: u32,
>     pub reasoning: u32,
> }
>
> #[derive(Clone, Serialize, Deserialize)]
> pub struct Usage { pub tokens: Tokens, pub model: ModelRef }
> ```
> Providers that do not report a counter MUST leave it `0` (`Default`), which costs
> correctly by construction (§8.4.3).
> **Accept:** `cargo test core::tokens_default_zero`.

> **R-CORE-110 → DESIGN §4, §8.6.6**
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "snake_case")]
> pub enum StopReason { Stop, ToolUse, Length, Aborted, Refusal }
> ```

---

## 6. Tool specification

> **R-CORE-120 → DESIGN §4, §11, GI-3**
> ```rust
> #[derive(Clone, Serialize, Deserialize)]
> pub struct ToolSpec {
>     pub name:        String,
>     pub description: String,
>     pub schema:      serde_json::Value,   // hand-written json!({...}), ordered
> }
> ```
> The `schema` value MUST NOT be produced from a `HashMap`; object key order is stable
> across processes (GI-3). *A `serde_json::Value::Object` is `BTreeMap`-backed by default,
> which satisfies this as long as `preserve_order` is off.*

---

## 7. Errors

> **R-CORE-130 → DESIGN §4, §8.1, §8.6.6, §7.5**
> `ProvErr`'s variants are load-bearing: retry (PCORE §8.1), compaction trigger (§7.5),
> and truncation policy (RCT §8.6.6) all branch on them. The set MUST be exactly:
> ```rust
> #[derive(Debug, Clone, Serialize, Deserialize)]
> pub enum ProvErr {
>     Connect(String),                    // pre-stream; retried inside stream()
>     Http { status: u16, body: String }, // pre-stream; retried on 429/5xx
>     Stream(String),                     // mid-stream error frame; fatal to the turn
>     ContextOverflow,                    // prompt too long → triggers compaction
>     Truncated,                          // stream ended with unfinished tool calls
>     Decode(String),                     // unparseable wire bytes
> }
> ```
> `ProvErr` MUST implement `std::error::Error` and `Display`.

> **R-CORE-135 → DESIGN §4**
> ```rust
> #[derive(Debug, Clone, Serialize, Deserialize)] pub struct StoreErr(pub String);
> #[derive(Debug, Clone, Serialize, Deserialize)] pub struct ToolErr(pub String);
> ```
> Both MUST implement `std::error::Error` and `Display`.

---

## 8. Events — the wire, the log, the truth

> **R-CORE-140 → DESIGN §5, §5.1, Principle 4**
> The crate MUST define one `Event` enum. A variant is **durable** iff it carries a
> `seq: u64` field; **transient** otherwise. Durable variants folded in `seq` order
> reconstruct a session exactly (§5). The enum MUST be exactly:
> ```rust
> #[derive(Clone, Serialize, Deserialize)]
> #[serde(tag = "kind")]
> pub enum Event {
>     // ── durable ──
>     SessionForked   { seq: u64, fork: ForkSnapshot },
>     MessageAppended { seq: u64, msg: Message },
>     ModelChanged    { seq: u64, model: ModelRef },
>     Compacted       { seq: u64, replaced: SeqRange, summary: Message },
>     UsageRecorded   { seq: u64, provider: String, model: String,
>                       kind: UsageKind, tokens: Tokens,
>                       price_snapshot: Price, cost_usd: f64, estimated: bool },
>     // ── transient ──
>     TurnStarted     { turn: u32 },
>     TextDelta       { msg_id: MsgId, idx: u32, delta: String },
>     ThinkingDelta   { msg_id: MsgId, idx: u32, delta: String },
>     ToolArgsDelta   { msg_id: MsgId, idx: u32, delta: String },
>     ToolStarted     { call_id: CallId, name: String },
>     ToolProgress    { call_id: CallId, note: String },
>     ToolFinished    { call_id: CallId, is_error: bool },
>     ApprovalRequest { id: ApprovalId, tool: String, args: serde_json::Value, cwd: PathBuf },
>     TurnEnded       { turn: u32, stop: StopReason },
>     HookFailed      { plugin: String, hook: HookName, reason: String },
>     Error           { message: String },
> }
> ```
> `SeqRange` is a serde-friendly `{ start: u64, end: u64 }` (not `std::ops::Range`, which
> serializes awkwardly and is not `Copy`).
> **Accept:** `cargo test core::event_tag` — every variant round-trips with its `"kind"`
> discriminant.

> **R-CORE-142 → DESIGN §9.1**
> `UsageRecorded.estimated` is `true` when the figure was inferred after an abort cut the
> stream before usage arrived (§9.1), `false` when provider-reported. This field MUST
> exist so the cost projection can flag estimates.

> **R-CORE-145 → DESIGN §5**
> `Event` MUST provide `fn seq(&self) -> Option<u64>`, returning `Some` iff the variant is
> durable, and `fn is_durable(&self) -> bool`.
> **Accept:** `cargo test core::seq_partition` — asserts exactly the five listed variants
> return `Some`, all others `None`.

> **R-CORE-150 → DESIGN §5, §7.3**
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "lowercase")]
> pub enum UsageKind { Main, Compaction, Subagent, Title }
> ```

> **R-CORE-155 → DESIGN §13.3**
> `HookName` carries one variant per hook in the plugin surface (PLUG §13.3); `on_event`
> is a subscription, not a hook, and is absent.
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "snake_case")]
> pub enum HookName {
>     BeforeToolCall, AfterToolCall, BeforeRequest, ShouldStopAfterTurn,
>     PrepareNextTurn, GetSteering, GetFollowup, GetApiKey,
> }
> ```

---

## 9. Fork snapshot

> **R-CORE-160 → DESIGN §7.3, §7.2**
> `SessionForked` (seq 0 of every derived session) carries a snapshot captured **at copy
> time**, never recomputed. It MUST be:
> ```rust
> #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(rename_all = "lowercase")]
> pub enum ForkReason { Fork, Rewind, Subagent, Tree }
>
> #[derive(Clone, Serialize, Deserialize)]
> pub struct ForkSnapshot {
>     pub origin_session:       SessionId,
>     pub origin_seq:           u64,
>     pub reason:               ForkReason,
>     pub inherited_cost_usd:   f64,
>     pub inherited_tokens_in:  u64,
>     pub inherited_tokens_out: u64,
>     pub inherited_cache_read: u64,
>     pub inherited_messages:   u32,
>     pub inherited_ctx_tokens: u32,
>     pub budget_remaining_usd: Option<f64>,
>     pub model_at_fork:        ModelRef,
>     pub thinking_at_fork:     Thinking,
>     pub cwd_at_fork:          PathBuf,
> }
> ```
> **Accept:** `cargo test core::fork_snapshot_serde`.

---

## 10. Provider interface

> **R-CORE-170 → DESIGN §8, §8.4**
> `Request` MUST be defined **once**, carrying the cache breakpoints:
> ```rust
> pub struct Request<'a> {
>     pub model:      &'a ModelSpec,
>     pub system:     Option<&'a str>,
>     pub messages:   &'a [Message],
>     pub tools:      &'a [ToolSpec],
>     pub thinking:   Thinking,
>     pub max_tokens: Option<u32>,
>     /// Priority order, deduplicated, capped (R-CORE-200). NOT positional.
>     pub cache:      &'a [Cache],
> }
> ```
> `Request` is a borrowing view and is **not** `Serialize` (it is never persisted; only its
> constituent parts are). This is the one non-payload struct in the crate.

> **R-CORE-180 → DESIGN §8**
> ```rust
> #[derive(Clone, Serialize, Deserialize)]
> #[serde(tag = "chunk", rename_all = "snake_case")]
> pub enum Chunk {
>     Text     { idx: u32, delta: String },
>     Thinking { idx: u32, delta: String },
>     ToolCall { idx: u32, id: CallId, name: String },
>     ToolArgs { idx: u32, delta: String },   // raw JSON fragments
>     Usage(Usage),
>     Stop(StopReason),
> }
> ```
> `Chunk` is `Serialize/Deserialize` so the replay provider (02) can store decoded chunks
> for its own unit tests, even though fixtures themselves are raw bytes (R-RPLY).

> **R-CORE-190 → DESIGN §8, §8.1, §1**
> ```rust
> pub trait Provider: Send + Sync {
>     fn name(&self) -> &str;
>     fn stream(&self, req: &Request, cancel: &Cancel)
>         -> Result<Box<dyn Iterator<Item = Result<Chunk, ProvErr>> + Send>, ProvErr>;
> }
> ```
> The returned iterator's `next()` blocks on socket I/O (this is why threads, not async,
> §1). Connection/HTTP-status retry happens **before the first chunk is yielded**
> (PCORE §8.1); a mid-stream error is yielded as `Err(ProvErr::Stream)` and is fatal to the
> turn.

---

## 11. Cache placement (pure function)

> **R-CORE-200 → DESIGN §8.4, §8.4.1**
> ```rust
> #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
> #[serde(tag = "at", rename_all = "snake_case")]
> pub enum Cache {
>     System,                  // system prompt; tools share this prefix
>     AfterMessage(usize),     // index into Request::messages
> }
>
> #[derive(Clone, Serialize, Deserialize)]
> #[serde(tag = "mode", rename_all = "lowercase")]
> pub enum CacheMode {
>     Explicit { max_breakpoints: u8, min_tokens: u32 },
>     Automatic,
>     None,
> }
> ```

> **R-CORE-210 → DESIGN §8.4, §8.4.1**
> The crate MUST expose `pub fn breakpoints(messages: &[Message], mode: &CacheMode) ->
> Vec<Cache>`, provider-independent, mirroring the opencode plugin's `applyCaching`. It
> MUST:
> 1. return empty for any mode other than `Explicit`;
> 2. build candidates in this exact order: `System`, `AfterMessage(last_user)`,
>    `AfterMessage(len-2)`, `AfterMessage(len-1)`, skipping any that don't exist;
> 3. deduplicate positions (a repeat wastes one of only `max_breakpoints` slots);
> 4. return the first `max_breakpoints` survivors, **in priority order — NOT sorted by
>    position**.
> ```rust
> pub fn breakpoints(messages: &[Message], mode: &CacheMode) -> Vec<Cache> {
>     let CacheMode::Explicit { max_breakpoints, .. } = mode else { return vec![] };
>     let last_user = messages.iter().rposition(|m| m.role == Role::User);
>     let len = messages.len();
>     let candidates = [
>         Some(Cache::System),
>         last_user.map(Cache::AfterMessage),
>         len.checked_sub(2).map(Cache::AfterMessage),
>         len.checked_sub(1).map(Cache::AfterMessage),
>     ];
>     let mut out = Vec::new();
>     for c in candidates.into_iter().flatten() {
>         if out.contains(&c) { continue; }
>         out.push(c);
>         if out.len() == *max_breakpoints as usize { break; }
>     }
>     out
> }
> ```
> **Accept:** `cargo test core::breakpoints` MUST include these cases:
> - `[assistant, user]`, cap 4 → `[System, AfterMessage(1), AfterMessage(0)]`
>   (proves priority-order, descending positions — the exact §8.4.1 case).
> - single user message, cap 4 → `[System, AfterMessage(0)]` (dedup collapses lastUser,
>   last, secondLast).
> - `mode = Automatic` → `[]`.
> - cap 2 on a long conversation → exactly 2 entries, the two stable anchors first.

---

## 12. The bus

> **R-CORE-220 → DESIGN §3, §3.1, §5.1**
> The crate MUST provide a broadcast bus for **transient** events only. Its contract:
> - publishing NEVER blocks the publisher (Principle 3);
> - each subscriber has a **bounded** queue; when full, the oldest transient event is
>   dropped (§5.1 self-healing covers the loss);
> - it carries no reply channel — value-returning work is a trait call, not a bus message.
> ```rust
> pub struct Bus { /* Vec<Sender> behind Mutex, bounded channels */ }
> pub struct Subscription { /* Receiver<Event> */ }
> impl Bus {
>     pub fn new() -> Self;
>     pub fn subscribe(&self, capacity: usize) -> Subscription;
>     pub fn publish(&self, event: Event);   // non-blocking; may drop for slow subs
> }
> impl Subscription {
>     pub fn recv(&self) -> Option<Event>;       // blocks; None when bus dropped
>     pub fn try_recv(&self) -> Option<Event>;   // non-blocking
> }
> ```
> **Accept:** `cargo test core::bus_never_blocks` — a subscriber that never drains does not
> stall `publish`; after `capacity` publishes, the oldest are dropped and newest retained.

> **R-CORE-225 → DESIGN §3.1, GI-4**
> The bus MUST NOT be the persistence path. Durable events reach disk via
> `Store::append` (STOR), which assigns `seq` and commits **before** the event is published
> to the bus for observers. `kn9t-core` provides the `Store` trait (R-CORE-250) but the bus
> itself neither writes nor guarantees delivery of durable events.

> **R-CORE-230 → DESIGN §8, §11**
> The transient-event sink used by `assemble()` (PCORE) and by tools (TOOL) to emit
> deltas/progress without knowing about the bus or store:
> ```rust
> pub trait EventSink: Send + Sync { fn emit(&self, e: Event); }
> ```
> A `Bus` MUST implement `EventSink` (its `emit` delegates to `publish`). Durable events
> MUST NOT be emitted through an `EventSink`.

---

## 13. Cancellation

> **R-CORE-240 → DESIGN §9.1, §3.2**
> `Cancel` is scoped to one turn, created by the ReAct loop at turn start, and passed to
> `Provider::stream` and every `Tool::execute`. It is never a bus message.
> ```rust
> #[derive(Clone)]
> pub struct Cancel(Arc<CancelInner>);   // AtomicBool + Condvar + Mutex<()>
> impl Cancel {
>     pub fn new() -> Self;
>     pub fn cancelled(&self) -> bool;                       // non-blocking poll
>     pub fn cancel(&self);                                  // idempotent; wakes waiters
>     pub fn wait_timeout(&self, d: Duration) -> bool;       // returns true if cancelled
> }
> ```
> `Cancel` is `Send + Sync + Clone` (clones share one flag). It is the one type in core
> holding an `Arc`; it is never an `Event` payload, so R-CORE-030 is not violated.
> **Accept:** `cargo test core::cancel_wakes` — a thread blocked in `wait_timeout` returns
> promptly when another thread calls `cancel`.

---

## 14. Store, tool, policy traits

These traits are defined in core (so `kn9t-react` sees only `dyn Trait`, GI-1) and
implemented in later stages.

> **R-CORE-250 → DESIGN §7.5, §3.1**
> ```rust
> #[derive(Clone, Serialize, Deserialize)]
> pub struct SeqRange { pub start: u64, pub end: u64 }
>
> #[derive(Clone)]
> pub struct CompactSpan { pub replaced: SeqRange, pub messages: Vec<Message> }
>
> pub struct RequestPlan {
>     pub system:   Option<String>,
>     pub messages: Vec<Message>,
>     pub tools:    Vec<ToolSpec>,
>     pub cache:    Vec<Cache>,
>     pub compact:  Option<CompactSpan>,   // Some ⇒ summarize before sending
> }
>
> #[derive(Clone, Serialize, Deserialize)]
> pub struct SessionSnapshot {
>     pub head_seq:   u64,
>     pub ctx_tokens: u32,
>     pub cost_usd:   f64,     // this session's own spend, excludes inherited
>     pub model:      ModelRef,
> }
>
> pub trait Store: Send + Sync {
>     fn plan_request(&self, session: &SessionId) -> Result<RequestPlan, StoreErr>;
>     /// Assigns seq, writes events + projections in one txn, returns the seq (§3.1).
>     fn append(&self, session: &SessionId, event: Event) -> Result<u64, StoreErr>;
>     fn snapshot(&self, session: &SessionId) -> Result<SessionSnapshot, StoreErr>;
> }
> ```
> `plan_request` also computes cache breakpoints (it already walks the messages and holds
> `ModelSpec`); §7.5.

> **R-CORE-260 → DESIGN §11, §11.1, §11.2**
> ```rust
> pub struct ToolOutput {
>     pub content:  Vec<Content>,       // what the MODEL sees, truncated
>     pub details:  Option<serde_json::Value>,   // what UI/DB see, full
>     pub is_error: bool,
> }
>
> pub struct ToolCtx {
>     pub cwd:  PathBuf,
>     pub read: Arc<Mutex<HashMap<PathBuf, (Sha256, SystemTime)>>>,  // edit staleness guard
>     pub bus:  Arc<dyn EventSink>,
> }
> pub type Sha256 = [u8; 32];
>
> pub trait Tool: Send + Sync {
>     fn spec(&self) -> &ToolSpec;
>     fn execute(&self, args: &serde_json::Value, ctx: &ToolCtx, cancel: &Cancel)
>         -> Result<ToolOutput, ToolErr>;
>     fn parallel_safe(&self) -> bool { false }
> }
> ```
> `ToolCtx.read`'s `HashMap` is internal shared state, never serialized (GI-3 concerns
> serialization only); the lock is held only for lookup/insert, never across I/O (§11.2).

> **R-CORE-270 → DESIGN §10**
> ```rust
> #[derive(Clone)]
> pub struct ToolCall { pub id: CallId, pub name: String, pub args_json: String }
>
> #[derive(Clone, Serialize, Deserialize)]
> #[serde(tag = "decision", rename_all = "lowercase")]
> pub enum Decision { Allow, Deny { reason: String } }
>
> pub trait Policy: Send + Sync {
>     fn check(&self, call: &ToolCall, cwd: &Path) -> Decision;
> }
> ```
> `ToolCall` here is the dispatch-time view (fully accumulated args, no `Content`
> wrapper); it is distinct from `Content::ToolCall`.

---

## 15. Stage gate

> **R-CORE-900**
> Stage 1 is **done** when: `cargo build -p kn9t-core` succeeds with zero warnings under
> `-D warnings`; `cargo test -p kn9t-core` passes every `core::*` acceptance test named
> above; CI checks GI-1, GI-2, GI-3, GI-5 against this crate; and `cargo doc` produces no
> broken intra-doc links. No later stage may begin until this gate is green.
