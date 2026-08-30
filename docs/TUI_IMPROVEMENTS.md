# kn9t TUI Improvements Roadmap

Comprehensive list of all improvements needed to match or exceed Pi and OpenCode TUIs.

**Status**: 17 of 62 items completed (2026-08-30)

---

## Phase 1: Input UX (Critical)

### 1.1 Undo/Redo System ✅ DONE (a7e85e7)
- **File**: `crates/kn9t-tui/src/input_history.rs`
- **Priority**: 🔴 Critical
- **Effort**: 2 days
- **Description**: Track input state changes for undo/redo
- **Implementation**:
  - `Vec<InputSnapshot>` with cursor position, text, selection
  - Max 100 entries, circular buffer
  - Coalesce rapid keystrokes (debounce 300ms)
- **Keybinds**:
  - `Ctrl+-` or `Ctrl+Z`: Undo
  - `Ctrl+Shift+Z` or `Ctrl+Y`: Redo
- **References**:
  - Pi: `editor.ts` undo stack
  - OpenCode: `⌘u/⌘r` bindings

### 1.2 Kill Ring (Emacs-style) ✅ DONE (788238e)
- **File**: `crates/kn9t-tui/src/kill_ring.rs`
- **Priority**: 🔴 Critical
- **Effort**: 1 day
- **Description**: Circular buffer of deleted text for yank/yank-pop
- **Implementation**:
  - `VecDeque<String>` with max 10 entries
  - Track last yank position for yank-pop cycling
- **Keybinds**:
  - `Ctrl+K`: Kill to end of line (add to ring)
  - `Ctrl+U`: Kill to start of line (add to ring)
  - `Ctrl+W`: Kill word backward (add to ring)
  - `Ctrl+Y`: Yank (paste from ring)
  - `Alt+Y`: Yank pop (cycle ring after yank)
- **References**:
  - Pi: `input.ts` kill-ring implementation

### 1.3 Prompt History ✅ DONE (1dc990f)
- **File**: `crates/kn9t-tui/src/prompt_history.rs`
- **Priority**: 🔴 Critical
- **Effort**: 1 day
- **Description**: Navigate previous prompts with Up/Down
- **Implementation**:
  - `Vec<String>` persisted to `~/.kn9t/prompt_history.json`
  - Max 500 entries
  - Prefix search: typing then Up filters to matching
  - Stash current input when navigating
- **Keybinds**:
  - `Up` (when at line 1): Previous prompt
  - `Down` (when at last line): Next prompt
  - `Ctrl+R`: Reverse search (optional)
- **References**:
  - OpenCode: frecency-based history

### 1.4 Prompt Stash ✅ DONE (2026-08-29)
- **File**: `crates/kn9t-tui/src/prompt_stash.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Save/restore prompt state
- **Implementation**:
  - Single slot stash (text + cursor)
  - `/stash` and `/unstash` commands
  - Swap on unstash if input is non-empty

### 1.5 Word Navigation (CJK/Emoji aware) ✅ DONE (2026-08-29)
- **File**: `crates/kn9t-tui/src/word_segmenter.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Proper word boundaries for CJK, emoji, combining marks
- **Implementation**:
  - Use `unicode-segmentation` crate for grapheme/word boundaries
  - Handle paste markers as atomic units
- **Keybinds**:
  - `Ctrl+Left`: Move word backward
  - `Ctrl+Right`: Move word forward
  - `Ctrl+Backspace`: Delete word backward
  - `Ctrl+Delete`: Delete word forward
- **References**:
  - Pi: `Intl.Segmenter` in JavaScript

---

## Phase 2: Rendering Quality

### 2.1 Syntax Highlighting ✅ DONE (0fa73a0)
- **File**: `crates/kn9t-tui/src/syntax.rs`
- **Priority**: 🔴 Critical
- **Effort**: 3 days
- **Description**: Highlight code blocks with language-aware colors
- **Implementation**:
  - Integrate `syntect` crate (TextMate grammars)
  - Or `tree-sitter-highlight` for better accuracy
  - Cache highlighted output per code block
  - Theme-aware: map syntax scopes to theme colors
- **Languages** (priority):
  - Rust, Python, JavaScript/TypeScript, Go, C/C++
  - JSON, YAML, TOML, Markdown
  - Shell (bash, powershell)
- **References**:
  - OpenCode: Shiki WASM with 100+ languages
  - Pi: callback-based highlighter

### 2.2 CSI 2026 Synchronized Output ✅ DONE (580799f)
- **File**: `crates/kn9t-tui/src/app.rs` (inlined, not separate module)
- **Priority**: 🔴 Critical
- **Effort**: 0.5 days
- **Description**: Flicker-free atomic screen updates
- **Implementation**:
  - Wrap render in `crossterm::terminal::BeginSynchronizedUpdate`
  - End with `EndSynchronizedUpdate`
  - Already supported by crossterm 0.27+
- **Code**:
  ```rust
  use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
  execute!(stdout, BeginSynchronizedUpdate)?;
  // ... render frame ...
  execute!(stdout, EndSynchronizedUpdate)?;
  ```
- **References**:
  - Pi: CSI 2026 for atomic updates

### 2.3 Differential Rendering ✅ SKIPPED (2026-08-29)
- **Status**: CSI 2026 synchronized output (2.2) provides sufficient flicker-free rendering
- **Rationale**: High complexity (3-5 days), ratatui uses immediate mode, CSI 2026 achieves the goal

### 2.4 Thinking Blocks ✅ DONE (1961ffc)
- **File**: `crates/kn9t-tui/src/thinking.rs`
- **Priority**: 🔴 Critical
- **Effort**: 1 day
- **Description**: Collapsible reasoning blocks with muted styling
- **Implementation**:
  - Parse `<thinking>...</thinking>` or `<antThinking>` tags
  - Render collapsed by default: `▶ Thinking... (42 lines)`
  - Expandable with Enter/Space
  - Muted opacity (60%) for expanded content
  - Subtle syntax highlighting
- **Keybinds**:
  - `Enter` or `Space`: Toggle expand/collapse
- **References**:
  - OpenCode: Collapsible with opacity control

### 2.5 LaTeX Math Rendering ✅ DONE (2026-08-29)
- **File**: `crates/kn9t-tui/src/latex.rs`
- **Priority**: 🟡 Low
- **Description**: Unicode approximation for math expressions
- **Implementation**:
  - Greek letters: `\alpha` → `α`, `\beta` → `β`, etc.
  - Operators: `\sum` → `∑`, `\int` → `∫`, `\infty` → `∞`
  - Subscripts/superscripts: `x^2` → `x²`, `x_i` → `xᵢ`
  - Fractions: `\frac{a}{b}` → `a/b`
  - Roots: `\sqrt{x}` → `√x`, `\sqrt[3]{x}` → `³√x`
  - `process_math()` for inline `$...$` and display `$$...$$`

### 2.6 OSC 8 Hyperlinks ✅ DONE (2026-08-29)
- **File**: `crates/kn9t-tui/src/hyperlinks.rs`
- **Priority**: 🟠 Medium
- **Description**: Clickable links in markdown and file paths
- **Implementation**:
  - `hyperlink(url, text)` - wraps text in OSC 8 sequence
  - `file_url(path)` - creates file:// URL from path
  - `file_link(path)` / `file_line_link(path, line)` - file path hyperlinks
  - `linkify_urls(text)` - auto-detect and wrap URLs
  - Terminal support detection (iTerm2, Kitty, WezTerm, Windows Terminal)
  - tmux pass-through sequences
- **Note**: Integration with markdown/render deferred (ratatui spans don't support raw escapes)

---

## Phase 3: Diff & Search

### 3.1 Diff Viewer ✅ DONE (a826a8d, enhanced 2026-08-29)
- **File**: `crates/kn9t-tui/src/diff_viewer.rs`
- **Priority**: 🔴 Critical
- **Effort**: 3-5 days
- **Description**: Side-by-side or unified diff display with mouse support
- **Implementation**:
  - Parse unified diff format
  - Two modes: split (default) and unified
  - File tree on left (for multi-file diffs)
  - Hunk navigation with `[` and `]`
  - Line commenting with format `[file:line] comment`
  - Mouse support: click to select, re-click to comment, wheel to scroll
  - Click on file tree to switch files
  - Cursor auto-scroll when navigating
  - Works on welcome screen
- **UI**:
  ```
  ┌─ Files ──────────┬─ Diff ─────────────────────────┐
  │ ▼ src/           │ @@ -10,5 +10,7 @@              │
  │   ├ app.rs       │ -    old line                  │
  │   └ render.rs ◀  │ +    new line                  │
  │                  │ +    another new               │
  └──────────────────┴────────────────────────────────┘
  j/k: line · n/p: file · ]/[: hunk · c: comment · b: tree · u: split · Esc: close
  ```
- **Keybinds**:
  - `j`/`k`: Move cursor up/down
  - `n`/`p`: Next/previous file
  - `[`/`]`: Previous/next hunk
  - `c`/`Enter`: Add comment on selected line
  - `b`: Toggle file tree sidebar
  - `u`: Toggle unified/split view
  - `Esc`: Close (appends comments to input)
  - Mouse click: Select line (re-click opens comment)
  - Mouse wheel: Scroll
- **References**:
  - OpenCode: Full diff viewer with hunk navigation

### 3.1b Command Palette ✅ DONE (2026-08-29)
- **File**: `crates/kn9t-tui/src/command_palette.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Fuzzy search command launcher (like VSCode Ctrl+P)
- **Implementation**:
  - Fuzzy matching on command names
  - Shows keybindings alongside commands
  - Categories: Navigation, Session, Edit, View, Tools, Settings
  - /palette slash command also opens it
- **Keybinds**:
  - `Ctrl+P`: Open command palette
  - `Up`/`Down`: Navigate
  - `Enter`: Execute selected command
  - `Esc`: Close

### 3.2 Viewport Search ✅ DONE (58f4bdb)
- **File**: `crates/kn9t-tui/src/search.rs`
- **Priority**: 🔴 Critical
- **Effort**: 2 days
- **Description**: Search transcript with highlighting
- **Implementation**:
  - `Ctrl+F` opens search bar
  - Regex support (optional toggle)
  - Highlight all matches
  - Navigate with `Enter`/`Ctrl+G` (next) and `Shift+Enter`/`Ctrl+Shift+G` (prev)
  - Match count display: `3/42`
  - Case-insensitive by default, toggle with `Alt+C`
- **UI**:
  ```
  ┌─ Search: [pattern_here______] ─ 3/42 ─ [.*] [Aa] ─┐
  ```
- **Keybinds**:
  - `Ctrl+F`: Open search
  - `Escape`: Close search
  - `Enter`: Next match
  - `Shift+Enter`: Previous match
  - `Alt+R`: Toggle regex
  - `Alt+C`: Toggle case sensitivity
- **References**:
  - Pi: Ctrl+Shift+F with regex
  - OpenCode: Built-in search

### 3.3 Semantic Prompt Navigation ✅ DONE (2026-08-29)
- **File**: `crates/kn9t-tui/src/app.rs` (jump_to_prev/next_user_message)
- **Priority**: 🟡 Low
- **Description**: Jump between user messages in transcript
- **Implementation**:
  - `Ctrl+Up`: Jump to previous user message
  - `Ctrl+Down`: Jump to next user message
  - Scrolls transcript to show the target message
  - Wraps around when at start/end

---

## Phase 4: Session & Model Management

### 4.1 Session Fork
- **File**: `crates/kn9t-tui/src/session_fork.rs`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Branch conversation from any point
- **Implementation**:
  - Server endpoint: `POST /session/{id}/fork?from_seq={seq}`
  - Creates new session with events up to seq
  - UI: Navigate to message, press `F` to fork
  - Timeline view showing fork points
- **Server Changes**:
  - `kn9t-server/src/routes/session.rs`: Add fork endpoint
  - Copy events from source session up to seq
- **References**:
  - OpenCode: Timeline view with fork capability

### 4.2 Session Rename
- **File**: `crates/kn9t-tui/src/session_manager.rs` (extend)
- **Priority**: 🟠 Medium
- **Effort**: 0.5 days
- **Description**: Rename sessions
- **Implementation**:
  - Server endpoint: `PATCH /session/{id}` with `{"title": "..."}`
  - UI: In session selector, press `R` to rename
  - Inline edit in selector list
- **Keybinds**:
  - `R` in session selector: Rename
  - `Enter`: Confirm
  - `Escape`: Cancel
- **References**:
  - OpenCode: Ctrl+R to rename

### 4.3 Session Quick Slots
- **File**: `crates/kn9t-tui/src/quick_slots.rs`
- **Priority**: 🟡 Low
- **Effort**: 1 day
- **Description**: Hotkeys for favorite sessions
- **Implementation**:
  - Assign sessions to slots 1-9
  - `Alt+1` through `Alt+9` to switch
  - Visual indicator in session list: `[1]`
  - Persist to config
- **Keybinds**:
  - `Alt+1..9`: Switch to slot
  - `Ctrl+Alt+1..9`: Assign current session to slot
- **References**:
  - OpenCode: Quick slots 1-9

### 4.4 Session Pin
- **File**: `crates/kn9t-tui/src/session_manager.rs` (extend)
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Pin sessions to top of list
- **Implementation**:
  - Server field: `pinned: bool`
  - UI: Press `P` to toggle pin
  - Pinned sessions sort to top with `📌` indicator
- **References**:
  - OpenCode: Pinned sessions

### 4.5 Model Cycling ✅ DONE (2026-08-30)
- **File**: `crates/kn9t-tui/src/keybind.rs`, `crates/kn9t-tui/src/app.rs`
- **Priority**: 🟠 Medium
- **Description**: Quick switch without opening picker
- **Implementation**:
  - `F2`: Next model (cycles through all available)
  - `Shift+F2`: Previous model
  - Works on both welcome and chat screens
  - Persists selection to server preferences
- **Keybinds**:
  - `F2`: Next model
  - `Shift+F2`: Previous model

### 4.6 Model Favorites
- **File**: `crates/kn9t-tui/src/model_favorites.rs`
- **Priority**: 🟡 Low
- **Effort**: 1 day
- **Description**: Mark models as favorites for quick access
- **Implementation**:
  - Persist favorites to `~/.kn9t/favorites.json`
  - UI: Press `F` in model picker to toggle favorite
  - Favorites shown at top with `★` indicator
  - Separate cycling through favorites only
- **Keybinds**:
  - `F` in model picker: Toggle favorite
  - `Ctrl+F2`: Cycle favorites only
- **References**:
  - OpenCode: Favorite models with separate cycling

### 4.7 Frecency Tracking
- **File**: `crates/kn9t-tui/src/frecency.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Smart sorting by frequency + recency
- **Implementation**:
  - Track access counts and timestamps
  - Score = frequency * recency_decay
  - Apply to: models, sessions, slash commands
  - Persist to disk
- **Algorithm**:
  ```rust
  fn score(accesses: &[(Timestamp, u32)]) -> f64 {
      accesses.iter().map(|(ts, count)| {
          let age_hours = now.duration_since(*ts).as_secs_f64() / 3600.0;
          *count as f64 * (0.5_f64).powf(age_hours / 24.0) // half-life 24h
      }).sum()
  }
  ```
- **References**:
  - OpenCode: Frecency for models, sessions

---

## Phase 5: Agents & Subagents

### 5.1 Agent Selection UI
- **File**: `crates/kn9t-tui/src/agent_selector.rs`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Select and switch between agents
- **Implementation**:
  - Fetch agent list from `/agents` endpoint
  - Overlay picker similar to model selector
  - Color-coded agents (from theme palette)
  - Tab/Shift+Tab to cycle
- **UI**:
  ```
  ┌─ Select Agent ────────────────┐
  │ 🔵 architect   Complex tasks  │
  │ 🟢 builder     Routine coding │
  │ 🟡 explorer    Codebase nav   │
  └───────────────────────────────┘
  ```
- **Keybinds**:
  - `Ctrl+A`: Open agent selector
  - `Tab`: Next agent
  - `Shift+Tab`: Previous agent
- **References**:
  - OpenCode: ⌘a agent selection

### 5.2 Subagent Display
- **File**: `crates/kn9t-tui/src/subagent_display.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Show subagent tool calls aggregated
- **Implementation**:
  - Parse Task tool output for subagent calls
  - Display as: `↳ 5 tool calls · 12.3s`
  - Expandable to show individual calls
  - Different styling from main agent tools
- **References**:
  - OpenCode: Subagent tool aggregation

---

## Phase 6: Keybinding System

### 6.1 Leader Key
- **File**: `crates/kn9t-tui/src/leader_key.rs`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Vim-style leader key for extended bindings
- **Implementation**:
  - Default leader: `Ctrl+X` (Emacs style) or `Space` (vim style)
  - Timeout: 2000ms
  - Enables sequences: `<leader>s` for session, `<leader>m` for model
  - Multiplies available keybind space
- **Sequences**:
  - `<leader>s`: Session commands (l=list, n=new, d=delete, r=rename)
  - `<leader>m`: Model commands (l=list, f=favorites)
  - `<leader>t`: Theme commands (l=list, d=dark, n=light)
  - `<leader>1-9`: Quick session slots
- **Config**:
  ```toml
  [keybinds]
  leader = "ctrl+x"  # or "space"
  leader_timeout_ms = 2000
  ```
- **References**:
  - OpenCode: Ctrl+X leader with 2000ms timeout

### 6.2 Which-Key Panel ✅ DONE (e404653)
- **File**: `crates/kn9t-tui/src/which_key.rs`
- **Priority**: 🔴 Critical
- **Effort**: 2 days
- **Description**: Vim-like keybinding help popup
- **Implementation**:
  - Show after leader key timeout (or immediately on `?`)
  - Group bindings by category
  - Navigate with arrows
  - Scrollable if many bindings
  - Context-aware (different bindings in tool mode)
- **UI**:
  ```
  ┌─ Keybindings ─────────────────────────┐
  │ Session                               │
  │   l  List sessions                    │
  │   n  New session                      │
  │   d  Delete session                   │
  │ Model                                 │
  │   l  List models                      │
  │   f  Favorites                        │
  └───────────────────────────────────────┘
  ```
- **Keybinds**:
  - `?` or `Ctrl+Alt+K`: Show which-key
  - `Escape`: Close
  - Arrows: Navigate
- **References**:
  - OpenCode: Vim-like which-key

### 6.3 Keybind Conflict Detection
- **File**: `crates/kn9t-tui/src/keybind_validator.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Warn on conflicting keybinds
- **Implementation**:
  - Check at config load time
  - Log warnings for conflicts
  - Show in which-key with `⚠️` indicator
- **References**:
  - OpenCode: Conflict detection

---

## Phase 7: Themes & Styling

### 7.1 Additional Themes
- **File**: `crates/kn9t-tui/src/themes/`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Port popular themes
- **Themes to Add**:
  - catppuccin (mocha, latte, frappe, macchiato)
  - dracula
  - nord
  - gruvbox (dark, light)
  - tokyo-night
  - one-dark
  - solarized (dark, light)
  - monokai
  - rosé-pine
- **Implementation**:
  - Add theme files in `themes/` directory
  - Load from `~/.kn9t/themes/` for custom themes
  - Theme selector overlay
- **Config**:
  ```toml
  [theme]
  name = "catppuccin-mocha"  # or path to custom theme
  ```
- **References**:
  - OpenCode: 32 built-in themes

### 7.2 System Theme Generation
- **File**: `crates/kn9t-tui/src/theme_detect.rs`
- **Priority**: 🟡 Low
- **Effort**: 1 day
- **Description**: Generate theme from terminal palette
- **Implementation**:
  - Query terminal colors via OSC 4 (indexed) and OSC 10/11 (fg/bg)
  - Build theme from 16 ANSI colors
  - Auto-detect dark/light from background luminance
- **References**:
  - OpenCode: System theme from terminal palette

### 7.3 Theme Selector Overlay
- **File**: `crates/kn9t-tui/src/theme_selector.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Visual theme picker
- **Implementation**:
  - List all themes with preview swatch
  - Live preview on hover/selection
  - Filter by name
  - Group by type (dark/light/system)
- **Keybinds**:
  - `Ctrl+T` or `/theme`: Open theme selector
- **References**:
  - OpenCode: Theme list with preview

---

## Phase 8: Tool Display

### 8.1 Specialized Tool Renderers
- **File**: `crates/kn9t-tui/src/tools/`
- **Priority**: 🟠 Medium
- **Effort**: 3 days
- **Description**: Custom rendering per tool type
- **Tools**:
  - `bash`: Show command, exit code, output with syntax highlight
  - `read`: Show file path, line range, content preview
  - `write`: Show file path, content diff
  - `edit`: Show before/after with inline diff
  - `glob`: Show pattern, file tree result
  - `grep`: Show pattern, matches with context
  - `webfetch`: Show URL, response preview
  - `task`: Show subagent type, status, summary
- **Implementation**:
  - `ToolRenderer` trait
  - Registry mapping tool name to renderer
  - Fallback to generic JSON display
- **References**:
  - OpenCode: 10+ specialized tool renderers

### 8.2 Tool Details Toggle
- **File**: `crates/kn9t-tui/src/tool_details.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Global toggle to show/hide tool details
- **Implementation**:
  - Config: `show_tool_details = true`
  - When false: Show only tool name and status
  - When true: Show full args/output
  - Per-tool override in tool mode
- **Keybinds**:
  - `Ctrl+D`: Toggle tool details globally
- **References**:
  - OpenCode: Tool details visibility

### 8.3 Tool Duration Display
- **File**: `crates/kn9t-tui/src/tool_timing.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Show execution time per tool
- **Implementation**:
  - Track `started_at` and `finished_at` from SSE events
  - Display: `bash (2.3s) ✓`
  - Sum for subagent calls
- **References**:
  - OpenCode: Duration per tool

---

## Phase 9: Images & Media

### 9.1 Inline Image Display
- **File**: `crates/kn9t-tui/src/image_display.rs`
- **Priority**: 🟠 Medium
- **Effort**: 3-5 days
- **Description**: Display images in terminal
- **Implementation**:
  - **Kitty Graphics Protocol** (primary)
    - Base64-encoded PNG chunks
    - Transmit with `\x1b_Ga=T,...\x1b\\`
  - **iTerm2 Inline Images** (fallback)
    - `\x1b]1337;File=...;inline=1:BASE64\x07`
  - **Sixel** (legacy fallback)
  - **Text fallback**: `[Image: 800x600]`
- **Terminal Detection**:
  - Check `TERM_PROGRAM` for kitty, iTerm, WezTerm
  - Query capabilities with `\x1b[?2026$p`
- **References**:
  - Pi: Kitty + iTerm2 with pixel-perfect scaling

### 9.2 Image Dimension Detection
- **File**: `crates/kn9t-tui/src/image_meta.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Read image dimensions from headers
- **Implementation**:
  - Parse PNG/JPEG/GIF/WebP headers
  - No full decode needed
  - Use for proper scaling
- **References**:
  - Pi: Header-based dimension detection

### 9.3 Cell Dimension Query
- **File**: `crates/kn9t-tui/src/cell_size.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Query terminal cell size in pixels
- **Implementation**:
  - Use `\x1b[16t` or `\x1b[14t` CSI sequences
  - Calculate pixel-perfect image scaling
  - Cache result (terminal resize invalidates)
- **References**:
  - Pi: Cell dimension queries

---

## Phase 10: Terminal Integration

### 10.1 Kitty Keyboard Protocol
- **File**: `crates/kn9t-tui/src/kitty_keyboard.rs`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Enhanced keyboard input handling
- **Implementation**:
  - Enable with `\x1b[>1u` (push mode)
  - Disable with `\x1b[<u` (pop mode)
  - Parse enhanced key events
  - Benefits: Distinguish Ctrl+I from Tab, Ctrl+M from Enter
  - Key release events
- **Fallback**: Standard crossterm input
- **References**:
  - Pi: Full Kitty keyboard support
  - OpenCode: Kitty keyboard enabled

### 10.2 Terminal Title
- **File**: `crates/kn9t-tui/src/terminal_title.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Dynamic terminal title
- **Implementation**:
  - Set with `\x1b]2;TITLE\x07`
  - Format: `kn9t - {session_title}`
  - Update on session change
  - Config option to disable
- **Config**:
  ```toml
  [tui]
  terminal_title = true
  ```
- **References**:
  - OpenCode: Dynamic title based on route

### 10.3 Terminal Suspend
- **File**: `crates/kn9t-tui/src/suspend.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Ctrl+Z to suspend (Unix only)
- **Implementation**:
  - Handle SIGTSTP
  - Exit raw mode before suspend
  - Re-enter raw mode on SIGCONT
  - Not available on Windows
- **Keybinds**:
  - `Ctrl+Z`: Suspend (Unix only)
- **References**:
  - OpenCode: Ctrl+Z support

### 10.4 tmux Awareness
- **File**: `crates/kn9t-tui/src/tmux.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Detect and handle tmux passthrough
- **Implementation**:
  - Check `TMUX` environment variable
  - Wrap escape sequences: `\x1bPtmux;....\x1b\\`
  - Handle: OSC 8 hyperlinks, Kitty graphics, OSC 52 clipboard
- **References**:
  - Pi: tmux-aware sequences

### 10.5 Color Scheme Detection
- **File**: `crates/kn9t-tui/src/color_detect.rs`
- **Priority**: 🟡 Low
- **Effort**: 1 day
- **Description**: Auto-detect dark/light mode
- **Implementation**:
  - OSC 11 query for background color
  - Calculate luminance
  - DSR color-scheme preference (if supported)
  - Listen for real-time changes
- **Current**: Uses `COLORFGBG` only
- **References**:
  - OpenCode: Luminance-based detection

---

## Phase 11: Sidebar & Layout

### 11.1 Left Sidebar
- **File**: `crates/kn9t-tui/src/sidebar_left.rs`
- **Priority**: 🟠 Medium
- **Effort**: 3 days
- **Description**: Context panel with multiple tabs
- **Tabs**:
  - **Context**: Attached files, images
  - **MCPs**: Connected MCP servers
  - **TODOs**: Task list from todowrite
  - **Files**: File explorer
- **Implementation**:
  - Collapsible (toggle with `Ctrl+B`)
  - Tab switching
  - Per-tab content rendering
- **UI**:
  ```
  ┌─ Context ─────────┬─ Chat ──────────────┐
  │ Files             │                     │
  │   src/app.rs      │ User: ...           │
  │   README.md       │                     │
  │ Images            │ Assistant: ...      │
  │   screenshot.png  │                     │
  └───────────────────┴─────────────────────┘
  ```
- **Keybinds**:
  - `Ctrl+B`: Toggle sidebar
  - `Ctrl+1..4`: Switch sidebar tab
- **References**:
  - OpenCode: Left sidebar with plugins

### 11.2 Responsive Breakpoints
- **File**: `crates/kn9t-tui/src/layout.rs` (extend)
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Adaptive layout for terminal size
- **Breakpoints**:
  - `<60 cols`: Hide right sidebar, single column
  - `<80 cols`: Hide left sidebar
  - `<100 cols`: Compact status bar
  - `>=120 cols`: Full layout with both sidebars
- **Implementation**:
  - Check terminal size on render
  - Layout enum: `Compact`, `Normal`, `Wide`
- **References**:
  - Pi: Responsive layout

### 11.3 Flex Layout System
- **File**: `crates/kn9t-tui/src/flex_layout.rs`
- **Priority**: 🟡 Low
- **Effort**: 3 days
- **Description**: CSS-like flex layout
- **Implementation**:
  - VStack/HStack components
  - Properties: `basis`, `grow`, `shrink`, `min_size`, `max_size`
  - Simplify current manual layout math
- **References**:
  - Pi: VStack/HStack with flex
  - OpenCode: Via OpenTUI

---

## Phase 12: Notifications & Feedback

### 12.1 Toast Notifications
- **File**: `crates/kn9t-tui/src/toast.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Transient notification messages
- **Implementation**:
  - Display in corner (top-right by default)
  - Auto-dismiss after 3s
  - Variants: info, success, warning, error
  - Queue multiple toasts
- **UI**:
  ```
  ┌────────────────────┐
  │ ✓ Session created  │
  └────────────────────┘
  ```
- **References**:
  - OpenCode: Toast notifications

### 12.2 Sound Notifications
- **File**: `crates/kn9t-tui/src/audio.rs`
- **Priority**: 🟡 Low
- **Effort**: 2 days
- **Description**: Audio feedback for events
- **Implementation**:
  - Use `rodio` crate for audio playback
  - Events: turn complete, error, approval needed
  - Volume control
  - Mute option
- **Config**:
  ```toml
  [audio]
  enabled = true
  volume = 0.5
  sound_pack = "default"
  ```
- **References**:
  - OpenCode: 6 sound packs

### 12.3 Progress Indicators
- **File**: `crates/kn9t-tui/src/progress.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Better progress feedback
- **Implementation**:
  - Progress bar for long operations
  - Estimated time remaining
  - Cancel option
- **Use Cases**:
  - Session loading
  - Large file operations
  - Model switching

---

## Phase 13: External Integration

### 13.1 External Editor
- **File**: `crates/kn9t-tui/src/external_editor.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Open files in external editor
- **Implementation**:
  - Use `$EDITOR` or `$VISUAL` environment variable
  - Support specific editors: VS Code, Zed, vim, emacs
  - Open file at specific line: `code -g file:line`
  - Return to TUI after editor closes
- **Keybinds**:
  - `Ctrl+O`: Open current file in editor
  - `e` in file context: Edit externally
- **References**:
  - OpenCode: Zed, VS Code integration

### 13.2 Clipboard via OSC 52
- **File**: `crates/kn9t-tui/src/clipboard_osc52.rs`
- **Priority**: 🟠 Medium
- **Effort**: 0.5 days
- **Description**: Cross-platform clipboard via terminal
- **Implementation**:
  - Write: `\x1b]52;c;BASE64\x07`
  - Read: `\x1b]52;c;?\x07` (limited support)
  - Works over SSH
  - Fallback to `arboard` for local
- **References**:
  - Pi: OSC 52 clipboard

### 13.3 Debug Console
- **File**: `crates/kn9t-tui/src/debug_console.rs`
- **Priority**: 🟡 Low
- **Effort**: 1 day
- **Description**: Built-in debug/log viewer
- **Implementation**:
  - Overlay showing log messages
  - Filter by level (debug, info, warn, error)
  - Copy log to clipboard
  - Real-time updates
- **Keybinds**:
  - `Ctrl+Shift+D`: Toggle debug console
- **References**:
  - OpenCode: Built-in console

---

## Phase 14: Paste Handling

### 14.1 Paste Markers
- **File**: `crates/kn9t-tui/src/paste_marker.rs`
- **Priority**: 🟠 Medium
- **Effort**: 1 day
- **Description**: Visual markers for large pastes
- **Implementation**:
  - Threshold: >10 lines triggers marker
  - Display: `[paste #1 +50 lines]`
  - Markers are atomic units for cursor movement
  - Expandable on demand
- **UI**:
  ```
  Some text before
  [paste #1 +50 lines] ▶
  Some text after
  ```
- **References**:
  - Pi: Paste markers with +N lines

### 14.2 Paste Summary
- **File**: `crates/kn9t-tui/src/paste_summary.rs`
- **Priority**: 🟡 Low
- **Effort**: 1 day
- **Description**: AI summary of pasted content
- **Implementation**:
  - Optional toggle
  - Call server endpoint for summary
  - Display summary in tooltip
  - Useful for large code pastes
- **Config**:
  ```toml
  [tui]
  paste_summary = false
  ```
- **References**:
  - OpenCode: Paste summarization

---

## Phase 15: IME & Accessibility

### 15.1 IME Support
- **File**: `crates/kn9t-tui/src/ime.rs`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Input Method Editor support for CJK
- **Implementation**:
  - Position hardware cursor at input location
  - Use APC sequences for cursor marker
  - Enable with `PI_HARDWARE_CURSOR=1` or config
  - Critical for Chinese/Japanese/Korean input
- **References**:
  - Pi: Full IME support with hardware cursor

### 15.2 Screen Reader Support
- **File**: `crates/kn9t-tui/src/accessibility.rs`
- **Priority**: 🟡 Low
- **Effort**: 2 days
- **Description**: Basic screen reader compatibility
- **Implementation**:
  - Proper ARIA-like announcements
  - Live regions for dynamic content
  - Focus management
- **Limited**: Terminal screen readers are rare

---

## Phase 16: Plugin System

### 16.1 Plugin Architecture
- **File**: `crates/kn9t-tui/src/plugin/`
- **Priority**: 🟡 Low
- **Effort**: 5+ days
- **Description**: Extensible plugin system
- **Implementation**:
  - Plugin manifest format
  - Slots: sidebar, status bar, overlays
  - Routes: custom screens
  - Commands: slash command extensions
  - API: expose TUI state to plugins
- **References**:
  - OpenCode: Full plugin system with slots/routes/commands

### 16.2 Plugin Slots
- **File**: `crates/kn9t-tui/src/plugin/slots.rs`
- **Priority**: 🟡 Low
- **Effort**: 2 days
- **Description**: Extension points for plugins
- **Slots**:
  - `sidebar:left`
  - `sidebar:right`
  - `status:left`
  - `status:right`
  - `overlay`
- **References**:
  - OpenCode: Plugin slots

---

## Phase 17: Performance

### 17.1 Virtual Scrolling
- **File**: `crates/kn9t-tui/src/virtual_scroll.rs`
- **Priority**: 🟠 Medium
- **Effort**: 2 days
- **Description**: Efficient rendering for long transcripts
- **Implementation**:
  - Only render visible messages
  - Pre-compute message heights
  - Smooth scrolling with overscan
  - Handle resize correctly
- **Current**: Renders all messages (can lag on long sessions)

### 17.2 Lazy Syntax Loading
- **File**: `crates/kn9t-tui/src/syntax_lazy.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Load syntax definitions on demand
- **Implementation**:
  - Start with common languages (rust, python, js)
  - Load others when first encountered
  - Cache loaded syntaxes
- **References**:
  - OpenCode: On-demand language loading

### 17.3 FPS Control
- **File**: `crates/kn9t-tui/src/fps.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Configurable frame rate
- **Implementation**:
  - Default: 60 FPS (16ms)
  - Config option to reduce for low-power
  - Adaptive: reduce when idle
- **Config**:
  ```toml
  [tui]
  target_fps = 60
  ```
- **References**:
  - OpenCode: Configurable FPS

### 17.4 Scroll Acceleration
- **File**: `crates/kn9t-tui/src/scroll_accel.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Smooth scroll with acceleration
- **Implementation**:
  - Track scroll velocity
  - Accelerate on repeated scroll events
  - Decelerate smoothly
- **Config**:
  ```toml
  [tui.scroll]
  acceleration = 1.5
  max_speed = 20
  ```
- **References**:
  - OpenCode: Scroll acceleration

---

## Phase 18: Workspace Support

### 18.1 Workspace Management
- **File**: `crates/kn9t-tui/src/workspace.rs`
- **Priority**: 🟡 Low
- **Effort**: 2 days
- **Description**: Multiple workspace/project support
- **Implementation**:
  - Track current workspace directory
  - Filter sessions by workspace
  - Workspace selector
  - Persist recent workspaces
- **References**:
  - OpenCode: Workspace support with worktrees

### 18.2 Directory Filter
- **File**: `crates/kn9t-tui/src/dir_filter.rs`
- **Priority**: 🟡 Low
- **Effort**: 0.5 days
- **Description**: Filter sessions by directory
- **Implementation**:
  - Extract working directory from session
  - Filter in session list
  - Visual indicator of current filter
- **References**:
  - OpenCode: Session directory filtering

---

## Summary Table

| Phase | Items | Priority | Total Effort |
|-------|-------|----------|--------------|
| 1. Input UX | 5 | 🔴 Critical | 5.5 days |
| 2. Rendering | 6 | 🔴 Critical | 13+ days |
| 3. Diff & Search | 3 | 🔴 Critical | 6 days |
| 4. Session/Model | 7 | 🟠 Medium | 6 days |
| 5. Agents | 2 | 🟠 Medium | 3 days |
| 6. Keybindings | 3 | 🟠 Medium | 4.5 days |
| 7. Themes | 3 | 🟠 Medium | 4 days |
| 8. Tool Display | 3 | 🟠 Medium | 4 days |
| 9. Images | 3 | 🟠 Medium | 4-6 days |
| 10. Terminal | 5 | 🟠 Medium | 5 days |
| 11. Sidebar | 3 | 🟠 Medium | 7 days |
| 12. Notifications | 3 | 🟠 Medium | 3.5 days |
| 13. External | 3 | 🟠 Medium | 2.5 days |
| 14. Paste | 2 | 🟠 Medium | 2 days |
| 15. IME | 2 | 🟠 Medium | 4 days |
| 16. Plugins | 2 | 🟡 Low | 7+ days |
| 17. Performance | 4 | 🟠 Medium | 3.5 days |
| 18. Workspace | 2 | 🟡 Low | 2.5 days |
| **TOTAL** | **61 items** | | **~87 days** |

---

## Implementation Order (Recommended)

### Sprint 1: Input UX (Week 1-2)
1. 1.1 Undo/Redo
2. 1.2 Kill Ring
3. 1.3 Prompt History
4. 2.2 CSI 2026 Sync (quick win)

### Sprint 2: Rendering (Week 3-4)
5. 2.1 Syntax Highlighting
6. 2.4 Thinking Blocks
7. 6.2 Which-Key Panel
8. 3.2 Viewport Search

### Sprint 3: Diff & Tools (Week 5-6)
9. 3.1 Diff Viewer
10. 8.1 Specialized Tool Renderers
11. 4.5 Model Cycling
12. 4.7 Frecency

### Sprint 4: Themes & Polish (Week 7-8)
13. 7.1 Additional Themes
14. 7.3 Theme Selector
15. 12.1 Toast Notifications
16. 10.2 Terminal Title

### Sprint 5: Terminal Integration (Week 9-10)
17. 10.1 Kitty Keyboard
18. 10.4 tmux Awareness
19. 2.6 OSC 8 Hyperlinks
20. 9.1 Inline Image Display

### Sprint 6: Advanced (Week 11-12)
21. 6.1 Leader Key
22. 11.1 Left Sidebar
23. 5.1 Agent Selection
24. 4.1 Session Fork

### Future (Backlog)
- 2.3 Differential Rendering
- 2.5 LaTeX Math
- 15.1 IME Support
- 16.x Plugin System
- 12.2 Sound Notifications

---

## Files to Create

```
crates/kn9t-tui/src/
├── input_history.rs      # 1.1 Undo/Redo
├── kill_ring.rs          # 1.2 Kill Ring
├── prompt_history.rs     # 1.3 Prompt History
├── prompt_stash.rs       # 1.4 Prompt Stash
├── word_segmenter.rs     # 1.5 Word Navigation
├── syntax.rs             # 2.1 Syntax Highlighting
├── render_sync.rs        # 2.2 CSI 2026
├── diff_render.rs        # 2.3 Differential Rendering
├── thinking.rs           # 2.4 Thinking Blocks
├── latex.rs              # 2.5 LaTeX
├── hyperlinks.rs         # 2.6 OSC 8
├── diff_viewer.rs        # 3.1 Diff Viewer
├── search.rs             # 3.2 Viewport Search
├── semantic_nav.rs       # 3.3 Semantic Navigation
├── session_fork.rs       # 4.1 Session Fork
├── quick_slots.rs        # 4.3 Quick Slots
├── model_favorites.rs    # 4.6 Model Favorites
├── frecency.rs           # 4.7 Frecency
├── agent_selector.rs     # 5.1 Agent Selection
├── subagent_display.rs   # 5.2 Subagent Display
├── leader_key.rs         # 6.1 Leader Key
├── which_key.rs          # 6.2 Which-Key
├── keybind_validator.rs  # 6.3 Conflict Detection
├── themes/               # 7.1 Theme files
│   ├── catppuccin.rs
│   ├── dracula.rs
│   ├── nord.rs
│   └── ...
├── theme_detect.rs       # 7.2 System Theme
├── theme_selector.rs     # 7.3 Theme Selector
├── tools/                # 8.1 Tool Renderers
│   ├── mod.rs
│   ├── bash.rs
│   ├── read.rs
│   ├── write.rs
│   ├── edit.rs
│   └── ...
├── tool_details.rs       # 8.2 Tool Details Toggle
├── tool_timing.rs        # 8.3 Tool Duration
├── image_display.rs      # 9.1 Inline Images
├── image_meta.rs         # 9.2 Image Dimensions
├── cell_size.rs          # 9.3 Cell Size Query
├── kitty_keyboard.rs     # 10.1 Kitty Keyboard
├── terminal_title.rs     # 10.2 Terminal Title
├── suspend.rs            # 10.3 Terminal Suspend
├── tmux.rs               # 10.4 tmux Awareness
├── color_detect.rs       # 10.5 Color Detection
├── sidebar_left.rs       # 11.1 Left Sidebar
├── flex_layout.rs        # 11.3 Flex Layout
├── toast.rs              # 12.1 Toast Notifications
├── audio.rs              # 12.2 Sound Notifications
├── progress.rs           # 12.3 Progress Indicators
├── external_editor.rs    # 13.1 External Editor
├── clipboard_osc52.rs    # 13.2 OSC 52 Clipboard
├── debug_console.rs      # 13.3 Debug Console
├── paste_marker.rs       # 14.1 Paste Markers
├── paste_summary.rs      # 14.2 Paste Summary
├── ime.rs                # 15.1 IME Support
├── accessibility.rs      # 15.2 Screen Reader
├── plugin/               # 16.x Plugin System
│   ├── mod.rs
│   └── slots.rs
├── virtual_scroll.rs     # 17.1 Virtual Scrolling
├── syntax_lazy.rs        # 17.2 Lazy Syntax
├── fps.rs                # 17.3 FPS Control
├── scroll_accel.rs       # 17.4 Scroll Acceleration
├── workspace.rs          # 18.1 Workspace Management
└── dir_filter.rs         # 18.2 Directory Filter
```

---

## Dependencies to Add

```toml
# Cargo.toml additions

# Syntax highlighting (pick one)
syntect = "5.0"           # TextMate grammars
# OR
tree-sitter = "0.20"      # Better accuracy
tree-sitter-highlight = "0.20"

# Unicode handling
unicode-segmentation = "1.10"  # Word/grapheme boundaries

# Audio (optional)
rodio = "0.17"            # Sound playback

# Image (optional)
image = "0.24"            # Image decoding for inline display
```

---

## References

- **Pi TUI**: `C:\_ddm\projects\Agents\Pi\packages\tui\`
- **OpenCode TUI**: `C:\_ddm\projects\opencode\packages\tui\`
- **kn9t TUI**: `C:\_ddm\projects\Agents\kn9t\crates\kn9t-tui\`

---

*Generated: 2026-08-29*
*Total improvements: 61*
*Estimated effort: ~87 days*

