# kn9t Configuration Reference — `~/.kn9t/config.toml`

> **GENERATED FILE — do not edit by hand.** Regenerate with
> `cargo run -p xtask -- generate`. Source of truth: `schema/config.json` (ADR-0005).
> The parser in `crates/kn9t-server/src/config.rs` is the authoritative implementation
> (R-TUI-012); this document is derived from the schema and cannot drift from it. Any
> mismatch is a bug in the schema or the parser, not in this file.

## Contents

- Top level
- Models — `[[model]]`
- Provider quirks
- Providers — `[provider.<name>]`
- Server — `[server]`
- Policy — `[policy]`
- Policy approvals — `[policy.approvals]`
- Plugins — `[[plugin]]`

## Top level

Keys that sit at the root of the file, outside any table. Project-local config files may set these too; `[policy]` is global-only.

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `default_model` | string | — | no | Model id used for new sessions. `provider:id` or a bare `id` when unambiguous. Absent → the first declared `[[model]]`. |
| `title_model` | string | — | no | Model used to auto-title a session after its first assistant turn. Absent → the session's own model. A cheap model here saves money; titling is best-effort and never fails a turn. |

## Models — `[[model]]`

An array of tables: one entry per model the server may be asked to run. `provider` must match a declared `[provider.<name>]`.

*Table: `[[model]]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `provider` | string | — | yes | Name of the `[provider.<name>]` table that serves this model. |
| `id` | string | — | yes | Model id as kn9t refers to it (sessions store this). |
| `api_id` | string | — | no | Wire model id actually sent to the provider. Absent → `id`. |
| `ctx` | integer | — | yes | Context window in tokens. Drives the compaction / truncation ladder. |
| `max_out` | integer | — | yes | Maximum output tokens requested per turn. |
| `thinking` | "off" \| "low" \| "medium" \| "high" | medium | no | Reasoning effort. |
| `price_in` | float | 0 | no | USD per million input tokens, used for cost reports. |
| `price_out` | float | 0 | no | USD per million output tokens. |
| `price_cache_read` | float | 0 | no | USD per million cached input tokens read. |
| `price_cache_write` | float | 0 | no | USD per million cached input tokens written. |
| `cache` | "explicit" \| "automatic" \| "none" | automatic | no | Prompt-cache strategy for this model. |
| `cache_breakpoints` | integer | 4 | no | Number of cache breakpoints to place. |
| `cache_min_tokens` | integer | 1024 | no | Minimum prefix size before a breakpoint is worth placing. |
| `quirks` | table | — | no | Per-model overrides of the provider's wire quirks. |

## Provider quirks

Wire-format deviations from the OpenAI default (DESIGN §8.2). `RawQuirks` is shared: a model-level `quirks` overrides the provider-level one field by field. Absent fields fall back to the provider defaults.

*Table: `[model.quirks] / [provider.<name>.quirks]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `max_tokens_field` | string | — | no | Request key that carries the output cap (`max_tokens`, `max_completion_tokens`, …). |
| `system_role` | string | — | no | Role name for the system prompt. |
| `usage_in_stream` | boolean | — | no | Whether usage arrives on the SSE stream. |
| `finish_reason` | boolean | — | no | Whether a `finish_reason` is emitted. |
| `reasoning` | string | — | no | How reasoning content is carried on the wire. |
| `tool_result_name` | boolean | — | no | Whether a tool result carries the tool name alongside its id. |
| `thinking_style` | string | — | no | Provider thinking-block dialect. |
| `thinking_replay` | string | — | no | How prior thinking is replayed in later requests. |
| `require_tools` | boolean | — | no | Whether the tools array is mandatory on every request. |
| `streaming` | boolean | — | no | Whether the endpoint supports streaming. |
| `trim_trailing_whitespace` | boolean | — | no | Whether trailing whitespace is trimmed from messages. |
| `session_header` | string | — | no | Header used to carry a session id to the provider. |
| `api` | "chat" \| "responses" | — | no | OpenAI surface to use (R-OAI-060). |

## Providers — `[provider.<name>]`

One table per provider, keyed by the name `[[model]]` entries refer to. `kind` selects the provider implementation.

*Table: `[provider.<name>]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `kind` | string | — | yes | `openai` for an HTTP endpoint, or `plugin` for a provider plugin subprocess. |
| `base_url` | string | — | no | Required for `kind = "openai"`. Base URL of the endpoint. |
| `api_key` | string | — | no | Bearer token for `kind = "openai"`, if the endpoint needs one. |
| `binary` | string | — | no | Required for `kind = "plugin"`: binary name or absolute path. |
| `env` | map<string, string> | {} | no | Environment variables injected into the plugin subprocess. Values support `env:VAR` interpolation. |
| `headers` | map<string, string> | {} | no | Extra request headers (R-SRV-CFG-010, `openai` only). |
| `discover` | boolean | true | no | Fetch `/models` and register discovered models. Set `false` to skip auto-discovery (R-SRV-CFG-030). |
| `tls_insecure` | boolean | false | no | Skip TLS certificate verification. Intended for local test endpoints only. |
| `quirks` | table | — | no | Provider-wide wire quirks, overridable per model. |

## Server — `[server]`

Optional runtime knobs. All fields are optional and take the defaults shown; `0` disables a deadline or, for `max_turns`, means unbounded. `[server]` is not hot-reloaded by `POST /config/reload`.

*Table: `[server]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `idle_exit_secs` | integer | 1800 | no | Seconds of inactivity (no attached clients, no running turns) before the server exits. `0` disables auto-exit. |
| `approval_timeout_secs` | integer | 1800 | no | Seconds a tool call waits for a human approval before denial. `0` waits forever. A backstop, so a client that disappears mid-prompt cannot pin the turn thread. |
| `interaction_timeout_secs` | integer | 1800 | no | `interaction_request` wait with a live cancellable turn. `0` disables the deadline. |
| `interaction_timeout_no_cancel_secs` | integer | 120 | no | `interaction_request` wait with NO cancellable turn. Nothing can interrupt such a wait, so this deadline is the only exit. `0` disables the deadline. |
| `tool_cancel_grace_ms` | integer | 1500 | no | Milliseconds a cancelled tool batch waits for a `parallel_safe` tool to notice `Cancel` before the loop synthesises a result and ends the turn. Raise for slow tools, lower to make ESC snappier. |
| `max_turns` | integer | unbounded | no | Optional spend guard: maximum turns one run may take before it stops with "turn limit reached". **Absent or `0` means unbounded** — the loop runs until it goes idle or the user aborts, which is the default. Set a positive value only to cap a model that never goes idle. |

## Policy — `[policy]`

Global only (~/.kn9t/config.toml), never read from a project-local file. ADR-0008 moved risk decisions into the policy plugin; `mode` survives as a reporting value, and `[policy.approvals]` persists `scope=always` grants.

*Table: `[policy]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `mode` | "ask_on_mutation" \| "allow_all" \| "deny_all" \| "readonly" | ask_on_mutation | no | Reporting value for the active policy posture. |
| `approvals` | table | — | no | Persistent approval grants. |

## Policy approvals — `[policy.approvals]`

Written by the server when a user answers an approval with `scope=always`. Never hand-edit the `always` list; the `never` entries are managed separately.

*Table: `[policy.approvals]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `always` | array<string> | [] | no | Fingerprints of approvals the user chose to always allow. |

## Plugins — `[[plugin]]`

An array of tables overriding plugin discovery (ADR-0004, R-PLUG-100): `cmd` pins a path, `env` alone injects vars, `enabled = false` / `disabled = true` suppresses the discovered plugin of the same name. Global-only; a project-local file cannot honor a plugin entry.

*Table: `[[plugin]]`*

| Key | Type | Default | Required | Description |
|-----|------|---------|----------|-------------|
| `name` | string | — | yes | Matches the plugin's declared name (usually its binary file name); used for dedup. |
| `cmd` | array<string> | — | no | Command plus args to spawn. Omit (or set `enabled = false`) for an env-only override or a disable entry. |
| `env` | map<string, string> | {} | no | Environment variables to inject. Values support `env:VAR` syntax. |
| `enabled` | boolean | true | no | `false` disables the discovered plugin of the same name. |
| `disabled` | boolean | false | no | Alias for `enabled = false`. If `true`, the plugin is disabled. |

