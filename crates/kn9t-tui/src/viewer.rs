//! The read-only file viewer (PLAN §P7 L2 / D4/D5).
//!
//! Choosing a file in the explorer opens it here, stacked above the transcript (the placement a
//! `placement="main"` plugin view uses, D5). Deliberately not an editor — the TUI's editor is the
//! conversation (D1). A file is capped at [`MAX_VIEWER_BYTES`] / [`MAX_VIEWER_LINES`], and hitting
//! the cap is stated in the header rather than silently truncating.

use std::path::Path;

/// Largest file the viewer will open, in bytes.
pub const MAX_VIEWER_BYTES: u64 = 1 << 20;
/// Largest number of lines kept, so the scroll maths stay cheap.
pub const MAX_VIEWER_LINES: usize = 5_000;

/// An open file: its raw lines, its syntax token, and the scroll position.
#[derive(Debug, Clone)]
pub struct ViewerState {
    /// Path as shown and mentioned — relative to the workspace root.
    path: String,
    /// Syntax token derived from the extension, when it has one.
    lang: Option<String>,
    lines: Vec<String>,
    /// First visible line.
    pub scroll: usize,
    /// Line the cursor sits on (0-based), for `c` (comment) and `v` (select).
    pub cursor: usize,
    /// Range anchor; `Some` while a selection is open.
    select_anchor: Option<usize>,
    /// Whether the viewer owns the keyboard.
    pub focused: bool,
    truncated: bool,
}

impl ViewerState {
    /// Open `rel` under `root`, or return the message to show in its place.
    pub fn open(root: &Path, rel: &str) -> Result<Self, String> {
        let abs = root.join(rel);
        let meta = std::fs::metadata(&abs).map_err(|e| format!("{rel}: {e}"))?;
        if meta.len() > MAX_VIEWER_BYTES {
            return Err(format!(
                "{rel}: {} KiB exceeds the {} KiB viewer cap",
                meta.len() / 1024,
                MAX_VIEWER_BYTES / 1024
            ));
        }
        let text = std::fs::read_to_string(&abs).map_err(|e| format!("{rel}: {e}"))?;
        if text.contains('\0') {
            return Err(format!("{rel}: binary file"));
        }

        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        let truncated = lines.len() > MAX_VIEWER_LINES;
        if truncated {
            lines.truncate(MAX_VIEWER_LINES);
        }

        Ok(Self {
            path: rel.to_string(),
            lang: lang_for(rel),
            lines,
            scroll: 0,
            cursor: 0,
            select_anchor: None,
            // Opened from the explorer, so the user is reading it: take the keyboard.
            focused: true,
            truncated,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn lang(&self) -> Option<&str> {
        self.lang.as_deref()
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_bottom(&mut self) {
        self.scroll = usize::MAX;
    }

    pub fn focus(&mut self) {
        self.focused = true;
    }

    pub fn blur(&mut self) {
        self.focused = false;
    }

    pub fn set_cursor(&mut self, line: usize) {
        self.cursor = line.min(self.lines.len().saturating_sub(1));
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.lines.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    /// Open a selection at the cursor, or clear it.
    pub fn toggle_selection(&mut self) {
        self.select_anchor = match self.select_anchor {
            Some(_) => None,
            None => Some(self.cursor),
        };
    }

    /// Selected line range, 1-based and inclusive, when a selection is open.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.select_anchor?;
        let (lo, hi) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        Some((lo + 1, hi + 1))
    }

    /// The `@path:L` / `@path:L1-L2` reference inserted into the prompt.
    pub fn reference(&self) -> String {
        match self.selection() {
            Some((lo, hi)) if lo != hi => format!("@{}:{}-{}", self.path, lo, hi),
            _ => format!("@{}:{}", self.path, self.cursor + 1),
        }
    }

    /// The header text: path, line count, and any truncation.
    pub fn title(&self) -> String {
        let mut t = format!(" {} ", self.path);
        if self.truncated {
            t.push_str(&format!("· first {MAX_VIEWER_LINES} lines "));
        } else {
            t.push_str(&format!("· {} lines ", self.line_count()));
        }
        t
    }
}

/// Syntax token for a path's extension, when it is one the highlighter knows.
fn lang_for(path: &str) -> Option<String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    let token = match ext.as_str() {
        "rs" => "rust",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "lua" => "lua",
        "sh" | "bash" => "bash",
        "c" => "c",
        "h" | "hpp" | "cpp" | "cc" | "cxx" => "cpp",
        "css" => "css",
        "html" | "htm" => "html",
        "xml" => "xml",
        "sql" => "sql",
        "java" => "java",
        "rb" => "ruby",
        _ => return None,
    };
    Some(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kn9t-viewer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write temp file");
        path
    }

    #[test]
    fn the_language_comes_from_the_extension() {
        assert_eq!(lang_for("src/app.rs").as_deref(), Some("rust"));
        assert_eq!(lang_for("tui/00_theme.lua").as_deref(), Some("lua"));
        assert_eq!(lang_for("Cargo.toml").as_deref(), Some("toml"));
        assert_eq!(lang_for("README"), None, "no extension, no highlighting");
    }

    #[test]
    fn opening_a_file_reads_its_lines() {
        let root = std::env::temp_dir().join(format!("kn9t-viewer-{}", std::process::id()));
        temp_file("a.rs", "fn main() {}\nlet x = 1;\n");
        let v = ViewerState::open(&root, "a.rs").expect("opens");
        assert_eq!(v.line_count(), 2);
        assert_eq!(v.lang(), Some("rust"));
        assert!(!v.is_truncated());
        assert!(v.title().contains("2 lines"));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let root = std::env::temp_dir().join(format!("kn9t-viewer-{}", std::process::id()));
        assert!(ViewerState::open(&root, "nope.rs").is_err());
    }

    #[test]
    fn a_reference_names_one_line_or_a_range() {
        let root = std::env::temp_dir().join(format!("kn9t-viewer-ref-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create temp dir");
        std::fs::write(root.join("ref.rs"), "l1\nl2\nl3\nl4\n").expect("write temp file");
        let mut v = ViewerState::open(&root, "ref.rs").expect("opens");

        assert_eq!(v.reference(), "@ref.rs:1", "the cursor line");
        v.move_cursor(2);
        assert_eq!(v.reference(), "@ref.rs:3");
        v.toggle_selection();
        v.move_cursor(-1);
        assert_eq!(
            v.reference(),
            "@ref.rs:2-3",
            "the range spans anchor..cursor"
        );
        v.toggle_selection();
        assert_eq!(v.reference(), "@ref.rs:2", "clearing drops the range");
    }

    #[test]
    fn scroll_is_clamped_at_the_top() {
        let mut v = ViewerState {
            path: "a".into(),
            lang: None,
            lines: vec!["x".into()],
            scroll: 0,
            cursor: 0,
            select_anchor: None,
            focused: false,
            truncated: false,
        };
        v.scroll_up(5);
        assert_eq!(v.scroll, 0);
        v.scroll_down(3);
        assert_eq!(v.scroll, 3);
    }
}
