//! Input-height math.
//!
//! Layout is defined in Lua (`render_ui`), so this module computes no screen
//! regions. What remains is `input_height_for`, the single source of truth for
//! how many rows the input box needs — published to Lua as `ctx.input_height`
//! so both sides agree.
//!
//! This deliberately takes the **measured** width of the input region rather
//! than deriving it from a sidebar model. The previous version subtracted a
//! hardcoded 24-column sidebar that no longer existed, so `ctx.input_height`
//! disagreed with wherever Lua actually put the input box.

/// Calculate required input height based on content and available width.
/// Returns number of lines needed (minimum 1, capped at max_lines).
pub fn calculate_input_height(input: &str, available_width: u16, max_lines: u16) -> u16 {
    if input.is_empty() {
        return 1;
    }

    // Account for prompt "› " (2 chars).
    let content_width = available_width.saturating_sub(2) as usize;
    if content_width == 0 {
        return 1;
    }

    let mut total_lines: u16 = 0;
    for line in input.lines() {
        // Each logical line may wrap into multiple display lines.
        let char_count = line.chars().count();
        let wrapped_lines = if char_count == 0 {
            1
        } else {
            char_count.div_ceil(content_width) as u16
        };
        total_lines = total_lines.saturating_add(wrapped_lines);
    }

    // Handle trailing newline (adds an empty line).
    if input.ends_with('\n') {
        total_lines = total_lines.saturating_add(1);
    }

    // Ensure at least 1 line, cap at max_lines.
    total_lines.clamp(1, max_lines)
}

/// Rows the input box needs, given the width Lua actually gave it.
///
/// `input_width` comes from the previous frame's resolved geometry
/// (`collect_natives`), falling back to the terminal width on the first frame.
/// That closes the loop a fixed sidebar width used to break.
pub fn input_height_for(input_width: u16, input: &str, max_input_lines: u16) -> u16 {
    calculate_input_height(input, input_width, max_input_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
