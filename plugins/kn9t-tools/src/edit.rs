//! edit tool — exact-string file replacement with stale-read detection.
//!
//! R-PLUG2-130: checks READ_MAP from read.rs to detect conflicting modifications.
//!
//! Features (ported from Pi's edit-diff.ts):
//! - Line ending detection and restoration (CRLF/LF preservation)
//! - Two-phase matching: exact first, then fuzzy with Unicode normalization
//! - Fuzzy matching handles smart quotes, dashes, special spaces
//! - Context-aware error messages

use kn9t_plugin_sdk::{ctx::ToolCallCtx, traits::{PluginTool, ToolOutput}, wire::{DefaultPolicy, Effect, EffectKind, ToolPolicy, ToolSpec}};
use serde_json::{json, Value};
use std::time::SystemTime;

use crate::encoding::{
    self, detect_line_ending, normalize_to_lf, restore_line_endings, TextEncoding,
};
use crate::read::read_map;

// ── Line ending handling ─────────────────────────────────────────────────────

// ── Fuzzy matching ───────────────────────────────────────────────────────────

/// Map one character to its fuzzy-matching equivalent: smart quotes to ASCII
/// quotes, Unicode dashes to `-`, special spaces to a plain space. Every
/// replacement is a single character, which is what lets the mapped normalizer
/// below stay aligned with the input.
fn fuzzy_char(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' => ' ',
        other => other,
    }
}

/// Normalize text for fuzzy matching: strip trailing whitespace per line, then
/// apply [`fuzzy_char`].
fn normalize_for_fuzzy_match(text: &str) -> String {
    text.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .map(fuzzy_char)
        .collect()
}

/// [`normalize_for_fuzzy_match`], plus a map from each output character to its
/// byte offset in the input.
///
/// The map is the point: a fuzzy match is found in normalized space, but the
/// file must be edited in its *original* space. Returning the offset lets the
/// caller replace only the matched span, instead of writing the fully
/// normalized content back and silently stripping trailing whitespace and
/// smart punctuation from every other line of the file.
fn normalize_fuzzy_mapped(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut out = String::with_capacity(text.len());
    // (byte offset, byte length) of each output character's source character.
    let mut map: Vec<(usize, usize)> = Vec::new();
    let mut offset = 0usize;
    let mut remaining = text;
    loop {
        let (line, rest) = match remaining.find('\n') {
            Some(i) => (&remaining[..i], Some(&remaining[i + 1..])),
            None => (remaining, None),
        };
        for (i, c) in line.trim_end().char_indices() {
            out.push(fuzzy_char(c));
            map.push((offset + i, c.len_utf8()));
        }
        match rest {
            Some(r) => {
                // `str::lines()` drops the empty final line a trailing '\n'
                // produces; mirror that so the normalized text matches exactly.
                if r.is_empty() {
                    break;
                }
                out.push('\n');
                map.push((offset + line.len(), 1));
                offset += line.len() + 1;
                remaining = r;
            }
            None => break,
        }
    }
    (out, map)
}

/// How `old_string` was located in the file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MatchKind {
    /// Byte-for-byte.
    Exact,
    /// Equal after whitespace/Unicode normalization.
    Fuzzy,
    /// Only equal after undoing cp1252→UTF-8 double-encoding.
    Repaired,
}

/// Outcome of locating `old_string`.
enum Resolution {
    Found {
        /// Byte span in whichever content base matched.
        start: usize,
        end: usize,
        kind: MatchKind,
    },
    /// Occurred `n > 1` times; the caller must supply more context.
    Duplicate(usize),
    NotFound,
}

/// Locate `old` in `content`: exact match first, fuzzy only if exact finds
/// nothing.
///
/// Exact-first is not an optimization, it is a correctness fix. Counting fuzzy
/// matches unconditionally made a file containing both `"x"` and `“x”` report
/// two occurrences of `"x"`, so an unambiguous edit was rejected with
/// "please provide more context" — for a match that was already exact.
fn resolve(content: &str, old: &str) -> Resolution {
    let exact = content.matches(old).count();
    if exact == 1 {
        let start = content.find(old).expect("count == 1 implies a match");
        return Resolution::Found {
            start,
            end: start + old.len(),
            kind: MatchKind::Exact,
        };
    }
    if exact > 1 {
        return Resolution::Duplicate(exact);
    }

    let (fuzzy_content, map) = normalize_fuzzy_mapped(content);
    let fuzzy_old = normalize_for_fuzzy_match(old);
    if fuzzy_old.is_empty() {
        // `old` was only whitespace, which normalization erased. `matches("")`
        // would claim a match at every position, so refuse rather than guess.
        return Resolution::NotFound;
    }
    let fuzzy_count = fuzzy_content.matches(&fuzzy_old).count();
    if fuzzy_count > 1 {
        return Resolution::Duplicate(fuzzy_count);
    }
    if fuzzy_count == 0 {
        return Resolution::NotFound;
    }

    let fstart = fuzzy_content
        .find(&fuzzy_old)
        .expect("count == 1 implies a match");
    let (start, end) = map_span(&fuzzy_content, &map, fstart, fuzzy_old.len());
    Resolution::Found {
        start,
        end,
        kind: MatchKind::Fuzzy,
    }
}

/// Translate a byte span in normalized space back to a byte span in the input.
fn map_span(fuzzy: &str, map: &[(usize, usize)], fstart: usize, flen: usize) -> (usize, usize) {
    let first = fuzzy[..fstart].chars().count();
    let last = fuzzy[..fstart + flen].chars().count() - 1;
    let (start, _) = map[first];
    let (last_start, last_len) = map[last];
    (start, last_start + last_len)
}

fn duplicate_error(n: usize) -> ToolOutput {
    ToolOutput::error(format!(
        "Found {n} occurrences of the text. The text must be unique. \
         Please provide more context to make it unique."
    ))
}

fn not_found_error() -> ToolOutput {
    ToolOutput::error(
        "old_string not found in file. The text must match exactly \
         including whitespace and newlines.",
    )
}

// ── Edit tool ────────────────────────────────────────────────────────────────

pub struct Edit;

impl PluginTool for Edit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit".into(),
            description: "Replace text in a file. Supports exact and fuzzy matching \
                (handles smart quotes, dashes, special spaces); only the matched span is \
                rewritten, so the rest of the file is untouched. The file's encoding is \
                detected (UTF-8, UTF-8 BOM, UTF-16, Windows-1252) and preserved, and \
                cp1252->UTF-8 mojibake left by a PowerShell write is repaired to UTF-8. \
                The file must have been observed first — via 'read', or a 'bash' command \
                naming the file — and must not have been modified since. \
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
            // Edit requires approval — Ask by default
            policy: ToolPolicy {
                pattern_field: Some("path".into()),
                default_policy: DefaultPolicy::Ask,
                builtin_allow: vec![],
                builtin_deny: vec![],
            },
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let path = match args.get("path").and_then(|p| p.as_str()) {
            // Relative to the session's cwd, not the plugin process's (see `crate::path`).
            Some(p) => crate::path::resolve(p, ctx.cwd.as_ref()),
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
                        "file was modified since it was last observed — re-read it before editing",
                    );
                }
            } else {
                return ToolOutput::error(
                    "file has not been read — use 'read' (or a 'bash' command naming the file) before 'edit'",
                );
            }
        }

        // Read the raw bytes; the encoding module decides what they are.
        let raw_bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("read error: {e}")),
        };

        // Decode with the file's real encoding, so a PowerShell-written
        // Windows-1252 file matches `é` instead of `�`.
        let (encoding, content) = encoding::decode(&raw_bytes);

        // Detect original line ending style
        let original_ending = detect_line_ending(&content);

        // Normalize content and old_string to LF for matching
        let normalized_content = normalize_to_lf(&content);
        let normalized_old = normalize_to_lf(&old);
        let normalized_new = normalize_to_lf(&new);

        // Resolve exact first, then fuzzy. If neither matches, the file may be
        // cp1252→UTF-8 mojibake from a PowerShell `Set-Content`: retry against
        // the repaired text and, if that matches, write the repair back too.
        let mut base = normalized_content;
        let mut out_encoding = encoding;
        let mut resolution = resolve(&base, &normalized_old);
        if matches!(resolution, Resolution::NotFound) {
            let repaired = encoding::repair_mojibake(&base);
            if repaired != base {
                match resolve(&repaired, &normalized_old) {
                    Resolution::Found { start, end, .. } => {
                        base = repaired;
                        // The repaired text is real UTF-8; writing it back as
                        // Windows-1252 would re-introduce the damage.
                        out_encoding = TextEncoding::Utf8;
                        resolution = Resolution::Found {
                            start,
                            end,
                            kind: MatchKind::Repaired,
                        };
                    }
                    other => resolution = other,
                }
            }
        }

        let (start, end, kind) = match resolution {
            Resolution::Found { start, end, kind } => (start, end, kind),
            Resolution::Duplicate(n) => return duplicate_error(n),
            Resolution::NotFound => return not_found_error(),
        };

        // Replace only the matched span. Everything else in the file — line
        // endings, trailing whitespace, smart punctuation — is left alone.
        let updated_normalized = format!("{}{}{}", &base[..start], normalized_new, &base[end..]);

        // Check that something actually changed
        if base == updated_normalized {
            return ToolOutput::error(
                "No changes made. The replacement produced identical content.",
            );
        }

        // Restore original line endings
        let updated_with_endings = restore_line_endings(&updated_normalized, original_ending);

        // Re-encode in the file's original encoding, unless the new text forced
        // an upgrade to UTF-8.
        let (final_bytes, encoding_upgraded) =
            encoding::encode_checked(out_encoding, &updated_with_endings);

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
        emit_unified_diff(ctx, &path_str, &base, &updated_normalized);

        let mut notes: Vec<&str> = Vec::new();
        match kind {
            MatchKind::Exact => {}
            MatchKind::Fuzzy => notes.push("fuzzy match"),
            MatchKind::Repaired => notes.push("encoding repaired: cp1252 mojibake -> UTF-8"),
        }
        if encoding_upgraded {
            notes.push("encoding upgraded to UTF-8");
        } else if out_encoding == TextEncoding::Windows1252 {
            notes.push("Windows-1252 preserved");
        }
        let note = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join("; "))
        };
        ToolOutput::text(format!("edit applied to {}{}", path.display(), note))
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
    use crate::test_support::{ctx, scratch, text};
    use serde_json::json;

    /// Write a file and register it as observed, satisfying the stale guard.
    fn write_track(path: &std::path::Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        assert!(crate::read::track_as_read(path));
    }

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
    fn mapped_normalizer_agrees_with_the_plain_one() {
        for s in [
            "foo   \nbar  \n",
            "a\r\nb",
            "",
            "a",
            "\n",
            "a\n\nb",
            "\u{201C}hi\u{201D}",
        ] {
            let (mapped, map) = normalize_fuzzy_mapped(s);
            assert_eq!(mapped, normalize_for_fuzzy_match(s), "for {s:?}");
            assert_eq!(mapped.chars().count(), map.len(), "map length for {s:?}");
        }
    }

    #[test]
    fn fuzzy_span_maps_back_to_the_original_bytes() {
        let content = "let x = \u{201C}hi\u{201D};   \nkeep\n";
        let old = "let x = \"hi\";";
        match resolve(content, old) {
            Resolution::Found { start, end, kind } => {
                assert_eq!(kind, MatchKind::Fuzzy);
                // Only the matched span — not the trailing whitespace on the line.
                assert_eq!(&content[start..end], "let x = \u{201C}hi\u{201D};");
            }
            _ => panic!("expected a fuzzy match"),
        }
    }

    #[test]
    fn exact_match_wins_over_a_fuzzy_lookalike() {
        // The regression: a file with both "x" and “x” used to report two
        // matches for "x" and reject an edit that was unambiguous.
        let content = "let a = \"x\";\nlet b = \u{201C}x\u{201D};\n";
        match resolve(content, "\"x\"") {
            Resolution::Found { kind, .. } => assert_eq!(kind, MatchKind::Exact),
            _ => panic!("expected an exact match"),
        }
    }

    #[test]
    fn genuinely_duplicated_text_is_rejected() {
        assert!(matches!(resolve("x\nx\n", "x"), Resolution::Duplicate(2)));
    }

    #[test]
    fn absent_text_is_not_found() {
        assert!(matches!(resolve("hello", "xyz"), Resolution::NotFound));
    }

    #[test]
    fn whitespace_only_old_string_is_not_found() {
        assert!(matches!(resolve("a\nb\n", "   "), Resolution::NotFound));
    }

    #[test]
    fn execute_only_rewrites_the_matched_span() {
        let dir = scratch("edit_fuzzy_scope");
        let file = dir.join("a.txt");
        write_track(&file, "say \u{201C}hi\u{201D}\nkeep  \n".as_bytes());

        let out = Edit.execute(
            &json!({ "path": "a.txt", "old_string": "say \"hi\"", "new_string": "said hi" }),
            &ctx(Some(dir.clone())),
        );
        assert!(!out.is_error, "{}", text(&out));
        // The fuzzy match must not have stripped the trailing spaces elsewhere.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "said hi\nkeep  \n");
    }

    #[test]
    fn execute_preserves_crlf_line_endings() {
        let dir = scratch("edit_crlf");
        let file = dir.join("a.txt");
        write_track(&file, b"line1\r\nold\r\nline3\r\n");

        let out = Edit.execute(
            &json!({ "path": "a.txt", "old_string": "old", "new_string": "new" }),
            &ctx(Some(dir.clone())),
        );
        assert!(!out.is_error, "{}", text(&out));
        assert_eq!(std::fs::read(&file).unwrap(), b"line1\r\nnew\r\nline3\r\n");
    }

    #[test]
    fn execute_reads_and_preserves_windows_1252() {
        let dir = scratch("edit_cp1252");
        let file = dir.join("a.txt");
        // PowerShell 5.1's ANSI output: é is a lone 0xE9 byte, not valid UTF-8.
        write_track(&file, b"caf\xE9 value\n");

        let out = Edit.execute(
            &json!({ "path": "a.txt", "old_string": "café value", "new_string": "café total" }),
            &ctx(Some(dir.clone())),
        );
        assert!(!out.is_error, "{}", text(&out));
        assert_eq!(std::fs::read(&file).unwrap(), b"caf\xE9 total\n");
    }

    #[test]
    fn execute_repairs_mojibake_and_writes_utf8() {
        let dir = scratch("edit_mojibake");
        let file = dir.join("a.txt");
        // "café — ok" after Set-Content -Encoding UTF8 double-encoded it.
        let damaged = "caf\u{00C3}\u{00A9} \u{00E2}\u{20AC}\u{201D} ok\n";
        write_track(&file, damaged.as_bytes());

        let out = Edit.execute(
            &json!({ "path": "a.txt", "old_string": "café — ok", "new_string": "café — done" }),
            &ctx(Some(dir.clone())),
        );
        assert!(!out.is_error, "{}", text(&out));
        assert!(text(&out).contains("repaired"), "{}", text(&out));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "café — done\n");
    }

    #[test]
    fn execute_rejects_a_duplicate() {
        let dir = scratch("edit_duplicate");
        let file = dir.join("a.txt");
        write_track(&file, b"x\nx\n");

        let out = Edit.execute(
            &json!({ "path": "a.txt", "old_string": "x", "new_string": "y" }),
            &ctx(Some(dir.clone())),
        );
        assert!(out.is_error);
        assert!(text(&out).contains("2 occurrences"), "{}", text(&out));
    }

    #[test]
    fn execute_requires_a_prior_read() {
        let dir = scratch("edit_guard");
        let file = dir.join("a.txt");
        std::fs::write(&file, b"hello\n").unwrap();
        crate::read::read_map().lock().unwrap().remove(&file);

        let out = Edit.execute(
            &json!({ "path": "a.txt", "old_string": "hello", "new_string": "bye" }),
            &ctx(Some(dir.clone())),
        );
        assert!(out.is_error);
        assert!(text(&out).contains("has not been read"), "{}", text(&out));
    }
}
