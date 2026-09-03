# TUI test strategy (96E-19)

## Why the gap existed
`kn9t-tui` was 14.6k loc with ~22 lines of test in early audits because most code was
terminal-bound (`App::run`, crossterm events, HTTP client). `DESIGN §12.8` warned about
the TUI dwarfing its core — it did. The fix is not to unit-test the terminal, but to
separate pure logic from I/O glue and test the former heavily.

## What is tested (pure logic, no PTY)

- **`reducer.rs` — the primary seam.** Pure `(State, SseFrame) -> State` with no I/O.
  Every `SseFrame` variant has at least one test: `TurnStarted`/`TurnEnded`/`TextDelta`/
  `ThinkingDelta`/`MessageAppended` (tool round-trip)/`UsageRecorded`/`ToolStarted`/
  `ToolProgress`/`ToolFinished`/`ApprovalRequest`/`InteractionRequest`/`ModelChanged`/
  `Compacted`/`Error`/`RetryAttempt`/`TurnStatus`/`TitleChanged`/`PluginNotification`/
  `HookFailed`. 25 tests after 96E-19 (was 14, was 2 in early audits). This is the
  most important function in the crate.

- **Helpers:** `message_handler` (9), `token_tracker` (6), `model_selector` (6),
  `session_manager` (4), `search` (4), `thinking` (7), `syntax` (5), `theme` (12),
  `hyperlinks` (4), `word_segmenter` (8), `prompt_history` (5), `input_history` (5),
  `kill_ring` (7), `slash` (1), `command_palette` (3), `which_key` (3),
  `prompt_stash` (4), `latex` (9), `diff_viewer` (5), etc. — total 140 lib tests
  after 96E-19 (was 126).

- **Golden snapshots:** `ui/render.rs :: golden::*` — 4 snapshot tests render real
  overlays (`Approval`, `Interaction` with/without input, `Help`) to a
  `ratatui::backend::TestBackend` and assert the buffer string contains the expected
  headings/payload/footer. This is the pattern for terminal-dependent code:
  render to a buffer, snapshot the string, no PTY needed. Add more snapshots by
  following `render_overlay_to_string`.

## What is intentionally left untested (I/O glue)

- `App::run` event loop, `crossterm` key/mouse binding, `Client` HTTP/SSE streaming,
  `Terminal` raw mode, clipboard/image paste PTY interactions. These are thin glue
  with no branching worth unit-testing; they are exercised manually via `tui-testing`
  MCP (live TUI against real server) and are not mocked.

## How to add tests
- New `SseFrame` handling → add a `reducer` test (construct `State`, call `reduce`,
  assert `State` fields and `transcript`).
- New overlay/widget → add a `golden` snapshot in `ui/render.rs` (build the
  `Overlay`/`App`, render to `TestBackend(60,15)`, assert `snap.contains(...)`).
- Pure helper → `#[test]` next to the helper.

## Metrics
- `kn9t-tui` src: ~14,815 loc (`wc -l src/**/*.rs`)
- lib tests: 140 passed after 96E-19 (was 126, was ~22 lines at audit)
- Test loc as fraction measurably increased; the important signal is that the
  pure-logic seam (`reducer`) went from 2 → 14 → 25 tests and that rendering has
  a reproducible snapshot harness.
