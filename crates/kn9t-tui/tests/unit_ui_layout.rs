//! Unit tests for ui/layout — extracted from src/ui/layout.rs
#![allow(clippy::unwrap_used)]

use kn9t_tui::ui::layout::input_height_for;

#[test]
fn narrower_region_needs_at_least_as_many_rows() {
    // `input_height_for` is the single source of truth published to Lua as
    // `ctx.input_height`. It must use the real width of the input region, or
    // a Lua-defined input box drifts from where the cursor actually lands.
    let long = "x".repeat(200);
    let wide = input_height_for(120, &long, 10);
    let narrow = input_height_for(86, &long, 10);
    assert!(narrow >= wide, "narrow={narrow} wide={wide}");
    assert!(wide >= 1, "must always claim at least one row");
}

#[test]
fn input_height_is_capped() {
    let huge = "y".repeat(10_000);
    assert!(input_height_for(80, &huge, 5) <= 5);
}

#[test]
fn empty_input_claims_one_row() {
    assert_eq!(input_height_for(80, "", 10), 1);
}

/// A degenerate width must not panic or report zero rows: the cursor still
/// has to live somewhere.
#[test]
fn tiny_width_still_claims_a_row() {
    assert_eq!(input_height_for(1, "hello", 10), 1);
    assert_eq!(input_height_for(0, "hello", 10), 1);
}

#[test]
fn newlines_add_rows() {
    assert_eq!(input_height_for(80, "a\nb\nc", 10), 3);
    // A trailing newline opens a new, empty row.
    assert_eq!(input_height_for(80, "a\n", 10), 2);
}
