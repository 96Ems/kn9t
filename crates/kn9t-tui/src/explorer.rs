//! The file explorer column (PLAN §P7 L2 / D3).
//!
//! One view over the same `FileIndex` backing the `@` dropdown (D6): the index owns the walk,
//! this owns tree shape (expanded directories, selected row). Rendering is native
//! (`ui::render::render_explorer`), so the index is never serialised into Lua.
//!
//! Focus is explicit: F1 shows/focuses, Esc returns the keyboard without closing the column.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::file_index::FileIndex;

/// One visible row: a directory or a file, with its indent depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerRow {
    pub name: String,
    /// Path relative to the index root, `/`-separated.
    pub path: String,
    pub is_dir: bool,
    pub depth: usize,
}

/// What activating the selected row did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activate {
    /// Nothing under the cursor.
    None,
    /// A directory's expansion changed; the caller must rebuild the rows.
    Toggled,
    /// A file was chosen; the caller opens it in the viewer (D4).
    File(String),
}

/// The explorer's state: visibility, focus, expansion and selection.
#[derive(Debug, Default)]
pub struct ExplorerState {
    visible: bool,
    focused: bool,
    /// Directories whose children are shown, by relative path.
    expanded: BTreeSet<String>,
    /// Selected index into `rows`.
    pub selected: usize,
    /// First visible row, adjusted by the renderer to keep the selection on screen.
    pub offset: usize,
    rows: Vec<ExplorerRow>,
    /// Set when `expanded`, the index root, or the index contents changed, so the next sync
    /// flattens once.
    dirty: bool,
    root: Option<PathBuf>,
    /// Index generation the `rows` were built from, so a rebuild in place (a file created or
    /// removed under an unchanged root) is noticed.
    generation: u64,
}

impl ExplorerState {
    /// Visible by default, but not focused: the column is useful to see, and it must not steal
    /// the keyboard from the prompt until the user engages it.
    pub fn new() -> Self {
        Self {
            visible: true,
            focused: false,
            dirty: true,
            ..Self::default()
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// `F1`: hidden → shown and focused; shown but unfocused → focused; focused → hidden.
    ///
    /// Returns the new visibility.
    pub fn toggle(&mut self) -> bool {
        if !self.visible {
            self.visible = true;
            self.focused = true;
            self.dirty = true;
        } else if !self.focused {
            self.focused = true;
        } else {
            self.visible = false;
            self.focused = false;
        }
        self.visible
    }

    /// Hide the column and release the keyboard.
    pub fn close(&mut self) {
        self.visible = false;
        self.focused = false;
    }

    /// Hand the keyboard back to the prompt without closing the column.
    pub fn blur(&mut self) {
        self.focused = false;
    }

    /// Give the column the keyboard (used when it is clicked).
    pub fn focus(&mut self) {
        self.visible = true;
        self.focused = true;
        self.dirty = true;
    }

    pub fn rows(&self) -> &[ExplorerRow] {
        &self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn selected_row(&self) -> Option<&ExplorerRow> {
        self.rows.get(self.selected)
    }

    /// The selected row when it is a file — the path a mention or the viewer wants.
    pub fn selected_file(&self) -> Option<&str> {
        self.selected_row()
            .filter(|r| !r.is_dir)
            .map(|r| r.path.as_str())
    }

    /// Whether a directory is currently expanded (for the chevron).
    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    /// Re-flatten the tree if the index root, its contents, or the expansion set changed.
    ///
    /// Cheap enough to call from the key path: it does nothing unless something changed.
    pub fn sync(&mut self, index: &FileIndex) {
        let root = index.root().map(PathBuf::from);
        if root != self.root {
            // A different workspace: the old expansion set is meaningless.
            self.root = root;
            self.expanded.clear();
            self.selected = 0;
            self.offset = 0;
            self.dirty = true;
        }
        // A rebuild in place keeps the root but replaces every path: the watcher-driven
        // refresh must reach the rows, or the tree stays a snapshot of the first walk.
        if self.generation != index.generation() {
            self.dirty = true;
        }
        if self.dirty {
            self.rebuild(index);
        }
    }

    fn rebuild(&mut self, index: &FileIndex) {
        let mut rows = Vec::new();
        flatten(index, &self.expanded, "", 0, &mut rows);
        self.rows = rows;
        self.generation = index.generation();
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.dirty = false;
    }

    pub fn select_next(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1).min(self.rows.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.rows.len().saturating_sub(1);
    }

    /// Select a row by index, clamping to the list.
    pub fn select(&mut self, index: usize) {
        self.selected = index.min(self.rows.len().saturating_sub(1));
    }

    /// Expand/collapse a directory, or choose a file.
    pub fn activate(&mut self, index: &FileIndex) -> Activate {
        let Some(row) = self.rows.get(self.selected) else {
            return Activate::None;
        };
        if row.is_dir {
            let path = row.path.clone();
            if !self.expanded.remove(&path) {
                self.expanded.insert(path);
            }
            self.dirty = true;
            self.rebuild(index);
            Activate::Toggled
        } else {
            Activate::File(row.path.clone())
        }
    }

    /// Left: collapse an open directory, otherwise move to the parent row.
    pub fn collapse_or_parent(&mut self, index: &FileIndex) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            return;
        };
        if row.is_dir && self.expanded.contains(&row.path) {
            self.expanded.remove(&row.path);
            self.dirty = true;
            self.rebuild(index);
            return;
        }
        // Walk back to the nearest row one level shallower: its parent directory.
        if row.depth == 0 {
            return;
        }
        if let Some(parent) = self.rows[..self.selected]
            .iter()
            .rposition(|r| r.depth < row.depth)
        {
            self.selected = parent;
        }
    }

    /// Right: open a collapsed directory.
    pub fn expand_only(&mut self, index: &FileIndex) {
        if let Some(row) = self.rows.get(self.selected) {
            if row.is_dir && !self.expanded.contains(&row.path) {
                self.expanded.insert(row.path.clone());
                self.dirty = true;
                self.rebuild(index);
            }
        }
    }
}

/// Flatten `dir`'s children, recursing into the expanded ones.
fn flatten(
    index: &FileIndex,
    expanded: &BTreeSet<String>,
    dir: &str,
    depth: usize,
    out: &mut Vec<ExplorerRow>,
) {
    for entry in index.children(dir) {
        let is_dir = entry.is_dir;
        out.push(ExplorerRow {
            name: entry.name,
            path: entry.path.clone(),
            is_dir,
            depth,
        });
        if is_dir && expanded.contains(&entry.path) {
            flatten(index, expanded, &entry.path, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> FileIndex {
        FileIndex::from_paths_at(
            "/w",
            [
                "README.md",
                "crates/kn9t-tui/src/app.rs",
                "crates/kn9t-tui/src/lib.rs",
                "docs/adr/0001.md",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
    }

    fn paths(state: &ExplorerState) -> Vec<String> {
        state.rows().iter().map(|r| r.path.clone()).collect()
    }

    #[test]
    fn the_tree_starts_collapsed_at_the_root() {
        let mut explorer = ExplorerState::new();
        explorer.sync(&index());
        assert_eq!(
            paths(&explorer),
            vec!["crates", "docs", "README.md"],
            "directories first, one level only"
        );
        assert!(explorer.selected_row().is_some());
        assert_eq!(explorer.selected_row().unwrap().depth, 0);
    }

    #[test]
    fn activating_a_directory_expands_it_in_place() {
        let mut explorer = ExplorerState::new();
        let idx = index();
        explorer.sync(&idx);

        assert_eq!(explorer.activate(&idx), Activate::Toggled);
        assert_eq!(
            paths(&explorer),
            vec!["crates", "crates/kn9t-tui", "docs", "README.md"],
            "the children are inserted directly under their directory"
        );
        assert_eq!(explorer.selected_row().unwrap().path, "crates");
        assert_eq!(explorer.rows()[1].depth, 1);

        // Collapsing removes them again.
        assert_eq!(explorer.activate(&idx), Activate::Toggled);
        assert_eq!(paths(&explorer), vec!["crates", "docs", "README.md"]);
    }

    #[test]
    fn left_collapses_then_walks_to_the_parent() {
        let mut explorer = ExplorerState::new();
        let idx = index();
        explorer.sync(&idx);
        for dir in ["crates", "crates/kn9t-tui", "crates/kn9t-tui/src"] {
            let i = explorer
                .rows()
                .iter()
                .position(|r| r.path == dir)
                .unwrap_or_else(|| panic!("{dir} visible after expanding its parent"));
            explorer.select(i);
            explorer.activate(&idx);
        }

        // Move onto the first file inside the expanded directory.
        let file_idx = explorer
            .rows()
            .iter()
            .position(|r| r.path == "crates/kn9t-tui/src/app.rs")
            .expect("the file is visible");
        explorer.select(file_idx);
        assert_eq!(explorer.selected_row().unwrap().depth, 3);

        // Left on a file jumps to its parent directory, not out of the tree.
        explorer.collapse_or_parent(&idx);
        assert_eq!(explorer.selected_row().unwrap().path, "crates/kn9t-tui/src");
        // Left again collapses that directory.
        explorer.collapse_or_parent(&idx);
        assert!(!explorer.is_expanded("crates/kn9t-tui/src"));
    }

    #[test]
    fn activating_a_file_returns_its_path() {
        let mut explorer = ExplorerState::new();
        let idx = index();
        explorer.sync(&idx);
        let readme = explorer
            .rows()
            .iter()
            .position(|r| r.path == "README.md")
            .expect("README is at the root");
        explorer.select(readme);
        assert_eq!(
            explorer.activate(&idx),
            Activate::File("README.md".to_string())
        );
        assert_eq!(explorer.selected_file(), Some("README.md"));
    }

    #[test]
    fn selection_is_clamped_to_the_list() {
        let mut explorer = ExplorerState::new();
        let idx = index();
        explorer.sync(&idx);
        explorer.select(999);
        assert!(explorer.selected < explorer.rows().len());
        assert!(explorer.selected_row().is_some(), "always a valid row");

        // A collapse that removes rows must leave the selection valid too.
        explorer.select(0);
        explorer.activate(&idx); // expand "crates"
        explorer.select_last();
        explorer.select(0);
        explorer.activate(&idx); // collapse it again
        assert!(explorer.selected < explorer.rows().len());
    }

    #[test]
    fn a_changed_index_refreshes_the_rows_without_a_root_change() {
        let root = std::env::temp_dir().join(format!("kn9t-explorer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("a.rs"), "a").expect("write");

        let mut idx = FileIndex::new();
        idx.rebuild(&root);

        let mut explorer = ExplorerState::new();
        explorer.sync(&idx);
        assert_eq!(paths(&explorer), vec!["a.rs"]);

        // A file appears under the same root, as the workspace watcher would report it.
        std::fs::write(root.join("b.rs"), "b").expect("write");
        idx.rebuild(&root);
        explorer.sync(&idx);
        assert_eq!(
            paths(&explorer),
            vec!["a.rs", "b.rs"],
            "the tree must follow the index, not just its root"
        );

        // A file disappears: the rows shrink too.
        std::fs::remove_file(root.join("a.rs")).expect("remove");
        idx.rebuild(&root);
        explorer.sync(&idx);
        assert_eq!(paths(&explorer), vec!["b.rs"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn f1_cycles_show_focus_hide() {
        let mut explorer = ExplorerState::new();
        assert!(explorer.is_visible(), "visible by default");
        assert!(!explorer.is_focused(), "but it must not own the keyboard");

        assert!(explorer.toggle(), "visible+unfocused → focused");
        assert!(explorer.is_visible() && explorer.is_focused());

        assert!(!explorer.toggle(), "focused → hidden");
        assert!(!explorer.is_visible() && !explorer.is_focused());

        assert!(explorer.toggle(), "hidden → visible+focused");
        assert!(explorer.is_visible() && explorer.is_focused());
    }
}
