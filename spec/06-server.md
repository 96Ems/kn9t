# 06 — `kn9t-server`

**Crate:** `kn9t-server`
**Depends on:** this is the **one** crate permitted more than one workspace dependency
(GI-1 exception): it wires `kn9t-core`, `kn9t-store`, `kn9t-react`, `kn9t-tools`, every
provider crate, and `kn9t-plugin`. External: `tiny_http`. It is the only crate that names
concrete `Provider`/`Tool`/`Store`/`Policy` types (§2, §12).
**DESIGN:** §12, §12.1–12.7, §8.7.3, §18.3, §18.14.
**Build order:** stage 6 of 10.

The server is **always** a separate process, even when a client just spawned it — one
wiring path only (§12). `kn9t -p` is a client exactly like the TUI.

---

## 1. HTTP surface

> **R-SRV-010 → DESIGN §12.1**
> The server MUST expose exactly this surface over `tiny_http` (blocking,
> thread-per-connection). `[lease required]` endpoints MUST return `409 session_busy` unless
> the caller holds the write lease (R-SRV-060).
> ```
> POST   /session                        create; body {cwd, model?, name?}
> GET    /session                        list
> GET    /session/{id}                   snapshot {meta, head_seq, transcript}
> POST   /session/{id}/fork              {origin_seq, reason} -> new session id
> DELETE /session/{id}
> GET    /session/{id}/events?from={seq} SSE; replays durable > seq, then live
> POST   /session/{id}/lease             acquire write lease (?takeover=1 to steal)
> DELETE /session/{id}/lease             release
> POST   /session/{id}/prompt            {text, blobs?, images?}    [lease required]
> POST   /session/{id}/steer             {text}                     [lease required]
> POST   /session/{id}/abort                                        [lease required]
> POST   /session/{id}/model             {provider, id}             [lease required]
> POST   /approve                        {id, decision, scope}      [lease required]
> POST   /blob                           body: bytes -> {hash, mime}
> GET    /blob/{hash}                    bytes, ETag, immutable
> GET    /models                         resolved registry + auth status
> GET    /cost?since=&group_by=          analytics over the usage projection
> GET    /budget                         provider-reported spend (§8.7.3)
> ```
> **Accept:** `cargo test srv::routes` — each route exists with the specified method and
> lease requirement; unknown routes 404.

> **R-SRV-015 → DESIGN §12.1**
> `tiny_http` MUST be the HTTP layer; `axum`/`hyper`/tokio are rejected (§12.1: ~100 crates
> reversing Principle 1 for one POST and one SSE). GI-5 (no async) holds.

## 2. Auth (mandatory)

> **R-SRV-020 → DESIGN §12.5**
> Auth MUST be mandatory. At startup the server MUST write a random 32-byte token to
> `~/.kn9t/token` (mode 0600 where the OS supports it) and the listening port to
> `~/.kn9t/port`. Every request MUST carry `Authorization: Bearer <token>`; a missing or
> wrong token is `401`.
> **Accept:** `cargo test srv::auth_required` — a request without the token is rejected.

> **R-SRV-030 → DESIGN §12.5**
> Any request carrying a cross-origin `Origin` header MUST be rejected (a webpage `fetch`
> must not be able to drive the agent, §12.5).
> **Accept:** `cargo test srv::origin_rejected`.

## 3. SSE replay — the attach race

> **R-SRV-040 → DESIGN §12.4, §5.1**
> `GET /session/{id}/events?from={seq}` MUST close the attach race in this exact order:
> 1. **subscribe first**, buffering everything that arrives;
> 2. read durable rows `> from` up to the current `head_seq`; emit them;
> 3. read `live_messages` for the in-flight partial, if any (R-SRV-050);
> 4. flush the buffer, **discarding anything with `seq <= head_seq`** (exact dedup, since
>    durable seqs are gapless, §3.1).
> Read-then-subscribe is forbidden (loses events committed in the gap; §12.4). The backlog
> read MUST NOT hold the write lock (a 400-event attach would stall the agent; §12.4).
> **Accept:** `cargo test srv::sse_no_gap_no_dup` — an event committed during the attach
> window is delivered exactly once.

> **R-SRV-050 → DESIGN §12.3, §5.2**
> On attach, the server MUST surface the in-flight assistant text from `live_messages`
> (STOR R-STOR-170) so a client attaching mid-stream sees partial text. This read is a
> display convenience; the authoritative `MessageAppended` still follows and supersedes it.
> A crash loses the partial (§5.2).

## 4. Session lifecycle and spawn

> **R-SRV-060 → DESIGN §12.6**
> Many observers, one writer. All attached clients receive the same SSE stream; exactly one
> holds the write lease and may `prompt`/`steer`/`abort`/`approve`/set `model`. The lease
> releases on explicit `DELETE`, on disconnect, or after an idle timeout (default 5 min,
> **SPEC-OPEN** §12.6), and MAY be stolen with `?takeover=1`.
> **Accept:** `cargo test srv::lease_single_writer` — a second lease acquire gets 409;
> `?takeover=1` succeeds and the prior holder's writes then 409.

> **R-SRV-070 → DESIGN §12.2, §2**
> Any client (including `kn9t -p`) MUST auto-spawn a server when none is listening:
> 1. take an exclusive lock on `~/.kn9t/spawn.lock`;
> 2. spawn `kn9t serve` **detached**, poll for `~/.kn9t/port`, connect;
> 3. a port file pointing at a closed socket is stale → delete and respawn;
> 4. release the lock.
> An in-process server for `-p` is forbidden (the second wiring path §2 prevents).
> **Accept:** `cargo test srv::spawn_race` — two clients starting together result in exactly
> one server; a stale port file triggers a respawn.

> **R-SRV-080 → DESIGN §12.2, §18.14**
> The server MUST exit after a short grace period once **all clients have disconnected and
> no turn is running**. Default grace: **5 seconds** (resolved from SPEC-OPEN §18.14).
> As long as any SSE client is connected the server MUST remain up regardless of inactivity.
> The grace period is configurable via `[server] idle_exit_secs` in `~/.kn9t/config.toml`;
> setting it to `0` disables auto-exit.
>
> The server MUST also accept `POST /stop` (auth required) for an explicit graceful shutdown.
> The server MUST send SSE keepalive pings (`: keepalive\n\n`) on a regular interval
> (default 15 s, `KN9T_SSE_HEARTBEAT_MS` in tests) so that a disconnected client is
> detected promptly via a write failure even when no events are being produced.
>
> **Accept:** `cargo test srv::idle_exit` — a server with no clients and no turn exits after
> the configured period; one with an attached client does not.
> **Accept:** `cargo test srv::stop_route` — `POST /stop` returns 200 and server shuts down.
> **Accept:** `cargo test srv::keepalive_detects_dropped_client` — dropped SSE client is
> detected within one heartbeat cycle; server idle-exits after grace period.

> **R-SRV-085 → DESIGN §12.7**
> `POST /session/{id}/prompt` accepts:
> - `text`: user message text
> - `blobs`: array of sha256 hashes (pre-uploaded via `/blob`)
> - `images`: array of base64 data URIs (`data:image/png;base64,...`)
>
> Inline images (`images` field) are parsed, stored as blobs, and referenced as
> `Content::Image { sha256: "sha256:<hash>", mime }`. The plan_request layer resolves
> blob refs to inline data URIs before sending to providers.
>
> **Accept:** `cargo test srv::prompt_with_images` — images stored as blobs, resolved for provider.

## 5. Blobs

> **R-SRV-090 → DESIGN §12.7, R-STOR-140**
> `POST /blob` MUST compute SHA-256, store once via the store (R-STOR-140), and return
> `{hash, mime}`. `GET /blob/{hash}` MUST return bytes with `ETag: "<hash>"` and
> `Cache-Control: immutable`. Messages/events carry `sha256:` refs, never base64 (§12.7).
> The provider layer resolves refs to bytes when building a request.
> **Accept:** `cargo test srv::blob_roundtrip` — put returns a hash; get returns identical
> bytes with the ETag; a second put of the same bytes reuses the row.

## 6. Session titling (decision: auto-title)

> **R-SRV-100 → DESIGN §18.3 (decision: auto-title, README §6)**
> After the **first assistant turn** of a session that has no `name`, the server MUST issue
> one cheap provider call to generate a short title, recording its usage as
> `UsageKind::Title` (R-CORE-150). It MUST be **best-effort**: on any failure the title stays
> null and no error surfaces to the user. A `name` supplied at creation or via API
> suppresses auto-titling.
> **Accept:** `cargo test srv::autotitle` — a nameless session gets a title after turn 1 and
> a `usage` row with `kind=title`; a provider failure leaves `name` null with no client
> error.

## 7. Cost and budget

> **R-SRV-110 → DESIGN §12.1, §7.3**
> `GET /cost?since=&group_by=` MUST serve the analytics the store computes (STOR R-STOR-180):
> totals by model/kind/session and the three §7.3 figures (marginal / effective / family).
> **Accept:** `cargo test srv::cost_query`.

> **R-SRV-120 → DESIGN §12.1, §8.7.3, §18.8**
> `GET /budget` MUST report provider-reported spend where available (gateway
> `/user/usage`, R-NBED-040) alongside the locally computed estimate. Drift between the two
> is **not** warned in v1 (**SPEC-OPEN** §18.8); both figures are simply returned.
> **Accept:** `cargo test srv::budget_reports_both`.

## 8. Config file — provider headers

> **R-SRV-CFG-010 → DESIGN §8.2, §14**
> The global config `~/.kn9t/config.toml` MUST support a per-provider `[provider.X.headers]`
> table. Each key-value pair is a header name and value injected verbatim on every request to
> that provider (via R-OAI-050). Values support the same `env:VAR` interpolation as `api_key`.
>
> ```toml
> [provider.my-gateway.headers]
> X-User-Id         = "env:GATEWAY_USER_ID"
> source_identifier = "my_app_id"
>
> [provider.my-plugin.headers]
> Authorization = "env:PLUGIN_API_KEY"
> ```
>
> The config layer resolves `env:VAR` at load time and passes the resulting
> `Vec<(String, String)>` into `OpenAiConfig::extra_headers`. No provider crate knows
> which deployment it is serving.
> **Accept:** `cargo test srv::config_headers` — a config with `[provider.X.headers]` results
> in those headers being sent (verified via a local test server capturing raw requests).

> **R-SRV-CFG-020 → DESIGN §14**
> Header values resolved from `env:VAR` that are empty or missing MUST cause the server to
> log a warning and omit that header (not send an empty value). A missing env var for `api_key`
> remains a hard error; for headers it is a soft warning because some deployments are
> partially keyed.
> **Accept:** covered by `srv::config_headers` — a missing env var produces a log warning and
> the header is absent from the request.

## 9. Stage gate

> **R-SRV-900 → DESIGN §12**
> Stage 6 is **done** when: every route in R-SRV-010 responds with correct method/lease
> semantics; auth + Origin rejection hold; the SSE attach race delivers exactly-once across
> a committed-during-attach event; leases enforce single-writer with takeover; auto-spawn
> is race-free and idle-exit fires; blobs round-trip with dedup and ETag; auto-titling works
> best-effort; `/cost` + `/budget` serve correct figures; and `[provider.X.headers]` injects
> extra headers per R-SRV-CFG-010. This crate is the sole GI-1 exception; all other crates
> it links still satisfy GI-1 individually.
