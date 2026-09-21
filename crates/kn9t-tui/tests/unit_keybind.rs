use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kn9t_tui::keybind::{is_valid_key_string, key_event_to_string, parse_action};

// Re-expose private helpers needed by the tests via the public surface.
// `parse_key` and `KeyPattern` are private; we test them indirectly through
// `is_valid_key_string` (which calls `parse_key`) and `key_event_to_string`.

fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, mods)
}

/// The Lua keymap layer compares `key_event_to_string(event)` against the
/// string Lua registered. If the two spellings ever diverge, every binding
/// silently stops firing — so pin the round-trip.
#[test]
fn key_string_round_trips_through_parse_key() {
    let cases = [
        ("C-t", ev(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        ("A-y", ev(KeyCode::Char('y'), KeyModifiers::ALT)),
        ("Enter", ev(KeyCode::Enter, KeyModifiers::NONE)),
        ("Esc", ev(KeyCode::Esc, KeyModifiers::NONE)),
        ("F5", ev(KeyCode::F(5), KeyModifiers::NONE)),
        ("Space", ev(KeyCode::Char(' '), KeyModifiers::NONE)),
        ("PageUp", ev(KeyCode::PageUp, KeyModifiers::NONE)),
        ("C-Home", ev(KeyCode::Home, KeyModifiers::CONTROL)),
    ];

    for (expected, event) in cases {
        let s = key_event_to_string(event).unwrap_or_else(|| panic!("no string for {expected}"));
        assert_eq!(s, expected, "event -> string");
        assert!(
            is_valid_key_string(&s),
            "'{s}' must parse back, or Lua maps would be rejected"
        );
    }
}

#[test]
fn modifier_order_is_canonical() {
    // C- then A- then S-, matching how parse_key strips prefixes.
    let e = ev(
        KeyCode::Char('x'),
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
    );
    let s = key_event_to_string(e).unwrap();
    assert_eq!(s, "C-A-S-x");
    assert!(is_valid_key_string(&s));
}

#[test]
fn typos_are_invalid() {
    assert!(!is_valid_key_string("NotAKey"));
    assert!(!is_valid_key_string("Ctrl+T"));
    assert!(is_valid_key_string("C-t"));
}

/// `key_event_to_string` and `parse_key` must agree: the TUI turns a live
/// event into a name, then looks that name up among registered bindings. Any
/// name the former emits but the latter rejects is a key that can never be
/// bound — `Backspace` and `Delete` were exactly that, so a plugin view
/// binding them got "ignoring unparseable key" and silently lost the key.
///
/// Only codes `key_event_to_string` actually emits are listed: `Insert` and
/// `BackTab` fall into its `_ => None` arm, so they never reach a lookup and
/// asserting on them would test a path that does not exist.
#[test]
fn emitted_key_names_parse_back() {
    for code in [
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::F(1),
        KeyCode::F(10),
        KeyCode::Char('j'),
        KeyCode::Char(' '),
    ] {
        let ev = KeyEvent::new(code, KeyModifiers::NONE);
        let name =
            key_event_to_string(ev).unwrap_or_else(|| panic!("{code:?} must produce a name"));
        assert!(
            is_valid_key_string(&name),
            "{code:?} emits {name:?}, which parse_key rejects"
        );
    }
}

/// A bare space is a real key a plugin can bind while capturing text; the
/// `trim()` in `parse_key` used to erase it into an empty string.
#[test]
fn space_is_bindable_both_spellings() {
    assert!(is_valid_key_string(" "), "literal space");
    assert!(is_valid_key_string("Space"), "named space");
}

/// Smoke-test `parse_action` for a representative set of names.
#[test]
fn parse_action_known_names() {
    for name in [
        "quit",
        "abort",
        "send",
        "scroll_up",
        "scroll_down",
        "session_picker",
        "toggle_left",
        "tool_mode",
        "toolmode",
        "kill_line",
        "kill_to_end",
        "open_search",
        "search",
        "cycle_model_next",
        "model_next",
        "switch_session",
        "focus_plugin",
    ] {
        assert!(
            parse_action(name).is_some(),
            "parse_action({name:?}) returned None"
        );
    }
}

#[test]
fn parse_action_unknown_name_returns_none() {
    assert!(parse_action("not_an_action").is_none());
    assert!(parse_action("").is_none());
    assert!(parse_action("Quit").is_none()); // case-sensitive
}
