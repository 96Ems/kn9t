//! Keybind system — R-TUI-160.
//!
//! All navigation uses a modifier key (Ctrl by default, configurable).
//! This ensures all letters are available for typing in the input.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Actions that can be bound to keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    // Core
    Quit,  // Ctrl+C or Ctrl+Q
    Abort, // Escape
    Help,  // Ctrl+P (command palette)
    Send,  // Enter (steers if streaming, prompts if idle)
    Queue, // Shift+Enter: queue message for post-idle send (unlike steer)
    Paste, // Ctrl+V: paste from system clipboard

    // Scrolling
    ScrollUp,     // Ctrl+Up or PageUp
    ScrollDown,   // Ctrl+Down or PageDown
    ScrollTop,    // Ctrl+Home
    ScrollBottom, // Ctrl+End

    // Message navigation
    PrevMessage, // Ctrl+K
    NextMessage, // Ctrl+J

    // Session picker (Ctrl+B). Sidebars are defined in tui.lua, so there is no
    // built-in sidebar action: a config toggles its own panel in Lua.
    SessionPicker,

    // Sessions
    NewSession, // Ctrl+N

    // Tool mode
    ToolMode, // Ctrl+T: enter/exit tool mode

    // Undo/Redo
    Undo, // Ctrl+Z: undo input change
    Redo, // Ctrl+Y or Ctrl+Shift+Z: redo input change

    // Kill ring (Emacs-style)
    KillToEnd,   // Ctrl+K: kill to end of line
    KillToStart, // Ctrl+U: kill to start of line
    KillWord,    // Ctrl+W: kill word backward
    Yank,        // Ctrl+Y: yank (paste from kill ring)
    YankPop,     // Alt+Y: cycle through kill ring after yank

    // Thinking blocks
    ToggleThinking, // Ctrl+E: toggle thinking block collapse

    // Search
    OpenSearch,  // Ctrl+F: open search bar
    CloseSearch, // Escape: close search bar (in search mode)
    NextMatch,   // Enter: next match (in search mode)
    PrevMatch,   // Shift+Enter: previous match (in search mode)
    ToggleRegex, // Alt+R: toggle regex mode (in search mode)
    ToggleCase,  // Alt+C: toggle case sensitivity (in search mode)

    // Word navigation
    WordLeft,        // Ctrl+Left: move cursor to previous word
    WordRight,       // Ctrl+Right: move cursor to next word
    DeleteWordLeft,  // Ctrl+Backspace: delete word backward
    DeleteWordRight, // Ctrl+Delete: delete word forward

    // Semantic navigation (jump between user/assistant messages)
    PrevUserMessage, // Ctrl+Up: jump to previous user message
    NextUserMessage, // Ctrl+Down: jump to next user message

    // Model cycling (quick switch without picker)
    CycleModelNext, // F2: next model
    CycleModelPrev, // Shift+F2: previous model

    // Diff review is no longer a native view: it ships as `kn9t-git-integration`,
    // which owns its own parsing, rendering and keys (`kn9t.on_key`). There is
    // deliberately no `OpenDiff` action — a plugin panel is reached by focusing
    // it (`focus_plugin`, or a click), not by a host action that opens a
    // host-owned widget.

    // Overlays reachable from Lua without going through the palette.
    OpenModels,
    OpenTools,
    OpenPalette,
    RefreshTools,

    // Unused for now
    PrevUser,
    NextUser,
    PrevAssistant,
    NextAssistant,
    /// Switch to a session by id. The id itself is not carried on the enum
    /// (which stays `Copy`, matching every other variant here); it travels
    /// alongside as `Option<String>` — see `keymap::drain_pending_actions`
    /// and `App::execute_action`'s second parameter.
    SwitchSession,
    /// Give keyboard focus to a plugin view, or clear focus when the argument
    /// is absent/empty. Carries the plugin name alongside, same as
    /// `SwitchSession` — see `App::execute_action`'s second parameter.
    ///
    /// This is what lets a config bind a key to reach a plugin panel; clicking
    /// the panel does the same thing.
    FocusPlugin,
    ExpandCard,
}

/// Keybind matcher.
pub struct Keybinds {
    bindings: HashMap<KeyPattern, Action>,
}

/// Helper to create KeyPattern.
fn kp(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> KeyPattern {
    KeyPattern {
        code,
        ctrl,
        alt,
        shift,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct KeyPattern {
    code: KeyCode,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Keybinds {
    pub fn new(config: &HashMap<String, String>) -> Self {
        let mut bindings = HashMap::new();

        // Parse config bindings.
        for (action_name, key_str) in config {
            if let (Some(action), Some(pattern)) = (parse_action(action_name), parse_key(key_str)) {
                bindings.insert(pattern, action);
            }
        }

        // ═══════════════════════════════════════════════════════════════════
        // DEFAULT KEYBINDS — All use Ctrl modifier (or special keys).
        // NO bare letters — all letters available for typing.
        // ═══════════════════════════════════════════════════════════════════

        // ─── Core ───
        bindings.insert(kp(KeyCode::Enter, false, false, false), Action::Send); // Enter: send (steers if streaming)
        bindings.insert(kp(KeyCode::Enter, true, false, false), Action::Queue); // Ctrl+Enter: queue for post-idle
                                                                                // Note: Shift+Enter and Alt+Enter insert newline (handled before keybinds)
        bindings.insert(kp(KeyCode::Esc, false, false, false), Action::Abort); // Esc: abort turn
        bindings.insert(kp(KeyCode::Char('c'), true, false, false), Action::Quit); // Ctrl+C: quit
        bindings.insert(kp(KeyCode::Char('q'), true, false, false), Action::Quit); // Ctrl+Q: quit
        bindings.insert(kp(KeyCode::Char('p'), true, false, false), Action::Help); // Ctrl+P: command palette

        // ─── Scrolling ───
        // Note: Ctrl+Up/Down now used for semantic navigation (jump between messages)
        bindings.insert(kp(KeyCode::PageUp, false, false, false), Action::ScrollUp); // PageUp: scroll up
        bindings.insert(
            kp(KeyCode::PageDown, false, false, false),
            Action::ScrollDown,
        ); // PageDown: scroll down
        bindings.insert(kp(KeyCode::Home, true, false, false), Action::ScrollTop); // Ctrl+Home: top
        bindings.insert(kp(KeyCode::End, true, false, false), Action::ScrollBottom); // Ctrl+End: bottom

        // ─── Word navigation ───
        bindings.insert(kp(KeyCode::Left, true, false, false), Action::WordLeft); // Ctrl+Left: word left
        bindings.insert(kp(KeyCode::Right, true, false, false), Action::WordRight); // Ctrl+Right: word right
        bindings.insert(
            kp(KeyCode::Backspace, true, false, false),
            Action::DeleteWordLeft,
        ); // Ctrl+Backspace: delete word left
        bindings.insert(
            kp(KeyCode::Delete, true, false, false),
            Action::DeleteWordRight,
        ); // Ctrl+Delete: delete word right
           // Ctrl+H is sent by some terminals for Ctrl+Backspace
        bindings.insert(
            kp(KeyCode::Char('h'), true, false, false),
            Action::DeleteWordLeft,
        ); // Ctrl+H: delete word left

        // ─── Semantic navigation (jump between user/assistant messages) ───
        bindings.insert(kp(KeyCode::Up, true, false, false), Action::PrevUserMessage); // Ctrl+Up: prev user message
        bindings.insert(
            kp(KeyCode::Down, true, false, false),
            Action::NextUserMessage,
        ); // Ctrl+Down: next user message

        // ─── Message navigation ───
        // Note: Ctrl+K/J are now used for kill-ring. Use Alt+K/J for messages.
        bindings.insert(
            kp(KeyCode::Char('k'), false, true, false),
            Action::PrevMessage,
        ); // Alt+K: prev msg
        bindings.insert(
            kp(KeyCode::Char('j'), false, true, false),
            Action::NextMessage,
        ); // Alt+J: next msg

        // ─── Sidebar ───
        bindings.insert(
            kp(KeyCode::Char('b'), true, false, false),
            Action::SessionPicker,
        ); // Ctrl+B: toggle left

        // ─── Sessions ───
        bindings.insert(
            kp(KeyCode::Char('n'), true, false, false),
            Action::NewSession,
        ); // Ctrl+N: new session

        // ─── Tool mode ───
        bindings.insert(kp(KeyCode::Char('t'), true, false, false), Action::ToolMode); // Ctrl+T: tool mode

        // ─── Undo/Redo ───
        bindings.insert(kp(KeyCode::Char('z'), true, false, false), Action::Undo); // Ctrl+Z: undo
                                                                                   // Note: Ctrl+Y is used for Yank (Emacs), not Redo
        bindings.insert(kp(KeyCode::Char('z'), true, false, true), Action::Redo); // Ctrl+Shift+Z: redo

        // ─── Kill Ring (Emacs-style) ───
        bindings.insert(
            kp(KeyCode::Char('k'), true, false, false),
            Action::KillToEnd,
        ); // Ctrl+K: kill to EOL
        bindings.insert(
            kp(KeyCode::Char('u'), true, false, false),
            Action::KillToStart,
        ); // Ctrl+U: kill to BOL
        bindings.insert(kp(KeyCode::Char('w'), true, false, false), Action::KillWord); // Ctrl+W: kill word
        bindings.insert(kp(KeyCode::Char('y'), true, false, false), Action::Yank); // Ctrl+Y: yank
        bindings.insert(kp(KeyCode::Char('y'), false, true, false), Action::YankPop); // Alt+Y: yank pop

        // ─── Thinking blocks ───
        bindings.insert(
            kp(KeyCode::Char('e'), true, false, false),
            Action::ToggleThinking,
        ); // Ctrl+E: toggle thinking

        // ─── Search ───
        bindings.insert(
            kp(KeyCode::Char('f'), true, false, false),
            Action::OpenSearch,
        ); // Ctrl+F: open search

        // ─── Clipboard ───
        // Note: Ctrl+V and Ctrl+Shift+V are intercepted by Windows Terminal.
        // F5 is used as a reliable cross-platform paste shortcut.
        bindings.insert(kp(KeyCode::Char('v'), true, false, false), Action::Paste); // Ctrl+V: paste (Linux/macOS)
        bindings.insert(kp(KeyCode::F(5), false, false, false), Action::Paste); // F5: paste (works everywhere)

        // ─── Model cycling ───
        bindings.insert(
            kp(KeyCode::F(2), false, false, false),
            Action::CycleModelNext,
        ); // F2: next model
        bindings.insert(
            kp(KeyCode::F(2), false, false, true),
            Action::CycleModelPrev,
        ); // Shift+F2: prev model

        Self { bindings }
    }

    /// Match a key event to an action.
    pub fn match_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // No multi-key sequences — all letters must be available for typing.
        // Scroll with PageUp/PageDown, Ctrl+Home/End, or mouse wheel.

        let pattern = KeyPattern {
            code: key.code,
            ctrl,
            alt,
            shift,
        };

        self.bindings.get(&pattern).copied()
    }
}

/// Parse an action name, as used in config files and `kn9t.action("...")`.
pub fn parse_action(name: &str) -> Option<Action> {
    match name {
        "quit" => Some(Action::Quit),
        "abort" => Some(Action::Abort),
        "help" => Some(Action::Help),
        "send" => Some(Action::Send),
        "queue" => Some(Action::Queue),
        "paste" => Some(Action::Paste),
        "scroll_up" => Some(Action::ScrollUp),
        "scroll_down" => Some(Action::ScrollDown),
        "scroll_top" => Some(Action::ScrollTop),
        "scroll_bottom" => Some(Action::ScrollBottom),
        "prev_message" => Some(Action::PrevMessage),
        "next_message" => Some(Action::NextMessage),
        // `toggle_left`/`toggle_right` are kept as aliases: they named this
        // behaviour in existing configs, and silently dropping a binding is
        // worse than an imprecise name.
        "session_picker" | "toggle_left" => Some(Action::SessionPicker),
        "new_session" => Some(Action::NewSession),
        "tool_mode" | "toolmode" => Some(Action::ToolMode),
        "undo" => Some(Action::Undo),
        "redo" => Some(Action::Redo),
        "kill_to_end" | "kill_line" => Some(Action::KillToEnd),
        "kill_to_start" => Some(Action::KillToStart),
        "kill_word" => Some(Action::KillWord),
        "yank" => Some(Action::Yank),
        "yank_pop" => Some(Action::YankPop),
        "toggle_thinking" => Some(Action::ToggleThinking),
        "open_search" | "search" => Some(Action::OpenSearch),
        "word_left" => Some(Action::WordLeft),
        "word_right" => Some(Action::WordRight),
        "delete_word_left" => Some(Action::DeleteWordLeft),
        "delete_word_right" => Some(Action::DeleteWordRight),
        "prev_user_message" => Some(Action::PrevUserMessage),
        "next_user_message" => Some(Action::NextUserMessage),
        "cycle_model_next" | "model_next" => Some(Action::CycleModelNext),
        "cycle_model_prev" | "model_prev" => Some(Action::CycleModelPrev),
        "open_models" | "models" => Some(Action::OpenModels),
        "open_tools" | "tools" => Some(Action::OpenTools),
        "open_palette" | "palette" => Some(Action::OpenPalette),
        "refresh_tools" => Some(Action::RefreshTools),
        "switch_session" => Some(Action::SwitchSession),
        "focus_plugin" => Some(Action::FocusPlugin),
        _ => None,
    }
}

fn parse_key(s: &str) -> Option<KeyPattern> {
    // A literal single space is a valid key, but `trim()` below would erase it
    // and leave an empty string that falls through to `None`. Handled first so
    // `" "` and the `"Space"` spelling both work — `key_event_to_string` never
    // emits `" "`, but a plugin binding printable characters in a loop does.
    if s == " " {
        return Some(kp(KeyCode::Char(' '), false, false, false));
    }

    let s = s.trim();
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut rest = s;

    // Parse modifiers: C-, A-, S-.
    loop {
        if rest.starts_with("C-") {
            ctrl = true;
            rest = &rest[2..];
        } else if rest.starts_with("A-") {
            alt = true;
            rest = &rest[2..];
        } else if rest.starts_with("S-") {
            shift = true;
            rest = &rest[2..];
        } else {
            break;
        }
    }

    let code = match rest {
        "Enter" | "enter" | "Return" | "return" => KeyCode::Enter,
        "Esc" | "esc" | "Escape" | "escape" => KeyCode::Esc,
        "Space" | "space" => KeyCode::Char(' '),
        "Tab" | "tab" => KeyCode::Tab,
        "Up" | "up" => KeyCode::Up,
        "Down" | "down" => KeyCode::Down,
        "Left" | "left" => KeyCode::Left,
        "Right" | "right" => KeyCode::Right,
        "Home" | "home" => KeyCode::Home,
        "End" | "end" => KeyCode::End,
        "PageUp" | "pageup" | "PgUp" | "pgup" => KeyCode::PageUp,
        "PageDown" | "pagedown" | "PgDn" | "pgdn" => KeyCode::PageDown,
        // Editing keys. These must round-trip with `key_event_to_string`, which
        // already emits "Backspace"/"Delete"/"Insert": without these arms a
        // binding on any of them was silently rejected as unparseable even
        // though the live event produced exactly that name.
        "Backspace" | "backspace" | "BS" | "bs" => KeyCode::Backspace,
        "Delete" | "delete" | "Del" | "del" => KeyCode::Delete,
        "Insert" | "insert" | "Ins" | "ins" => KeyCode::Insert,
        "BackTab" | "backtab" => KeyCode::BackTab,
        "F1" | "f1" => KeyCode::F(1),
        "F2" | "f2" => KeyCode::F(2),
        "F3" | "f3" => KeyCode::F(3),
        "F4" | "f4" => KeyCode::F(4),
        "F5" | "f5" => KeyCode::F(5),
        "F6" | "f6" => KeyCode::F(6),
        "F7" | "f7" => KeyCode::F(7),
        "F8" | "f8" => KeyCode::F(8),
        "F9" | "f9" => KeyCode::F(9),
        "F10" | "f10" => KeyCode::F(10),
        "F11" | "f11" => KeyCode::F(11),
        "F12" | "f12" => KeyCode::F(12),
        _ if rest.len() == 1 => KeyCode::Char(rest.chars().next()?),
        _ => return None,
    };

    Some(KeyPattern {
        code,
        ctrl,
        alt,
        shift,
    })
}

/// Whether `s` is a key string this system can actually match.
///
/// Lets Lua keymap registration reject typos up front instead of creating
/// bindings that could never fire.
pub fn is_valid_key_string(s: &str) -> bool {
    parse_key(s).is_some()
}

/// Whether `name` is a known action, for validating `kn9t.action("...")`.
pub fn is_valid_action_name(name: &str) -> bool {
    parse_action(name).is_some()
}

/// Canonical key name for a `KeyEvent`, in the same syntax `parse_key` accepts.
///
/// Used to look up Lua keymaps: Lua registers `"C-t"`, we turn the live event
/// back into `"C-t"` and compare. Round-trips with [`parse_key`].
pub fn key_event_to_string(key: KeyEvent) -> Option<String> {
    let mut s = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        s.push_str("C-");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        s.push_str("A-");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        s.push_str("S-");
    }

    match key.code {
        KeyCode::Enter => s.push_str("Enter"),
        KeyCode::Esc => s.push_str("Esc"),
        KeyCode::Tab => s.push_str("Tab"),
        KeyCode::Up => s.push_str("Up"),
        KeyCode::Down => s.push_str("Down"),
        KeyCode::Left => s.push_str("Left"),
        KeyCode::Right => s.push_str("Right"),
        KeyCode::Home => s.push_str("Home"),
        KeyCode::End => s.push_str("End"),
        KeyCode::PageUp => s.push_str("PageUp"),
        KeyCode::PageDown => s.push_str("PageDown"),
        KeyCode::Backspace => s.push_str("Backspace"),
        KeyCode::Delete => s.push_str("Delete"),
        KeyCode::F(n) => s.push_str(&format!("F{n}")),
        KeyCode::Char(' ') => s.push_str("Space"),
        KeyCode::Char(c) => s.push(c),
        _ => return None,
    }

    Some(s)
}

