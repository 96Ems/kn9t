//! `@path` mentions (PLAN §P7 L2 / D19).
//!
//! Typing `@` anywhere in the prompt opens a fuzzy file dropdown; picking a row replaces
//! the `@token` with `@path`. The agent then reads the file itself — the mention is a fast,
//! unambiguous *reference*, not an embedding (D19).
//!
//! **This is not a second dropdown.** The list, the filtering, the selection and the
//! rendering are the `/` command dropdown's; what differs is the *anchor*: a slash command
//! replaces the whole input (it must start at column 0), a mention replaces one token in the
//! middle of a sentence. Two anchor policies over one completion widget, which is why this
//! module is small and `slash.rs` stays the only place entries are gathered.
//!
//! The anchoring rule is the part worth reading twice: `@` only starts a mention at the
//! start of the input or after whitespace, so `me@example.com` is not a file reference.

use crate::file_index::FileIndex;

/// How many rows the dropdown offers at most.
pub const MENTION_LIMIT: usize = 8;

/// Where a mention token sits in the input, in **char** indices (not bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MentionAnchor {
    /// Index of the `@`.
    pub start: usize,
    /// Index one past the last character of the query — the cursor, in practice.
    pub end: usize,
}

/// Find the mention token the cursor sits in, if any.
///
/// The token runs back from the cursor through non-whitespace characters to an `@`. That
/// `@` must be at the start of the input or preceded by whitespace: an `@` inside a word
/// (an email, a decorator, a handle) is not a file reference and must not hijack the
/// dropdown — which is the difference between a useful feature and one that interrupts
/// every second sentence.
pub fn anchor_at(input: &str, cursor_col: usize) -> Option<MentionAnchor> {
    let chars: Vec<char> = input.chars().collect();
    if chars.is_empty() || cursor_col > chars.len() {
        return None;
    }
    // A mention never spans a line break.
    let mut i = cursor_col;
    while i > 0 {
        let c = chars[i - 1];
        if c.is_whitespace() {
            return None;
        }
        if c == '@' {
            let at = i - 1;
            let preceded_ok = at == 0 || chars[at - 1].is_whitespace();
            return preceded_ok.then_some(MentionAnchor {
                start: at,
                end: cursor_col,
            });
        }
        i -= 1;
    }
    None
}

/// The mention dropdown's state.
#[derive(Debug, Clone, Default)]
pub struct MentionState {
    /// Whether the dropdown is showing.
    pub active: bool,
    /// The token being completed.
    pub anchor: Option<MentionAnchor>,
    /// Matching paths, best first.
    pub matches: Vec<String>,
    /// Selected row in `matches`.
    pub selected: usize,
}

impl MentionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-derive the dropdown from the input and the index.
    ///
    /// Called per keystroke: cheap when inactive (`anchor_at` is a scan of one token), and
    /// when active the search is the index's bounded top-k.
    pub fn sync(&mut self, index: &FileIndex, input: &str, cursor_col: usize) {
        let Some(anchor) = anchor_at(input, cursor_col) else {
            self.deactivate();
            return;
        };
        let query = query_text(input, anchor);
        // `is_empty` here means a bare `@`, which legitimately offers the frecency ranking.
        self.active = true;
        self.anchor = Some(anchor);
        self.matches = index.search(query, MENTION_LIMIT);
        if self.selected >= self.matches.len() {
            self.selected = 0;
        }
    }

    pub fn deactivate(&mut self) {
        self.active = false;
        self.anchor = None;
        self.matches.clear();
        self.selected = 0;
    }

    pub fn selected_path(&self) -> Option<&str> {
        self.matches.get(self.selected).map(String::as_str)
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = if self.selected == 0 {
                self.matches.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    /// Replace the `@token` with `@path`, returning the new cursor column.
    ///
    /// The inserted path has no spaces escaped: paths with spaces are rare enough in a
    /// repository, and inventing an escaping scheme the agent does not know would make the
    /// mention worse than the plain text it replaced.
    pub fn apply(&mut self, input: &mut String, path: &str) -> usize {
        let Some(anchor) = self.anchor else {
            return input.chars().count();
        };
        let chars: Vec<char> = input.chars().collect();
        let start = anchor.start.min(chars.len());
        let end = anchor.end.min(chars.len());
        let before: String = chars[..start].iter().collect();
        let after: String = chars[end..].iter().collect();
        let inserted = format!("@{path}");
        *input = format!("{before}{inserted}{after}");
        self.deactivate();
        before.chars().count() + inserted.chars().count()
    }
}

/// The query text: what follows the `@`, up to the anchor's end.
fn query_text(input: &str, anchor: MentionAnchor) -> &str {
    // Byte-safe: find the byte offset of the anchor by walking chars.
    let mut byte_start = None;
    for (chars_seen, (byte_idx, _)) in input.char_indices().enumerate() {
        if chars_seen == anchor.start + 1 {
            byte_start = Some(byte_idx);
            break;
        }
    }
    let Some(byte_start) = byte_start else {
        return "";
    };
    let mut byte_end = input.len();
    for (chars_seen, (byte_idx, _)) in input.char_indices().enumerate() {
        if chars_seen == anchor.end {
            byte_end = byte_idx;
            break;
        }
    }
    &input[byte_start..byte_end.max(byte_start)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> FileIndex {
        FileIndex::from_paths(
            [
                "crates/kn9t-tui/src/theme.rs",
                "crates/kn9t-tui/src/app.rs",
                "docs/design.md",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
    }

    #[test]
    fn a_bare_at_opens_the_dropdown() {
        let a = anchor_at("@", 1).expect("bare @ anchors");
        assert_eq!(a, MentionAnchor { start: 0, end: 1 });
    }

    #[test]
    fn an_at_after_whitespace_anchors_mid_sentence() {
        let input = "look at @the";
        let a = anchor_at(input, input.chars().count()).expect("anchors after a space");
        assert_eq!(a.start, 8);
        assert_eq!(query_text(input, a), "the");
    }

    #[test]
    fn an_at_inside_a_word_is_not_a_mention() {
        // The email case: hijacking this would interrupt ordinary prose.
        let input = "mail me@example.com";
        assert!(anchor_at(input, input.chars().count()).is_none());

        // A decorator *after* whitespace is a legitimate anchor, though — the rule is
        // "start of input or after whitespace", not "never inside a line".
        let input = "decorator @jit(x)";
        let a = anchor_at(input, 11).expect("anchors just after the @");
        assert_eq!(a.start, 10);
        assert_eq!(query_text(input, a), "");

        // With the cursor at the '@' itself there is no token yet: the character before
        // the cursor is the space, so nothing anchors.
        assert!(anchor_at(input, 10).is_none());
    }

    #[test]
    fn a_token_after_a_space_no_longer_anchors() {
        let input = "@theme and";
        assert!(
            anchor_at(input, input.chars().count()).is_none(),
            "the cursor is past a space, so the mention is finished"
        );
    }

    #[test]
    fn the_query_is_what_follows_the_at_up_to_the_cursor() {
        let input = "read @theme";
        let a = anchor_at(input, input.chars().count()).expect("anchors");
        assert_eq!(query_text(input, a), "theme");

        // Cursor mid-token: the query is the part before it.
        let a = anchor_at(input, 8).expect("anchors");
        assert_eq!(query_text(input, a), "th");
    }

    #[test]
    fn apply_replaces_only_the_token() {
        let index = index();
        let mut state = MentionState::new();
        let mut input = "look at @the now".to_string();
        let cursor = 12; // just after "the"
        state.sync(&index, &input, cursor);
        assert!(state.active, "the dropdown must be live");

        let path = state.selected_path().expect("a match").to_string();
        let new_cursor = state.apply(&mut input, &path);

        assert!(input.starts_with("look at @"), "got {input:?}");
        assert!(input.ends_with(" now"), "the tail must survive: {input:?}");
        assert!(!state.active, "applying closes the dropdown");
        assert_eq!(
            new_cursor,
            "look at @".chars().count() + path.chars().count()
        );
    }

    #[test]
    fn sync_deactivates_when_the_cursor_leaves_the_token() {
        let index = index();
        let mut state = MentionState::new();
        state.sync(&index, "@theme", 6);
        assert!(state.active);

        state.sync(&index, "@theme and more", 15);
        assert!(!state.active, "no anchor past a space");
        assert!(state.matches.is_empty());
    }
}
