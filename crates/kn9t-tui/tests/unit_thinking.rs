//! Unit tests for the reasoning card renderer.
#![allow(clippy::unwrap_used)]

use kn9t_tui::theme::Theme;
use kn9t_tui::thinking::{render_content, render_header};
use ratatui::text::Line;

fn text_of(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

#[test]
fn live_header_spins_and_states_no_size() {
    let text = text_of(&render_header(false, 42, true, &Theme::default()));
    assert!(
        text.contains('✻'),
        "a live card needs a spinner, got {text:?}"
    );
    assert!(text.contains("thinking"), "got {text:?}");
    assert!(
        !text.contains("lines"),
        "a still-growing card cannot state a size: {text:?}"
    );
}

#[test]
fn collapsed_header_is_a_disclosure_with_a_size() {
    let text = text_of(&render_header(true, 7, false, &Theme::default()));
    assert!(text.contains('▶'), "got {text:?}");
    assert!(text.contains("thinking"), "got {text:?}");
    assert!(text.contains("(7 lines)"), "got {text:?}");
}

#[test]
fn expanded_header_points_down() {
    let text = text_of(&render_header(false, 7, false, &Theme::default()));
    assert!(text.contains('▼'), "got {text:?}");
}

#[test]
fn content_carries_a_gutter_and_keeps_line_count() {
    let lines = render_content("first\nsecond", &Theme::default(), 40);
    assert_eq!(lines.len(), 2);
    for line in &lines {
        assert!(
            text_of(line).starts_with("│ "),
            "every reasoning line carries the gutter"
        );
    }
    assert!(text_of(&lines[0]).contains("first"));
    assert!(text_of(&lines[1]).contains("second"));
}
