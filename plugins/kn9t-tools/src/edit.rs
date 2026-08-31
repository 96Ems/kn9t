//! edit tool — exact-string file replacement with stale-read detection.
//!
//! R-PLUG2-130: checks READ_MAP from read.rs to detect conflicting modifications.
//!
//! Features (ported from Pi's edit-diff.ts):
//! - Line ending detection and restoration (CRLF/LF preservation)
//! - Two-phase matching: exact first, then fuzzy with Unicode normalization
//! - Fuzzy matching handles smart quotes, dashes, special spaces
//! - Context-aware error messages

use kn9t_plugin_sdk::{ctx::ToolCallCtx, traits::{PluginTool, ToolOutput}, wire::{Effect, EffectKind, ToolSpec}};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::read::read_map;

// ── Encoding detection ───────────────────────────────────────────────────────

/// Decode bytes to text, returning (BOM bytes, decoded string).
/// Handles UTF-8, UTF-16 LE/BE (with or without BOM).
fn decode_with_bom(data: &[u8]) -> (Vec<u8>, String) {
    // UTF-16 LE BOM
    if data.starts_with(&[0xFF, 0xFE]) {
        let text = decode_utf16_le(&data[2..]);
        return (vec![0xFF, 0xFE], text);
    }
    // UTF-16 BE BOM
    if data.starts_with(&[0xFE, 0xFF]) {
        let text = decode_utf16_be(&data[2..]);
        return (vec![0xFE, 0xFF], text);
    }
    // UTF-8 BOM
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        let text = String::from_utf8_lossy(&data[3..]).into_owned();
        return (vec![0xEF, 0xBB, 0xBF], text);
    }
    // Heuristic: if lots of null bytes interleaved, likely UTF-16 LE without BOM
    if data.len() >= 4 && data[1] == 0 && data[3] == 0 {
        let text = decode_utf16_le(data);
        return (vec![], text);
    }
    // Default: UTF-8 without BOM
    (vec![], String::from_utf8_lossy(data).into_owned())
}

fn decode_utf16_le(data: &[u8]) -> String {
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
}

fn decode_utf16_be(data: &[u8]) -> String {
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
}

/// Encode text back with the original BOM/encoding.
fn encode_with_bom(bom: &[u8], text: &str) -> Vec<u8> {
    match bom {
        [0xFF, 0xFE] => {
            // UTF-16 LE
            let mut out = vec![0xFF, 0xFE];
            for c in text.encode_utf16() {
                out.extend_from_slice(&c.to_le_bytes());
            }
            out
        }
        [0xFE, 0xFF] => {
            // UTF-16 BE
            let mut out = vec![0xFE, 0xFF];
            for c in text.encode_utf16() {
                out.extend_from_slice(&c.to_be_bytes());
            }
            out
        }
        [0xEF, 0xBB, 0xBF] => {
            // UTF-8 with BOM
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            out
        }
        _ => {
            // UTF-8 without BOM (or unknown)
            text.as_bytes().to_vec()
        }
    }
}

// ── Line ending handling ─────────────────────────────────────────────────────

/// Detect the dominant line ending in content.
fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    
    match (lf_idx, crlf_idx) {
        (None, _) => "\n",           // No newlines, default to LF
        (_, None) => "\n",           // Only LF found
        (Some(lf), Some(crlf)) => {
            if crlf < lf { "\r\n" } else { "\n" }
        }
    }
}

/// Normalize all line endings to LF for matching.
fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Restore line endings to the original style.
fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

// ── Fuzzy matching ───────────────────────────────────────────────────────────

/// Normalize text for fuzzy matching:
/// - Strip trailing whitespace per line
/// - Smart quotes → ASCII quotes
/// - Unicode dashes → ASCII hyphen
/// - Special spaces → regular space
fn normalize_for_fuzzy_match(text: &str) -> String {
    text
        // Strip trailing whitespace per line
        .lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        // Smart single quotes → '
        .replace('\u{2018}', "'")  // '
        .replace('\u{2019}', "'")  // '
        .replace('\u{201A}', "'")  // ‚
        .replace('\u{201B}', "'")  // ‛
        // Smart double quotes → "
        .replace('\u{201C}', "\"") // "
        .replace('\u{201D}', "\"") // "
        .replace('\u{201E}', "\"") // „
        .replace('\u{201F}', "\"") // ‟
        // Various dashes → -
        .replace('\u{2010}', "-")  // hyphen
        .replace('\u{2011}', "-")  // non-breaking hyphen
        .replace('\u{2012}', "-")  // figure dash
        .replace('\u{2013}', "-")  // en-dash
        .replace('\u{2014}', "-")  // em-dash
        .replace('\u{2015}', "-")  // horizontal bar
        .replace('\u{2212}', "-")  // minus sign
        // Special spaces → regular space
        .replace('\u{00A0}', " ")  // NBSP
        .replace('\u{2002}', " ")  // en space
        .replace('\u{2003}', " ")  // em space
        .replace('\u{2004}', " ")  // three-per-em space
        .replace('\u{2005}', " ")  // four-per-em space
        .replace('\u{2006}', " ")  // six-per-em space
        .replace('\u{2007}', " ")  // figure space
        .replace('\u{2008}', " ")  // punctuation space
        .replace('\u{2009}', " ")  // thin space
        .replace('\u{200A}', " ")  // hair space
        .replace('\u{202F}', " ")  // narrow NBSP
        .replace('\u{205F}', " ")  // medium math space
        .replace('\u{3000}', " ")  // ideographic space
}

/// Result of text matching.
struct MatchResult {
    found: bool,
    index: usize,
    match_length: usize,
    used_fuzzy: bool,
}

/// Find old_text in content using two-phase matching:
/// 1. Try exact match first
/// 2. If exact fails, try fuzzy match with Unicode normalization
fn find_text(content: &str, old_text: &str) -> MatchResult {
    // Phase 1: Exact match
    if let Some(idx) = content.find(old_text) {
        return MatchResult {
            found: true,
            index: idx,
            match_length: old_text.len(),
            used_fuzzy: false,
        };
    }
    
    // Phase 2: Fuzzy match
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    
    if let Some(idx) = fuzzy_content.find(&fuzzy_old) {
        return MatchResult {
            found: true,
            index: idx,
            match_length: fuzzy_old.len(),
            used_fuzzy: true,
        };
    }
    
    MatchResult {
        found: false,
        index: 0,
        match_length: 0,
        used_fuzzy: false,
    }
}

/// Count occurrences using fuzzy matching.
fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old = normalize_for_fuzzy_match(old_text);
    
    if fuzzy_old.is_empty() {
        return 0;
    }
    
    fuzzy_content.matches(&fuzzy_old).count()
}

// ── Edit tool ────────────────────────────────────────────────────────────────

pub struct Edit;

impl PluginTool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace text in a file. Supports exact and fuzzy matching \
                (handles smart quotes, dashes, special spaces). The file must have been \
                read first ('read' tool) and must not have been modified since. \
                Line endings (CRLF/LF) are preserved."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path":       { "type": "string", "description": "Path to the file." },
                    "old_string": { "type": "string", "description": "Text to replace (exact or fuzzy match)." },
                    "new_string": { "type": "string", "description": "Replacement text." }
                },
                "required": ["path", "old_string", "new_string"]
            }),
            parallel_safe: false,
            hidden: false,
            effects: vec![Effect { field: "path".into(), kind: EffectKind::FsWrite }],
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => PathBuf::from(p),
            None => return ToolOutput::error("missing 'path'"),
        };
        let old = match args.get("old_string").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error("missing 'old_string'"),
        };
        let new = match args.get("new_string").and_then(|s| s.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error("missing 'new_string'"),
        };

        // Validate: old_string must not be empty
        if old.is_empty() {
            return ToolOutput::error("old_string must not be empty");
        }

        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }

        // Stale-read check: file must not have been modified since last read.
        {
            let map = read_map();
            let map = map.lock().unwrap();
            if let Some((_sha, tracked_mtime)) = map.get(&path) {
                let current_mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                if current_mtime > *tracked_mtime {
                    return ToolOutput::error(
                        "file was modified since last read — re-read it before editing",
                    );
                }
            } else {
                return ToolOutput::error(
                    "file has not been read — use 'read' before 'edit'",
                );
            }
        }

        // Read the file as raw bytes first to detect BOM and line endings
        let raw_bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("read error: {e}")),
        };

        // Detect encoding and decode
        let (bom, content) = decode_with_bom(&raw_bytes);

        // Detect original line ending style
        let original_ending = detect_line_ending(&content);

        // Normalize content and old_string to LF for matching
        let normalized_content = normalize_to_lf(&content);
        let normalized_old = normalize_to_lf(&old);
        let normalized_new = normalize_to_lf(&new);

        // Check for duplicates using fuzzy matching
        let occurrences = count_occurrences(&normalized_content, &normalized_old);
        if occurrences == 0 {
            return ToolOutput::error(
                "old_string not found in file. The text must match exactly \
                 including whitespace and newlines."
            );
        }
        if occurrences > 1 {
            return ToolOutput::error(format!(
                "Found {} occurrences of the text. The text must be unique. \
                 Please provide more context to make it unique.",
                occurrences
            ));
        }

        // Find the match (exact or fuzzy)
        let match_result = find_text(&normalized_content, &normalized_old);
        if !match_result.found {
            return ToolOutput::error("old_string not found in file");
        }

        // Apply the replacement
        let updated_normalized = if match_result.used_fuzzy {
            // When using fuzzy match, we need to work in normalized space
            let fuzzy_content = normalize_for_fuzzy_match(&normalized_content);
            let fuzzy_old = normalize_for_fuzzy_match(&normalized_old);
            let fuzzy_new = normalize_for_fuzzy_match(&normalized_new);
            
            // Replace in fuzzy-normalized content
            let fuzzy_updated = fuzzy_content.replacen(&fuzzy_old, &fuzzy_new, 1);
            
            // For fuzzy matches, we use the fuzzy-normalized result
            // This may lose some original formatting, but ensures the edit works
            fuzzy_updated
        } else {
            // Exact match: simple replace
            normalized_content.replacen(&normalized_old, &normalized_new, 1)
        };

        // Check that something actually changed
        if normalized_content == updated_normalized {
            return ToolOutput::error(
                "No changes made. The replacement produced identical content."
            );
        }

        // Restore original line endings
        let updated_with_endings = restore_line_endings(&updated_normalized, original_ending);

        // Reconstruct with BOM and original encoding
        let final_bytes = encode_with_bom(&bom, &updated_with_endings);

        if let Err(e) = std::fs::write(&path, &final_bytes) {
            return ToolOutput::error(format!("write error: {e}"));
        }

        // Update the tracking entry so subsequent edits on the same file work.
        {
            let map = read_map();
            let mut map = map.lock().unwrap();
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let sha = crate::read::sha256_pub(&final_bytes);
            map.insert(path.clone(), (sha, mtime));
        }

        // Emit unified diff via progress for TUI display
        let path_str = path.display().to_string();
        emit_unified_diff(ctx, &path_str, &normalized_content, &updated_normalized);

        let fuzzy_note = if match_result.used_fuzzy {
            " (fuzzy match)"
        } else {
            ""
        };
        ToolOutput::text(format!("edit applied to {}{}", path.display(), fuzzy_note))
    }
}

// ── Unified Diff ─────────────────────────────────────────────────────────────

/// Emit a unified diff of before/after via ctx.progress for TUI display.
fn emit_unified_diff(ctx: &ToolCallCtx, path: &str, before: &str, after: &str) {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();

    // Simple diff: find first and last differing lines
    let mut first_diff = 0;
    let mut last_diff_a = a.len();
    let mut last_diff_b = b.len();

    // Find first difference
    for i in 0..a.len().min(b.len()) {
        if a[i] != b[i] {
            first_diff = i;
            break;
        }
        first_diff = i + 1;
    }

    // Find last difference (from end)
    let mut ai = a.len();
    let mut bi = b.len();
    while ai > first_diff && bi > first_diff {
        ai -= 1;
        bi -= 1;
        if a[ai] != b[bi] {
            last_diff_a = ai + 1;
            last_diff_b = bi + 1;
            break;
        }
        last_diff_a = ai;
        last_diff_b = bi;
    }

    // Emit diff header
    ctx.progress.send(&format!("--- a/{}", path));
    ctx.progress.send(&format!("+++ b/{}", path));

    // Context lines before
    let ctx_start = first_diff.saturating_sub(3);
    let ctx_end_a = (last_diff_a + 3).min(a.len());
    let ctx_end_b = (last_diff_b + 3).min(b.len());

    ctx.progress.send(&format!(
        "@@ -{},{} +{},{} @@",
        ctx_start + 1,
        ctx_end_a - ctx_start,
        ctx_start + 1,
        ctx_end_b - ctx_start
    ));

    // Emit context before
    for i in ctx_start..first_diff {
        if i < a.len() {
            ctx.progress.send(&format!(" {}", a[i]));
        }
    }

    // Emit removed lines
    for i in first_diff..last_diff_a {
        if i < a.len() {
            ctx.progress.send(&format!("-{}", a[i]));
        }
    }

    // Emit added lines
    for i in first_diff..last_diff_b {
        if i < b.len() {
            ctx.progress.send(&format!("+{}", b[i]));
        }
    }

    // Emit context after
    for i in last_diff_a..ctx_end_a {
        if i < a.len() {
            ctx.progress.send(&format!(" {}", a[i]));
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_line_ending_lf() {
        assert_eq!(detect_line_ending("foo\nbar\n"), "\n");
    }

    #[test]
    fn test_detect_line_ending_crlf() {
        assert_eq!(detect_line_ending("foo\r\nbar\r\n"), "\r\n");
    }

    #[test]
    fn test_detect_line_ending_mixed_crlf_first() {
        // CRLF appears before LF
        assert_eq!(detect_line_ending("foo\r\nbar\nbaz"), "\r\n");
    }

    #[test]
    fn test_detect_line_ending_no_newlines() {
        assert_eq!(detect_line_ending("foobar"), "\n");
    }

    #[test]
    fn test_normalize_to_lf() {
        assert_eq!(normalize_to_lf("foo\r\nbar\r\n"), "foo\nbar\n");
        assert_eq!(normalize_to_lf("foo\rbar\n"), "foo\nbar\n");
        assert_eq!(normalize_to_lf("foo\nbar\n"), "foo\nbar\n");
    }

    #[test]
    fn test_restore_line_endings() {
        assert_eq!(restore_line_endings("foo\nbar\n", "\r\n"), "foo\r\nbar\r\n");
        assert_eq!(restore_line_endings("foo\nbar\n", "\n"), "foo\nbar\n");
    }

    #[test]
    fn test_normalize_smart_quotes() {
        // U+201C " and U+201D " -> "
        // U+2018 ' and U+2019 ' -> '
        let input = "\u{201C}Hello\u{201D} and \u{2018}world\u{2019}";
        let expected = "\"Hello\" and 'world'";
        assert_eq!(normalize_for_fuzzy_match(input), expected);
    }

    #[test]
    fn test_normalize_dashes() {
        let input = "foo–bar—baz";  // en-dash and em-dash
        let expected = "foo-bar-baz";
        assert_eq!(normalize_for_fuzzy_match(input), expected);
    }

    #[test]
    fn test_normalize_special_spaces() {
        let input = "foo\u{00A0}bar";  // NBSP
        let expected = "foo bar";
        assert_eq!(normalize_for_fuzzy_match(input), expected);
    }

    #[test]
    fn test_normalize_trailing_whitespace() {
        let input = "foo   \nbar  \n";
        let expected = "foo\nbar";
        assert_eq!(normalize_for_fuzzy_match(input), expected);
    }

    #[test]
    fn test_find_text_exact() {
        let result = find_text("hello world", "world");
        assert!(result.found);
        assert_eq!(result.index, 6);
        assert!(!result.used_fuzzy);
    }

    #[test]
    fn test_find_text_fuzzy_smart_quotes() {
        let content = "say \"hello\"";
        // U+201C " and U+201D " (smart quotes)
        let old_text = "say \u{201C}hello\u{201D}";
        let result = find_text(content, old_text);
        assert!(result.found);
        assert!(result.used_fuzzy);
    }

    #[test]
    fn test_find_text_not_found() {
        let result = find_text("hello world", "xyz");
        assert!(!result.found);
    }

    #[test]
    fn test_count_occurrences() {
        assert_eq!(count_occurrences("foo bar foo baz foo", "foo"), 3);
        assert_eq!(count_occurrences("hello world", "xyz"), 0);
        assert_eq!(count_occurrences("hello world", "world"), 1);
    }

    #[test]
    fn test_count_occurrences_fuzzy() {
        // Smart quotes should match regular quotes
        let content = "say \"hello\" and \"world\"";
        // U+201C " and U+201D " (smart quotes)
        let search = "say \u{201C}hello\u{201D}";
        assert_eq!(count_occurrences(content, search), 1);
    }
}
