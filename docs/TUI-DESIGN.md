# kn9t TUI Design

> Grill session output — 2026-08-27

## 1. Architecture

### 1.1 Framework

- **ratatui** + **crossterm** (Rust-native, spec-mandated)
- No async (GI-5 compliant)
- Pure event-driven, zero polling

### 1.2 Event Loop

```
┌─────────────────────────────────────────────────────────────┐
│                      Main Thread                            │
│                                                             │
│   ┌─────────────┐     ┌─────────────┐     ┌─────────────┐   │
│   │  Keyboard   │     │    SSE      │     │   Timer     │   │
│   │  (crossterm)│     │  (network)  │     │ (streaming) │   │
│   └──────┬──────┘     └──────┬──────┘     └──────┬──────┘   │
│          │                   │                   │          │
│          └───────────────────┼───────────────────┘          │
│                              ▼                              │
│                    ┌─────────────────┐                      │
│                    │  Unified Event  │                      │
│                    │     Channel     │                      │
│                    └────────┬────────┘                      │
│                             │                               │
│                             ▼                               │
│                    ┌─────────────────┐                      │
│                    │   Event Loop    │ ◄── blocks on recv() │
│                    └────────┬────────┘                      │
│                             │                               │
│                             ▼                               │
│                    ┌─────────────────┐                      │
│                    │  Redraw ONLY    │                      │
│                    │  when needed    │                      │
│                    └─────────────────┘                      │
└─────────────────────────────────────────────────────────────┘
```

**Principles:**
- Block on `recv()` — zero CPU when idle
- Redraw only on state change
- Timer thread only active during streaming (for spinner)

### 1.3 Unified Event Enum

```rust
enum TuiEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Sse(SseEvent),
    Tick,  // spinner animation, only during streaming
}
```

---

## 2. Layout

### 2.1 Main Layout

```
┌───────────┬─────────────────────────────────────────────┬───────────────┐
│ SESSIONS  │              TRANSCRIPT                     │ MODEL         │
│           │                                             │ claude  $0.04 │
│ ⠋ fix bug │  user: fix the bug                          │ 2k/800        │
│   refactor│                                             ├───────────────┤
│   tests   │  assistant: Looking at the code...          │ TOOLS    [±]  │
│           │                                             │ ☑ bash        │
│           │  ▶ read [main.rs]                ✓          │ ☑ read        │
│           │  ▶ edit [main.rs +3/-1]          ✓          │ ☐ webfetch    │
│           │                                             ├───────────────┤
│           │  assistant: Fixed.                          │ GIT      [±]  │
│           │                                             │ main +3 -1    │
│           │  ⠋ thinking...                              │ ▶ main.rs     │
│           │                                             │ ▶ lib.rs      │
│ [hover to │                                             │               │
│  expand]  │                                             │               │
├───────────┼─────────────────────────────────────────────┤               │
│           │ > _                                         │               │
│           ├─────────────────────────────────────────────┤               │
│           │ [abc123] claude | $0.04 | ⠋ thinking | ?    │               │
└───────────┴─────────────────────────────────────────────┴───────────────┘
```

### 2.2 Layout Rules

| Area | Width | Collapsible |
|------|-------|-------------|
| Left sidebar | 15 cols (expanded), 3 cols (collapsed) | ✓ hover/keybind |
| Right sidebar | 20 cols (expanded), 3 cols (collapsed) | ✓ hover/keybind |
| Transcript | remaining | always visible |
| Input box | full width of transcript | always visible |
| Status bar | full width of transcript | always visible |
| Min terminal | 60 cols (hide sidebars below) | — |

### 2.3 Sidebar Configuration

```toml
[tui]
left_sidebar = true   # false to disable entirely
right_sidebar = true  # false to disable entirely
```

---

## 3. Left Sidebar — Sessions

### 3.1 Content

Minimal: status spinner + session name per row.

```
┌─────────────┐
│ ⠋ fix bug   │  ← spinner = running
│   refactor  │  ← no spinner = idle
│   add tests │
│   deploy    │
└─────────────┘
```

### 3.2 Behavior

| Aspect | Decision |
|--------|----------|
| Expand trigger | Mouse hover OR keybind |
| Collapse trigger | Mouse leave OR keybind |
| Click session | Instant switch (no confirmation) |
| Running turn | Continues in background on switch |
| Collapsed indicator | Thin edge visible (3 cols) |

---

## 4. Right Sidebar — Context Panel

### 4.1 Sections

```
┌─────────────────────────────┐
│ MODEL                       │
│ claude-3.5-sonnet    $0.04  │
│ 2.1k in / 800 out           │
├─────────────────────────────┤
│ TOOLS/PLUGINS         [±]   │
│ ☑ bash                      │
│ ☑ read                      │
│ ☑ write                     │
│ ☐ webfetch (disabled)       │
│ ☑ mcp:github                │
├─────────────────────────────┤
│ GIT                   [±]   │
│ branch: main                │
│ +3 -1 uncommitted           │
│                             │
│ ▶ src/main.rs        +2 -0  │
│ ▶ src/lib.rs         +1 -1  │
│                             │
│ Recent commits:             │
│ • abc123 fix: typo          │
└─────────────────────────────┘
```

### 4.2 Tools/Plugins Toggle

| Action | Result |
|--------|--------|
| Click checkbox | Enable/disable tool live |
| Keybind on selected | Toggle enable/disable |
| Effect | Immediate — no restart needed |

### 4.3 Git Section

| Feature | Description |
|---------|-------------|
| Branch name | Current branch |
| Changes count | `+N -M uncommitted` |
| Changed files | List with diff stats |
| Click file | Opens diff viewer overlay |
| Recent commits | Last 3-5 commits (truncated) |

### 4.4 Plugin Sidebar API

Plugins expose structured data, TUI renders with standard components:

```rust
enum SidebarWidget {
    Section {
        title: String,
        collapsed: bool,
        content: Vec<SidebarWidget>,
    },
    KeyValue {
        items: Vec<(String, String)>,
    },
    List {
        items: Vec<ListItem>,
        selectable: bool,
    },
    Toggle {
        label: String,
        enabled: bool,
        on_toggle: String,
    },
    Tree {
        root: TreeNode,
    },
    Button {
        label: String,
        action: String,
    },
}
```

No custom rendering in v1 — plugins return data, TUI renders.

---

## 5. Transcript Area

### 5.1 Message Display

```
user: fix the bug in main.rs

assistant: Looking at the code...

▶ read [main.rs]                           ✓
▶ edit [main.rs +3/-1]                     ✓
   ┌────────────────────────────────────┐
   │ - old line                         │  ← expanded on click
   │ + new line                         │
   └────────────────────────────────────┘

assistant: Fixed. Run tests to verify.
```

### 5.2 Tool Cards

| State | Display |
|-------|---------|
| Running | `▶ tool [args]` + spinner |
| Completed | `▶ tool [args] ✓` collapsed |
| Error | `▶ tool [args] ✗` with red indicator |
| Expanded | Shows full input/output |

**Lazy loading:** Tool data fetched only on expand.

### 5.3 Scroll Behavior

| Action | Key | Behavior |
|--------|-----|----------|
| Auto-scroll | (automatic) | Follows stream; breaks if user scrolls up |
| Jump to end | `End` or `G` | Snap to bottom, re-enable auto-scroll |
| Jump to start | `Home` or `gg` | Snap to top |
| Page up/down | `PgUp`/`PgDn` | Scroll by screen height |
| Line up/down | `k`/`j` | Scroll by line |
| Prev user msg | `[u` | Jump to previous user message |
| Next user msg | `]u` | Jump to next user message |
| Prev assistant msg | `[a` | Jump to previous assistant message |
| Next assistant msg | `]a` | Jump to next assistant message |

**"↓ Jump to end"** button visible in bottom-right when scrolled up during streaming.

### 5.4 Virtual Scrolling / Lazy Load

| Aspect | Decision |
|--------|----------|
| Initial load | Last 50 messages |
| Scroll to top | "Load earlier" button appears |
| Load chunk | Fetch previous 50, prepend |
| Tool data | Lazy load on card expand |

---

## 6. Input Box

### 6.1 Key Bindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Shift-Enter` | Newline |
| `Ctrl-Enter` | Newline (fallback) |
| `Alt-Enter` | Newline (fallback) |
| `Ctrl-E` | Open `$EDITOR` (escape hatch) |

### 6.2 File Mentions

Syntax: `@path/to/file` or `@directory/`

| Input | Behavior |
|-------|----------|
| `@src/main.rs` | Embed file content |
| `@src/` | Embed directory listing (tree, not contents) |

Autocomplete triggers on `@`:

```
┌─────────────────────────────────┐
│ 📁 src/                         │
│   main.rs                       │
│   lib.rs                        │
│   utils/                        │
└─────────────────────────────────┘
```

### 6.3 Image Paste

| Aspect | Decision |
|--------|----------|
| Display | `[Image: 1.2MB PNG 1920x1080]` placeholder |
| Storage | Blob store (SHA-256) |
| Send to model | Base64 encoded |
| Render in terminal | Never |

---

## 7. Status Bar

Bottom of transcript area:

```
[abc123] claude-3.5 | $0.04 | 2k/800 | ⠋ thinking... | ?=help
```

| Element | Description |
|---------|-------------|
| Session ID | Truncated |
| Model | Current model name |
| Cost | Session cost |
| Tokens | `in / out` |
| Status | Spinner + phrase when streaming |
| Help hint | `?=help` |

### 7.1 Streaming Indicator

Animated braille spinner + rotating fun phrases:

```
⠋ thinking...
⠙ pondering...
⠹ cooking...
⠸ summoning bytes...
```

Configurable:

```toml
[tui.streaming]
spinner = "braille"
phrases = [
  "thinking...",
  "pondering...",
  "summoning bytes...",
]
phrase_interval_ms = 2000
```

---

## 8. Overlays

### 8.1 Approval Prompt (Blocking)

Full-screen modal — user cannot type until resolved.

```
╔═══════════════════════════════════════════════════════════════╗
║  APPROVAL REQUIRED                                            ║
║                                                               ║
║  Tool:  bash                                                  ║
║  Args:  rm -rf /tmp/build                                     ║
║                                                               ║
║  [ Allow ]   [ Always ]   [ Reject ]         (timeout: 30s)   ║
╚═══════════════════════════════════════════════════════════════╝
```

For edits, shows diff preview in the overlay.

| Key | Action |
|-----|--------|
| `←`/`→` | Navigate options |
| `Enter` | Confirm selection |
| `Esc` | Reject (default) |
| `y` | Allow |
| `n` | Reject |
| `a` | Always |

### 8.2 Diff Viewer (Side-by-Side)

Triggered from Git sidebar → click file.

```
╔══════════════════════════════ src/main.rs ══════════════════════════════════╗
║  OLD                                │  NEW                                  ║
╠─────────────────────────────────────┼───────────────────────────────────────╣
║   8 │ fn helper() {                 │   8 │ fn helper() {                   ║
║   9 │     // todo                   │   9 │     // todo            💬       ║
║  10 │     let x = 5;                │  10 │     let x = 10;        💬       ║
║  11 │ }                             │  11 │     validate(x);       +        ║
║     │                               │  12 │ }                               ║
╠═════════════════════════════════════════════════════════════════════════════╣
║  COMMENTS (click any line to add)                                           ║
║  └─ line 9: this todo should be removed                                     ║
║  └─ line 10: why change from 5 to 10?                                       ║
║                                                                 [Close]     ║
╚═════════════════════════════════════════════════════════════════════════════╝
```

| Action | Result |
|--------|--------|
| Click any line | Add comment for that line |
| Close | Append comments to input box |
| Format | `[file:line] comment` per line |

User can edit/add more text before sending.

### 8.3 Help Overlay

Full-screen, shows all keybindings (user's actual config):

```
╔═══════════════════════════════ HELP ════════════════════════════════════════╗
║                                                                             ║
║  NAVIGATION              SESSIONS              ACTIONS                      ║
║  j/k      scroll         <leader>s  list       Enter    send                ║
║  gg/G     top/bottom     <leader>n  new        Ctrl-C   abort               ║
║  [u/]u    prev/next      1-9        quick sw   Ctrl-Q   quit                ║
║           user msg                                                          ║
║  [a/]a    prev/next      SIDEBARS              TOOLS                        ║
║           asst msg       [          left       Space    expand card         ║
║                          ]          right      Enter    toggle              ║
║                                                                             ║
║  Press ? or Esc to close                                   kn9t v0.1.0      ║
╚═════════════════════════════════════════════════════════════════════════════╝
```

### 8.4 Session List Dialog

Alternative to sidebar (or when sidebar disabled):

```
╔═══════════════════════ SESSIONS ════════════════════════════╗
║                                                             ║
║  ⠋ fix bug in main.rs                          2 min ago   ║
║    refactor authentication                     1 hour ago  ║
║    add unit tests                              yesterday   ║
║    deploy to production                        2 days ago  ║
║                                                             ║
║  [n] New session    [d] Delete    [Enter] Switch    [Esc]   ║
╚═════════════════════════════════════════════════════════════╝
```

---

## 9. Keybindings

### 9.1 Default Bindings (Vim-style)

**Navigation:**
| Key | Action |
|-----|--------|
| `j` / `k` | Scroll down/up |
| `gg` | Jump to top |
| `G` | Jump to bottom |
| `Ctrl-D` / `Ctrl-U` | Half page down/up |
| `PgDn` / `PgUp` | Full page down/up |
| `[u` / `]u` | Prev/next user message |
| `[a` / `]a` | Prev/next assistant message |

**Sessions:**
| Key | Action |
|-----|--------|
| `<leader>s` | Session list |
| `<leader>n` | New session |
| `1`-`9` | Quick switch to session N |

**Sidebars:**
| Key | Action |
|-----|--------|
| `[` | Toggle left sidebar |
| `]` | Toggle right sidebar |

**Actions:**
| Key | Action |
|-----|--------|
| `Enter` | Send message (in input) |
| `Ctrl-C` | Abort running turn |
| `Ctrl-Q` | Quit |
| `?` | Help overlay |
| `Esc` | Close overlay / cancel |

### 9.2 Configuration

```toml
[keybinds]
leader = "space"

# Override any binding
scroll_up = "k"
scroll_down = "j"
jump_top = "gg"
jump_bottom = "G"
prev_user_msg = "[u"
next_user_msg = "]u"
toggle_left_sidebar = "<leader>e"
toggle_right_sidebar = "<leader>r"
session_list = "<leader>s"
new_session = "<leader>n"
abort_turn = "<C-c>"
quit = "<C-q>"
help = "?"
```

User bindings override defaults.

---

## 10. Mouse Support

Full mouse support required.

### 10.1 Actions

| Target | Click | Hover | Drag |
|--------|-------|-------|------|
| Left sidebar edge | — | Expand | — |
| Right sidebar edge | — | Expand | — |
| Session row | Switch | Highlight | — |
| Tool card | Expand/collapse | Highlight | — |
| Toggle checkbox | Toggle | Highlight | — |
| Git file | Open diff | Highlight | — |
| Transcript | — | — | Select text |
| Scroll | — | — | Scroll |

### 10.2 Selection & Copy

| Aspect | Decision |
|--------|----------|
| Selection | Mouse drag (terminal native) |
| Copy | Terminal's native shortcut |
| Our job | Don't interfere with selection |

---

## 11. Theming

### 11.1 Mode Detection

Auto-detect terminal background (light/dark) via `\e]11;?\a` query.

### 11.2 Configuration

```toml
[theme]
mode = "auto"  # "auto" | "light" | "dark"

[theme.colors]
background = "#1e1e2e"
foreground = "#cdd6f4"
primary = "#89b4fa"
secondary = "#a6adc8"
error = "#f38ba8"
warning = "#fab387"
success = "#a6e3a1"
muted = "#6c7086"
user = "#89dceb"
assistant = "#cba6f7"
tool = "#f9e2af"

[theme.syntax]
keyword = "#cba6f7"
string = "#a6e3a1"
comment = "#6c7086"
diff_add = "#a6e3a1"
diff_remove = "#f38ba8"
```

---

## 12. Error Handling

### 12.1 Errors as Messages

Errors are first-class, persisted in DB alongside messages.

```
┌─ ERROR ─────────────────────────────────────────────────────┐
│ ⚠ API Error                                                 │
│                                                             │
│ Rate limit exceeded. Retry in 30s.                          │
│                                                             │
│ ▶ Details (click to expand)                                 │
└─────────────────────────────────────────────────────────────┘
```

### 12.2 Error Types

| Type | Header |
|------|--------|
| Provider error | `API Error` |
| Connection failed | `Network Error` |
| Tool failed | `Tool Error` |
| Turn timeout | `Timeout` |
| User cancelled | `Aborted` |

All copyable, all reloadable on session restore.

---

## 13. Confirmations

Minimal — only destructive actions:

| Action | Confirm? |
|--------|----------|
| Quit while turn running | ✓ |
| Delete session | ✓ |
| Clear conversation | ✓ |
| Abort turn | ✗ |
| Switch session while turn running | ✗ |
| Send long message | ✗ |

---

## 14. Git Integration

### 14.1 Features (All v1)

| Feature | Location |
|---------|----------|
| Branch name | Right sidebar |
| Uncommitted changes count | Right sidebar |
| Changed files list | Right sidebar |
| Click file → diff viewer | Overlay |
| Comment on any line | Diff viewer |
| Commit | Via tool/plugin |
| Git graph | Plugin |
| Switch branches | Via tool/plugin |

### 14.2 Diff Viewer Comments

Comments append to user input on close:

```
[src/main.rs:9] this todo should be removed
[src/main.rs:10] why change from 5 to 10?
[src/main.rs:11] needs error handling
```

User can add more context before sending.

---

## 15. Summary of Decisions

| Topic | Decision |
|-------|----------|
| Framework | ratatui + crossterm |
| Event model | Pure event-driven, block on recv() |
| Polling | Never |
| Layout | Left sidebar (sessions) + transcript + right sidebar (context) |
| Sidebars | Configurable, hover-to-expand |
| Mouse | Full support required |
| Input | `Enter` = send, `Shift/Ctrl/Alt-Enter` = newline |
| Approval | Blocking overlay |
| Scroll | Auto-scroll with escape, jump-to-end button |
| Message navigation | `[u`/`]u`, `[a`/`]a` |
| Session switch | Instant, no confirmation |
| Virtual scroll | Last 50 messages, "load earlier" button |
| Tool data | Lazy load on expand |
| Keybinds | Vim-style default, fully customizable |
| Theming | Auto light/dark + user CSS-like config |
| Status bar | Bottom |
| Spinner | Animated braille + fun phrases |
| Errors | Inline cards, persisted in DB |
| Clipboard | Terminal native (we don't interfere) |
| Images | Placeholder only, send base64 to model |
| File mentions | `@path` syntax with autocomplete |
| Diff viewer | Side-by-side, click anywhere to comment |
| Git | Full integration via sidebar + plugins |

---

## Appendix A: Example Config

```toml
[tui]
left_sidebar = true
right_sidebar = true

[tui.streaming]
spinner = "braille"
phrases = [
  "thinking...",
  "pondering...",
  "cooking...",
  "summoning bytes...",
  "consulting the void...",
]
phrase_interval_ms = 2000

[theme]
mode = "auto"

[theme.colors]
primary = "#89b4fa"
error = "#f38ba8"

[keybinds]
leader = "space"
scroll_up = "k"
scroll_down = "j"
session_list = "<leader>s"
new_session = "<leader>n"
quit = "<C-q>"
```

---

## Appendix B: File Structure

```
crates/kn9t-tui/
├── src/
│   ├── lib.rs
│   ├── app.rs           # main app state
│   ├── event.rs         # TuiEvent enum, event loop
│   ├── ui/
│   │   ├── mod.rs
│   │   ├── layout.rs    # 3-column layout
│   │   ├── transcript.rs
│   │   ├── input.rs
│   │   ├── status_bar.rs
│   │   ├── sidebar_left.rs
│   │   ├── sidebar_right.rs
│   │   └── overlay/
│   │       ├── mod.rs
│   │       ├── approval.rs
│   │       ├── diff_viewer.rs
│   │       ├── help.rs
│   │       └── session_list.rs
│   ├── keybind.rs       # keybind parsing, matching
│   ├── theme.rs         # color schemes
│   ├── config.rs        # TUI config parsing
│   └── widgets/
│       ├── mod.rs
│       ├── tool_card.rs
│       ├── message.rs
│       ├── spinner.rs
│       └── sidebar_plugin.rs
└── Cargo.toml
```
