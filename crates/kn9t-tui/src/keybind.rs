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

    // Sidebar
    ToggleLeft,  // Ctrl+B
    ToggleRight, // Ctrl+R (or always visible)

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

    // Unused for now
    PrevUser,
    NextUser,
    PrevAssistant,
    NextAssistant,
    SessionList,
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
            Action::ToggleLeft,
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

fn parse_action(name: &str) -> Option<Action> {
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
        "toggle_left" => Some(Action::ToggleLeft),
        "toggle_right" => Some(Action::ToggleRight),
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
        _ => None,
    }
}

fn parse_key(s: &str) -> Option<KeyPattern> {
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
