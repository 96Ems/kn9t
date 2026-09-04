# Stage 07 — TUI Client

> Implements the terminal user interface for kn9t.
> See `docs/TUI-DESIGN.md` for design rationale and grill session output.
>
> **Reconciliation note (Phase 5, 2026-08-31):** Many R-TUI-* requirements have no acceptance test and some
> describe behavior not yet built. Per AGENTS.md §9, an unimplementable MUST is a spec bug. Status:
> - **R-TUI-012** was stale — wire uses `snake_case` per AGENTS.md §12 (`rename_all = "snake_case"`), not
>   PascalCase; amended below and verified by `cargo test -p xtask` + `check-schema.sh` (`wire.rs`/`api.rs` generated).
> - **R-TUI-050** amended — `GET /tools` (Phase 4) drives the sidebar from the server's `ToolRegistry`
>   (discovered `~/.kn9t/plugins/` + pinned `[[plugin]]`); the local `enabled` toggle was dead code (`app.rs:1789`)
>   and now refreshes from `GET /tools` instead of lying.
> - **R-TUI-110** amended — `pending_images` renamed to `staged_images` (`app.rs:128`) per AGENTS.md §10
>   patch-smell fix; multiple `queued_*` buffers eliminated Phase 4.4c.
> - **R-TUI-220** deferred to v2 per `docs/internal/job/phase4.md:9` user decision — "Do not add an extensibility seam to a
>   2,814-line god object. Decompose first (4.4), then revisit widgets (4.5)". Spec shape (`SidebarWidget` enum)
>   is correct and will be populated from server data once the TUI is decomposed; `tui::plugin_sidebar` will remain
>   `✗` until then.
> - **R-TUI-230** built Phase 4 — `app.rs:630` reconnects from `last_seq` (`session_manager.rs:115 ?from=`), honestly
>   shows "reconnecting..." (`reducer::sse_reconnect_seq_tracking` + `tui::sse_reconnect` green).
> - Remaining R-TUI-01x–21x/24x without tests are honest `☐`/`▣` in `TRACKING.md`; no silent `☑`.

## 1. Crate Structure

> **R-TUI-010 → DESIGN §13, GI-6**
> The TUI crate MUST be named `kn9t-tui` and MUST NOT depend on any `kn9t-*` workspace
> crates (GI-6). It communicates with the server exclusively via HTTP + SSE.
> Dependencies: `ratatui`, `crossterm`, `ureq`, `serde`, `serde_json`.

## 1.1 Environment Variables

> **R-TUI-011**
> The TUI MUST read connection info from environment variables:
> - `KN9T_URL` — server base URL (e.g., `http://127.0.0.1:7474`)
> - `KN9T_TOKEN` — authentication token
> - `KN9T_MODEL` — default model ID (optional)
>
> If `KN9T_URL` or `KN9T_TOKEN` are not set, the TUI MUST fall back to reading
> `~/.kn9t/port` and `~/.kn9t/token` respectively.
>
> **Accept:** `cargo test tui::env_vars` — TUI reads env vars correctly.

## 1.2 API Compliance

> **R-TUI-012 (amended Phase 5)**
> The TUI wire types (`wire.rs`) MUST match the server API as generated from `schema/http.json`:
> - SSE events use `#[serde(rename_all = "snake_case")]` → `text_delta`, `message_appended`, etc. (AGENTS.md §12; previous spec incorrectly said PascalCase)
> - Lease response reads `body["lease"]` (not `body["holder"]`)
> - All request/response payloads match the generated `api.rs`/`wire.rs`/`API.md` schemas exactly (ADR-0005, `cargo run -p xtask -- generate` + `check-schema.sh`)
>
> If a discrepancy is found between server behavior and `schema/http.json`, update `schema/http.json` (schema is authoritative).
>
> **Accept:** `cargo run -p xtask -- generate` idempotent + `check-schema.sh` OK + `cargo test -p kn9t-tui --lib` wire decode green.

## 2. Event Architecture

> **R-TUI-020 → TUI-DESIGN §1.2**
> The TUI MUST use a pure event-driven architecture with zero polling.
> A unified channel receives events from three sources:
> - Keyboard/mouse thread (crossterm)
> - SSE thread (server events)
> - Timer thread (spinner ticks, only active during streaming)
>
> The main loop MUST block on `recv()` — zero CPU when idle.
> Redraw MUST occur only on state change, never on a timer loop.
>
> ```rust
> enum TuiEvent {
>     Key(KeyEvent),
>     Mouse(MouseEvent),
>     Resize(u16, u16),
>     Sse(SseFrame),
>     Tick,
> }
> ```
>
> **Accept:** `cargo test tui::event_loop_blocks` — verify loop blocks, no busy spin.

## 3. Layout

> **R-TUI-030 → TUI-DESIGN §2**
> The TUI MUST implement a 2-column layout:
> - Center: transcript + input + status bar
> - Right sidebar: context panel (24 cols expanded, 2 collapsed)
>
> Right sidebar MUST collapse/expand via keybind.
> If terminal width < 60 cols, sidebar MUST hide entirely.
>
> **Accept:** `cargo test tui::layout_responsive` — layout adapts to terminal size.

## 4. Session Picker Overlay

> **R-TUI-040 → TUI-DESIGN §3**
> Sessions MUST be accessible via `/session` slash command or `Ctrl+B` keybind.
> The session picker MUST be a modal overlay with:
> - Title: "SELECT SESSION"
> - Filter input with fuzzy matching (type to filter)
> - Session list showing:
>   - Spinner icon if turn running
>   - Arrow icon if current session
>   - Session name (truncated to fit)
> - Footer: navigation hints
>
> Up/Down arrows navigate, Enter selects, Esc cancels.
> Running turns MUST continue in background after switch.
>
> **Accept:** `cargo test tui::session_switch` — switching works, turn continues.

## 5. Right Sidebar — Context Panel

> **R-TUI-050 → TUI-DESIGN §4 (amended Phase 4)**
> The right sidebar MUST display collapsible sections:
> - MODEL: name, cost, tokens (in/out)
> - TOOLS/PLUGINS: list from `GET /tools` (server is source of truth, ADR-0005; discovered `~/.kn9t/plugins/` + pinned `[[plugin]]` merged, first-wins dedup). The previous "toggleable checkbox per tool" was dead code (`app.rs:1789` flipped `enabled` and nothing read it) and has been removed; clicking refreshes from `GET /tools` (future per-tool enable would be a server endpoint, not a lying local flip, per AGENTS.md §11).
> - GIT: branch, uncommitted count, changed files list
>
> Clicking a git file MUST open the diff viewer overlay.
>
> Configurable via `[tui] right_sidebar = true|false`.
>
> **Accept:** `cargo test -p kn9t-tui` `refresh_tools` path (`app.rs:233` `GET /tools` → `ToolEntry`) + `cargo test -p kn9t-server --test acceptance` `srv::tools_*` (hardcoded list gone).

## 6. Transcript

> **R-TUI-060 → TUI-DESIGN §5**
> The transcript MUST display messages with:
> - Role labels (user, assistant)
> - Tool cards (collapsible, lazy-loaded)
> - Error cards (inline, persisted)
>
> Tool cards MUST show: `▶ tool [args] status`
> - Running: spinner
> - Completed: `✓` collapsed
> - Error: `✗` with error type
>
> Click or Enter on a tool card MUST expand/collapse it.
> Tool input/output MUST be fetched lazily on expand.
>
> **Accept:** `cargo test tui::tool_card_lazy` — data fetched only on expand.

## 7. Virtual Scrolling

> **R-TUI-070 → TUI-DESIGN §5.4**
> The transcript MUST implement virtual scrolling:
> - Initial load: last 50 messages
> - Scroll to top: show "Load earlier messages" button
> - Load chunk: fetch previous 50, prepend, preserve scroll position
>
> **Accept:** `cargo test tui::virtual_scroll` — loads incrementally.

## 8. Scroll Behavior

> **R-TUI-080 → TUI-DESIGN §5.3**
> During streaming, transcript MUST auto-scroll to follow new content.
> If user scrolls up, auto-scroll MUST disengage.
> A "↓ Jump to end" button MUST appear when scrolled up during streaming.
> Pressing `End` or `G` MUST snap to bottom and re-enable auto-scroll.
>
> Navigation keys:
> - `j`/`k`: scroll line
> - `gg`/`G`: top/bottom
> - `[u`/`]u`: prev/next user message
> - `[a`/`]a`: prev/next assistant message
>
> **Accept:** `cargo test tui::scroll_auto_disengage` — scrolling up disengages auto-scroll.

## 9. Input Box

> **R-TUI-090 → TUI-DESIGN §6**
> The input box MUST support:
> - `Enter`: send message
> - `Shift-Enter`, `Ctrl-Enter`, `Alt-Enter`: insert newline
> - `Ctrl-E`: open `$EDITOR` with current content
>
> **Accept:** `cargo test tui::input_multiline` — modifier+Enter inserts newline.

## 10. File Mentions

> **R-TUI-100 → TUI-DESIGN §6.2**
> Typing `@` MUST trigger autocomplete showing files/directories.
> - `@path/to/file`: embed file content in message
> - `@directory/`: embed directory listing (tree format)
>
> Autocomplete MUST fuzzy-match as user types.
>
> **Accept:** `cargo test tui::file_mention_autocomplete` — autocomplete works.

## 11. Image Paste

> **R-TUI-110 → TUI-DESIGN §6.3 (amended Phase 4.4c)**
> Pasting an image MUST:
> - Insert inline marker at cursor: `[imgN: WxH PNG]` (e.g., `[img1: 982x414 PNG]`)
> - Store image as base64 data URI in `staged_images` (renamed from `pending_images` per AGENTS.md §10 patch-smell; `queued_*` buffers eliminated, handlers now take `&Sender<Event>` and act immediately)
> - On send: server stores blob (SHA-256), resolves to data URI before provider call
>
> Multiple images supported: `[img1: ...] text [img2: ...]` allows inline references.
> User message added locally (no SSE round-trip for display).
> Image MUST NOT be rendered in terminal.
>
> **Accept:** `cargo test tui::image_paste` — marker inserted, image stored, sent to model (impl exists; test is overlay render but is `▣` until `tui::image_paste` is wired).

## 12. Status Bar

> **R-TUI-120 → TUI-DESIGN §7**
> Bottom status bar MUST display:
> - Session ID (truncated)
> - Model name
> - Cost
> - Tokens (in/out)
> - Status (spinner + phrase when streaming)
> - Help hint (`?=help`)
>
> Streaming indicator: animated braille spinner + rotating phrases.
> Phrases configurable via `[tui.streaming] phrases = [...]`.
>
> **Accept:** `cargo test tui::status_bar_streaming` — spinner animates during stream.

## 13. Approval Overlay

> **R-TUI-130 → TUI-DESIGN §8.1**
> When a permission request is pending, TUI MUST display a blocking overlay.
> User MUST NOT be able to type in input until resolved.
>
> Overlay MUST show:
> - Tool name and arguments
> - For edits: diff preview
> - Options: `[Allow]` `[Always]` `[Reject]`
>
> Keys:
> - `←`/`→`: navigate options
> - `Enter`: confirm
> - `Esc` or `n`: reject
> - `y`: allow
> - `a`: always
>
> **Accept:** `cargo test tui::approval_blocks_input` — cannot type while overlay open.

## 14. Diff Viewer

> **R-TUI-140 → TUI-DESIGN §8.2**
> The diff viewer MUST display side-by-side diff (old | new).
> Clicking ANY line (not just changed) MUST allow adding a comment.
> Comments MUST be listed at bottom of viewer.
>
> On close, comments MUST append to input box as:
> ```
> [file:line] comment text
> ```
>
> User can edit input before sending.
>
> **Accept:** `cargo test tui::diff_comment` — comments append to input.

## 15. Help Overlay

> **R-TUI-150 → TUI-DESIGN §8.3**
> Pressing `?` MUST display full-screen help overlay.
> Help MUST show user's actual keybindings (from config).
> Organized in columns by category: navigation, sessions, actions.
>
> `Esc` or `?` closes the overlay.
>
> **Accept:** `cargo test tui::help_shows_bindings` — displays configured bindings.

## 16. Keybindings

> **R-TUI-160 → TUI-DESIGN §9**
> Default keybindings MUST be vim-style.
> User MUST be able to override any binding in config:
>
> ```toml
> [keybinds]
> leader = "space"
> scroll_up = "k"
> scroll_down = "j"
> session_list = "<leader>s"
> quit = "<C-q>"
> ```
>
> Leader key: configurable (default `space`), 2s timeout.
>
> **Accept:** `cargo test tui::keybind_override` — user bindings override defaults.

## 17. Mouse Support

> **R-TUI-170 → TUI-DESIGN §10**
> Full mouse support MUST be implemented:
> - Hover: expand sidebars, highlight items
> - Click: select, toggle, expand/collapse
> - Drag: text selection (terminal native)
> - Scroll: mouse wheel scrolls transcript
>
> **Accept:** `cargo test tui::mouse_hover_sidebar` — hover expands sidebar.

## 18. Theming

> **R-TUI-180 → TUI-DESIGN §11**
> TUI MUST auto-detect terminal background (light/dark).
> User MUST be able to override colors:
>
> ```toml
> [theme]
> mode = "auto"  # "auto" | "light" | "dark"
>
> [theme.colors]
> background = "#1e1e2e"
> primary = "#89b4fa"
> error = "#f38ba8"
> ```
>
> **Accept:** `cargo test tui::theme_override` — user colors applied.

## 19. Error Display

> **R-TUI-190 → TUI-DESIGN §12**
> Errors MUST be displayed as inline cards in transcript.
> Errors MUST be persisted in DB (survive session reload).
> Error cards MUST be collapsible (details hidden by default).
> Error text MUST be copyable.
>
> Error types: `API Error`, `Network Error`, `Tool Error`, `Timeout`, `Aborted`.
>
> **Accept:** `cargo test tui::error_persisted` — error survives reload.

## 20. Confirmations

> **R-TUI-200 → TUI-DESIGN §13**
> Confirmation dialogs MUST appear for:
> - Quit while turn running
> - Delete session
> - Clear conversation
>
> NO confirmation for: abort, switch session, send long message.
>
> **Accept:** `cargo test tui::quit_confirm` — quitting during turn shows confirm.

## 21. Git Integration

> **R-TUI-210 → TUI-DESIGN §14**
> Git section in right sidebar MUST show:
> - Current branch
> - Uncommitted changes count
> - Changed files with +/- stats
>
> Clicking a file MUST open diff viewer.
> Git actions (commit, switch branch) via tools/plugins.
>
> **Accept:** `cargo test tui::git_sidebar` — git info displayed.

## 22. Plugin Sidebar API

> **R-TUI-220 → TUI-DESIGN §4.4 (deferred to v2 per docs/internal/job/phase4.md:9)**
> Plugins MUST be able to contribute sidebar widgets via structured data:
>
> ```rust
> enum SidebarWidget {
>     Section { title, collapsed, content },
>     KeyValue { items },
>     List { items, selectable },
>     Toggle { label, enabled, on_toggle },
>     Tree { root },
>     Button { label, action },
> }
> ```
>
> No custom rendering — plugins return data, TUI renders.
> Deferred per user decision: "Do not add an extensibility seam to a 2,814-line god object. Decompose first (4.4), then revisit widgets (4.5)". Once widgets arrive as server data (like `GET /tools`), plugin-contributed UI is a schema addition, not a TUI rewrite. `tui::plugin_sidebar` remains `✗` until then.
>
> **Accept:** `cargo test tui::plugin_sidebar` — plugin widget renders (deferred).

## 23. SSE Connection

> **R-TUI-230**
> TUI MUST connect to server via SSE for real-time updates.
> On disconnect, TUI MUST show "reconnecting..." and retry.
> On reconnect, TUI MUST request events from last known seq.
>
> **Accept:** `cargo test tui::sse_reconnect` — reconnects after disconnect.

## 24. Server Lifecycle

> **R-TUI-240**
> If server is not running, TUI MUST auto-start it (like CLI).
> TUI MUST read `~/.kn9t/{port,token}` for connection.
>
> **Accept:** `cargo test tui::server_autostart` — starts server if needed.

## 25. Gate G3

> **R-TUI-900 → DESIGN §16**
> Gate G3: 3 TUI instances against 1 server, 1 session.
> - All 3 see real-time SSE updates
> - Only 1 has lease at a time
> - Lease acquisition is transparent (backoff)
> - Screenshot paste shows placeholder
>
> **Accept:** Manual test — open 3 TUIs, verify lease handoff, paste image.

