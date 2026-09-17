//! The workspace file index (PLAN §P7 L2 / D6).
//!
//! One walk feeds the explorer tree, the file viewer and the `@` dropdown, so the three cannot
//! drift. The TUI owns it: an index needs the filesystem the TUI already sits on, and PLAN §P7
//! draws the line — a view over host state is native, over external data a plugin.
//!
//! The search path is the hot path (a dropdown re-searches on every keystroke), so paths live in
//! two contiguous buffers (`paths` plus an ASCII-lowercased `lower` mirror) with `(start, end)`
//! ranges, scoring allocates nothing per candidate, and top-k selection is a bounded min-heap
//! rather than a full sort per keystroke. ASCII-only lowercasing keeps the paths/lower offsets
//! aligned; a non-ASCII path simply matches case-sensitively.
//!
//! Frecency (hit count + last-used tick) ranks a familiar file above a cold equal match. Ignore
//! rules are a documented subset, not a `.gitignore` implementation — negations and globs are not
//! honoured, and `DEFAULT_SKIPS` covers the rest.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};

/// Directory names never worth indexing, even without a `.gitignore` — otherwise build
/// artifacts flood the tree and dropdown.
pub const DEFAULT_SKIPS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vs",
    "dist",
    "build",
];

/// Walk ceiling, guarding against a symlink loop or pathological tree turning a keystroke into
/// a hang; hitting it is reported via [`FileIndex::truncated`], never silently ignored.
pub const MAX_INDEXED: usize = 200_000;

/// Score added per recorded access, and per recent access. Small enough that match quality
/// still dominates: a bad match used frequently must not outrank a good one.
const FREQ_WEIGHT: i64 = 6;
const RECENCY_WEIGHT: i64 = 10;

/// How many ticks back a use still counts as "recent".
const RECENCY_WINDOW: u32 = 200;

/// The workspace file list, relative to a root; an arena rather than `Vec<String>` (see module docs).
#[derive(Debug, Default, Clone)]
pub struct FileIndex {
    root: Option<PathBuf>,
    /// Every path, concatenated. Ranges index into this.
    paths: String,
    /// ASCII-lowercased mirror of `paths`, byte-for-byte the same length.
    lower: String,
    /// `(start, end)` byte ranges into `paths`/`lower`, sorted by path.
    ranges: Vec<(u32, u32)>,
    /// Access count per file, parallel to `ranges`.
    hits: Vec<u32>,
    /// Tick of the last access per file, 0 when never used.
    last_used: Vec<u32>,
    /// Monotonic clock for `last_used`. Incremented on every recorded access.
    tick: u32,
    /// True when the walk stopped at `MAX_INDEXED`.
    truncated: bool,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The `i`-th path, in sorted order.
    pub fn path_at(&self, i: usize) -> Option<&str> {
        self.ranges.get(i).map(|&(s, e)| &self.paths[s as usize..e as usize])
    }

    /// Every path, in sorted order. Allocates a `Vec` of borrowed slices, cheap but not
    /// free — prefer [`children`](Self::children) or [`search`](Self::search) in a hot path.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        (0..self.ranges.len()).filter_map(|i| self.path_at(i))
    }

    /// Build from an explicit relative-path list instead of a walk (a test, or a
    /// `git ls-files` picker).
    pub fn from_paths<I: IntoIterator<Item = String>>(paths: I) -> Self {
        let mut idx = Self::default();
        let mut sorted: Vec<String> = paths.into_iter().collect();
        sorted.sort();
        idx.install(&sorted, &[]);
        idx
    }

    /// Like [`from_paths`](Self::from_paths) but with a root set, for consumers that compare it
    /// (the explorer resets its expansion set when the workspace changes).
    pub fn from_paths_at<P: Into<PathBuf>, I: IntoIterator<Item = String>>(
        root: P,
        paths: I,
    ) -> Self {
        let mut idx = Self::from_paths(paths);
        idx.root = Some(root.into());
        idx
    }

    /// Fill the arenas from a sorted path list, carrying frecency forward for paths that
    /// still exist.
    fn install(&mut self, sorted: &[String], previous: &[(String, u32, u32)]) {
        self.paths.clear();
        self.lower.clear();
        self.ranges.clear();
        self.hits.clear();
        self.last_used.clear();

        for path in sorted {
            let start = self.paths.len() as u32;
            self.paths.push_str(path);
            let end = self.paths.len() as u32;
            self.ranges.push((start, end));
            // Byte-for-byte the same length as the original, which is what lets one range
            // index both buffers.
            self.lower.push_str(&path.to_ascii_lowercase());

            let (hits, last) = previous
                .iter()
                .find(|(p, _, _)| p == path)
                .map(|(_, h, l)| (*h, *l))
                .unwrap_or((0, 0));
            self.hits.push(hits);
            self.last_used.push(last);
        }
        debug_assert_eq!(self.paths.len(), self.lower.len());
    }

    /// Rebuild for `root` unless the index already covers it; returns true when a walk happened.
    /// Re-indexing per frame would scale cost with the repository.
    pub fn refresh(&mut self, root: &Path) -> bool {
        if self.root.as_deref() == Some(root) && !self.ranges.is_empty() {
            return false;
        }
        self.rebuild(root);
        true
    }

    /// Walk `root` unconditionally, carrying frecency forward for paths that still exist
    /// (losing it would make the ranking flicker).
    pub fn rebuild(&mut self, root: &Path) {
        let mut collected: Vec<String> = Vec::new();
        let mut truncated = false;
        let skips = load_skips(root);

        let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
        while let Some((dir, prefix)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue; // unreadable directory: skip it, do not fail the whole index
            };
            for entry in entries.flatten() {
                if collected.len() >= MAX_INDEXED {
                    truncated = true;
                    break;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let rel = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                if is_skipped(&name, &rel, &skips) {
                    continue;
                }
                // `file_type()` comes from the directory entry on both Windows and Unix, so
                // this is not an extra stat per file.
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    stack.push((entry.path(), rel));
                } else {
                    collected.push(rel);
                }
            }
        }
        collected.sort();

        // Carry frecency across the rebuild by path.
        let previous: Vec<(String, u32, u32)> = self
            .ranges
            .iter()
            .enumerate()
            .map(|(i, _)| {
                (
                    self.path_at(i).unwrap_or_default().to_string(),
                    self.hits.get(i).copied().unwrap_or(0),
                    self.last_used.get(i).copied().unwrap_or(0),
                )
            })
            .collect();

        self.root = Some(root.to_path_buf());
        self.install(&collected, &previous);
        self.truncated = truncated;
    }

    /// Record that a file was opened, for frecency ranking.
    ///
    /// Callers pass the path they handed the user (from [`search`](Self::search) or
    /// [`children`](Self::children)); an unknown path is ignored rather than inserted, since
    /// the index is a snapshot of the walk, not a second source of truth.
    pub fn record_open(&mut self, path: &str) {
        let Some(i) = self
            .ranges
            .iter()
            .position(|&(s, e)| &self.paths[s as usize..e as usize] == path)
        else {
            return;
        };
        self.tick = self.tick.wrapping_add(1);
        self.hits[i] = self.hits[i].saturating_add(1);
        self.last_used[i] = self.tick;
    }

    /// One level under `dir` (empty string = root), derived from the file list so the tree
    /// cannot disagree with the dropdown; an empty directory is simply absent.
    pub fn children(&self, dir: &str) -> Vec<TreeEntry> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir.trim_end_matches('/'))
        };

        let mut seen: Vec<TreeEntry> = Vec::new();
        for i in 0..self.ranges.len() {
            let Some(file) = self.path_at(i) else { continue };
            let Some(rest) = file.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((name, _)) => {
                    if !seen.iter().any(|e| e.is_dir && e.name == name) {
                        seen.push(TreeEntry {
                            name: name.to_string(),
                            path: format!("{prefix}{name}"),
                            is_dir: true,
                        });
                    }
                }
                None => seen.push(TreeEntry {
                    name: rest.to_string(),
                    path: file.to_string(),
                    is_dir: false,
                }),
            }
        }

        // Directories first, then files, each alphabetically: the convention every file
        // explorer uses, and the only order that stays stable as a tree is expanded.
        seen.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        seen
    }

    /// Best fuzzy matches for `query`, best first. An empty query returns the most
    /// frecency-ranked files, so a bare `@` offers what you work on rather than an alphabetical
    /// head; otherwise the path must contain the query as a subsequence.
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        if limit == 0 || self.ranges.is_empty() {
            return Vec::new();
        }
        let needle = query.trim().to_ascii_lowercase();

        if needle.is_empty() {
            // No query: rank by frecency alone. `select` then sort is O(n) for the common
            // case of a limit far smaller than the index.
            let mut ranked: Vec<(i64, usize)> = (0..self.ranges.len())
                .map(|i| (self.frecency(i), i))
                .collect();
            let k = limit.min(ranked.len());
            ranked.select_nth_unstable_by(k.saturating_sub(1), better);
            ranked.truncate(k);
            ranked.sort_by(better);
            return ranked
                .into_iter()
                .filter_map(|(_, i)| self.path_at(i).map(str::to_string))
                .collect();
        }

        // Bounded min-heap: O(n log k), no per-candidate allocation (a full sort was O(n log n)
        // per keystroke).
        let mut best: BinaryHeap<Reverse<(i64, usize)>> = BinaryHeap::with_capacity(limit + 1);
        for i in 0..self.ranges.len() {
            let (s, e) = self.ranges[i];
            let hay = &self.lower[s as usize..e as usize];
            if !is_subsequence(hay, &needle) {
                continue;
            }
            let score = score(hay, &needle) + self.frecency(i);
            if best.len() < limit {
                best.push(Reverse((score, i)));
            } else if let Some(Reverse((worst, _))) = best.peek() {
                if score > *worst {
                    best.pop();
                    best.push(Reverse((score, i)));
                }
            }
        }

        let mut hits: Vec<(i64, usize)> = best.into_iter().map(|Reverse(x)| x).collect();
        hits.sort_by(better);
        hits.into_iter()
            .filter_map(|(_, i)| self.path_at(i).map(str::to_string))
            .collect()
    }

    /// Frecency bonus for file `i`: what you open often, and opened recently.
    fn frecency(&self, i: usize) -> i64 {
        let hits = self.hits.get(i).copied().unwrap_or(0) as i64;
        let last = self.last_used.get(i).copied().unwrap_or(0);
        let age = self.tick.saturating_sub(last);
        let recent = if last > 0 && age < RECENCY_WINDOW {
            // Linear decay across the window, so the most recent use ranks highest.
            (RECENCY_WINDOW - age) as i64 * RECENCY_WEIGHT / RECENCY_WINDOW as i64
        } else {
            0
        };
        hits.min(20) * FREQ_WEIGHT + recent
    }
}

/// One row of the explorer tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Last path component, for display.
    pub name: String,
    /// Full path relative to the index root, `/`-separated.
    pub path: String,
    pub is_dir: bool,
}

/// Best score first; for equal scores the *earlier* path. The tie-break is load-bearing — the
/// obvious descending index compare reverses path order for ties, so the dropdown churned between
/// keystrokes as scores tied and untied.
fn better(a: &(i64, usize), b: &(i64, usize)) -> std::cmp::Ordering {
    b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
}

/// Does `needle` appear in `hay` in order? A cheap byte filter that runs before any scoring.
#[inline]
fn is_subsequence(hay: &str, needle: &str) -> bool {
    let mut chars = hay.bytes();
    needle.bytes().all(|n| chars.any(|h| h == n))
}

/// Score an already-lowercased path against a subsequence needle: a contiguous run in the
/// basename beats scattered hits and unmatched gaps cost, so typing a longer number ranks its
/// file above a shorter prefix that happens to have a shorter path.
fn score(hay: &str, needle: &str) -> i64 {
    let base_start = hay.rfind('/').map(|i| i + 1).unwrap_or(0);
    let base = &hay[base_start..];

    let mut total: i64 = 0;
    let mut hay_iter = hay.bytes().enumerate();
    let mut prev: Option<usize> = None;
    let mut run: i64 = 0;
    let mut max_run: i64 = 0;
    let mut gaps: i64 = 0;

    for n in needle.bytes() {
        let Some((idx, _)) = hay_iter.find(|(_, h)| *h == n) else {
            break;
        };
        if prev.map(|p| p + 1 == idx).unwrap_or(false) {
            run += 1; // contiguous with the previous match
        } else {
            if prev.is_some() {
                gaps += 1;
            }
            run = 1;
        }
        max_run = max_run.max(run);
        if idx >= base_start {
            total += 6; // a basename hit beats a parent-directory hit
        }
        prev = Some(idx);
    }

    total += max_run * 5 - gaps * 3;

    if base.contains(needle) {
        total += 40;
    }
    if base == needle {
        total += 60;
    }
    // Prefer shorter paths, so `theme` picks `theme.rs` over `docs/theme/notes.md`.
    total - hay.len() as i64 / 4
}

/// Skip rules: the built-in list plus the root `.gitignore`'s plain names.
///
/// Parsed once per walk rather than once per entry.
fn load_skips(root: &Path) -> Vec<String> {
    let mut skips: Vec<String> = DEFAULT_SKIPS.iter().map(|s| s.to_string()).collect();
    let Ok(text) = std::fs::read_to_string(root.join(".gitignore")) else {
        return skips;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue; // comments, and negations are out of the supported subset
        }
        let name = line.trim_end_matches('/').trim_start_matches('/');
        if name.is_empty() || name.contains('*') || name.contains('?') || name.contains('/') {
            continue;
        }
        skips.push(name.to_string());
    }
    skips
}

fn is_skipped(name: &str, rel: &str, skips: &[String]) -> bool {
    skips.iter().any(|s| s == name || s == rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index built directly, without touching the filesystem.
    fn index_of(files: &[&str]) -> FileIndex {
        let mut idx = FileIndex::from_paths(files.iter().map(|s| s.to_string()));
        idx.root = Some(PathBuf::from("/w"));
        idx
    }

    /// A large synthetic index, for the bounded-search guard.
    fn big_index(n: usize) -> FileIndex {
        let files: Vec<String> = (0..n)
            .map(|i| format!("crates/crate{i}/src/module{i}/impl_{i}.rs"))
            .collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        index_of(&refs)
    }

    #[test]
    fn the_lowercase_mirror_stays_offset_aligned() {
        // The whole arena design rests on this: one range indexes both buffers.
        let idx = index_of(&["Crates/Theme.RS", "docs/ADR/0001.md"]);
        assert_eq!(idx.paths.len(), idx.lower.len());
        for &(s, e) in &idx.ranges {
            let original = &idx.paths[s as usize..e as usize];
            let lower = &idx.lower[s as usize..e as usize];
            assert_eq!(original.to_ascii_lowercase(), lower);
        }
    }

    #[test]
    fn children_lists_directories_before_files() {
        let idx = index_of(&[
            "README.md",
            "crates/kn9t-tui/src/app.rs",
            "crates/kn9t-tui/src/lib.rs",
            "docs/adr/0001.md",
        ]);

        let root = idx.children("");
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["crates", "docs", "README.md"]);
        assert!(root[0].is_dir && root[0].path == "crates");
        assert!(!root[2].is_dir && root[2].path == "README.md");

        let src = idx.children("crates/kn9t-tui/src");
        let names: Vec<&str> = src.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["app.rs", "lib.rs"], "one level only");
        assert_eq!(src[0].path, "crates/kn9t-tui/src/app.rs");
    }

    #[test]
    fn children_are_derived_from_the_file_list() {
        // A directory that exists but holds no indexed file is absent: the tree shows what
        // can be opened, not what happens to be on disk.
        let idx = index_of(&["a/b.txt"]);
        assert!(idx.children("a/nope").is_empty());
        assert_eq!(idx.children("a").len(), 1);
    }

    #[test]
    fn search_prefers_the_basename_and_shorter_paths() {
        let idx = index_of(&[
            "docs/theme/notes.md",
            "crates/kn9t-tui/src/theme.rs",
            "crates/kn9t-tui/src/syntax.rs",
        ]);
        let hits = idx.search("theme", 5);
        assert_eq!(
            hits.first().map(|s| s.as_str()),
            Some("crates/kn9t-tui/src/theme.rs"),
            "the file whose *basename* is theme ranks first, got {hits:?}"
        );
    }

    #[test]
    fn search_is_a_subsequence_match_over_the_whole_path() {
        let idx = index_of(&["crates/kn9t-tui/src/app.rs", "docs/adr/0001.md"]);
        // The characters appear in order across directory boundaries, which is the point of
        // fuzzy path matching: you remember fragments, not the full path.
        assert_eq!(idx.search("ktsrc", 5), vec!["crates/kn9t-tui/src/app.rs"]);
    }

    #[test]
    fn search_is_case_insensitive_and_never_returns_a_non_match() {
        let idx = index_of(&["Crates/Kn9t-Tui/Src/Theme.RS", "abc.rs"]);
        assert_eq!(idx.search("THEME", 5).len(), 1);
        assert!(idx.search("zzz", 5).is_empty());
        assert!(idx.search("x", 0).is_empty(), "limit 0 returns nothing");
    }

    #[test]
    fn a_query_cannot_match_across_two_files() {
        // The subsequence test is per path: `ab` must not be satisfied by `a.rs` + `b.rs`.
        let idx = index_of(&["a.rs", "b.rs"]);
        assert!(idx.search("ab", 5).is_empty());
    }

    #[test]
    fn frecency_ranks_what_you_open_often_and_recently() {
        let mut idx = index_of(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        // No query: the ranking is frecency alone, and ties keep path order.
        assert_eq!(
            idx.search("", 3),
            vec!["src/a.rs", "src/b.rs", "src/c.rs"],
            "with nothing opened, the order is the sorted path order"
        );

        for _ in 0..5 {
            idx.record_open("src/c.rs");
        }
        assert_eq!(
            idx.search("", 3).first().map(String::as_str),
            Some("src/c.rs")
        );

        // Frequency is not outranked by a single more recent use: a file you open every day
        // stays above one you touched once, which is the point of ranking by access at all.
        idx.record_open("src/b.rs");
        assert_eq!(
            idx.search("", 3).first().map(String::as_str),
            Some("src/c.rs"),
            "one recent use must not displace a frequent file"
        );

        // Recency decides between *equally* frequent files.
        for _ in 0..4 {
            idx.record_open("src/b.rs");
        }
        assert_eq!(
            idx.search("", 3).first().map(String::as_str),
            Some("src/b.rs"),
            "at equal frequency, the most recently used wins"
        );

        // An unknown path is ignored, not inserted: the index is a snapshot of the walk.
        idx.record_open("nope.rs");
        assert_eq!(idx.len(), 3);
    }

    #[test]
    fn frecency_never_beats_match_quality() {
        let mut idx = index_of(&["src/theme.rs", "vendor/other/long/path/theme_helper_name.rs"]);
        for _ in 0..30 {
            idx.record_open("vendor/other/long/path/theme_helper_name.rs");
        }
        // The exact basename match must still win: a file you open often is not a file you
        // meant when you typed its exact name elsewhere.
        let hits = idx.search("theme.rs", 5);
        assert_eq!(hits.first().map(String::as_str), Some("src/theme.rs"), "got {hits:?}");
    }

    #[test]
    fn bounded_search_returns_the_best_k_out_of_many() {
        // The top-k heap must not simply return the first k candidates it saw.
        let idx = big_index(2_000);
        let hits = idx.search("module1999", 5);
        assert!(
            hits[0].contains("module1999"),
            "the exact match must rank first, got {hits:?}"
        );
        // `module1999` is also a legitimate *subsequence* of `module1993`, so a fuzzy
        // search returning only one hit would mean the matcher had been restricted to
        // substrings — which is the fzf behaviour this is not.
        assert!(hits.len() > 1, "subsequence matches must survive, got {hits:?}");

        let hits = idx.search("impl", 10);
        assert_eq!(hits.len(), 10, "the limit is honoured");
    }

    /// A gross-regression guard, not a benchmark: the bound is deliberately loose and only
    /// catches the catastrophic case (re-walking or re-lowercasing per keystroke).
    #[test]
    fn searching_a_large_index_stays_interactive() {
        let idx = big_index(50_000);
        let start = std::time::Instant::now();
        for q in ["ktsrc", "module1234", "impl", "theme", "x"] {
            let _ = idx.search(q, 20);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 2_000,
            "5 searches over 50k files took {elapsed:?}, which means the hot path allocates \
             or sorts per candidate"
        );
    }

    #[test]
    fn a_walk_indexes_files_and_respects_the_built_in_skips() {
        // A dedicated parent, so the "new root walks" assertion cannot accidentally walk all of
        // `%TEMP%` (it made this test take eight seconds).
        let parent = std::env::temp_dir().join(format!("kn9t-index-{}", std::process::id()));
        let root = parent.join("w");
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        std::fs::create_dir_all(root.join("target/debug")).expect("mkdir target");
        std::fs::create_dir_all(root.join("node_modules/x")).expect("mkdir node_modules");
        std::fs::write(root.join("src/main.rs"), "fn main() {}").expect("write");
        std::fs::write(root.join("README.md"), "hi").expect("write");
        std::fs::write(root.join("target/debug/junk"), "junk").expect("write");
        std::fs::write(root.join("node_modules/x/idx.js"), "x").expect("write");

        let mut idx = FileIndex::new();
        idx.rebuild(&root);

        let mut found: Vec<&str> = idx.paths().collect();
        found.sort();
        assert_eq!(
            found,
            vec!["README.md", "src/main.rs"],
            "build artifacts must not be indexed"
        );
        assert!(!idx.truncated());
        // Paths use `/` on every platform — they are keys for Lua and `@` mentions, where `\` escapes.
        assert!(idx.paths().all(|f| !f.contains('\\')));

        // Frecency survives a rebuild.
        idx.record_open("src/main.rs");
        idx.rebuild(&root);
        assert_eq!(idx.search("", 1).first().map(String::as_str), Some("src/main.rs"));

        // Re-indexing the same root is a no-op.
        assert!(!idx.refresh(&root), "same root must not walk again");
        assert!(idx.refresh(&parent), "new root walks");

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_root_gitignore_contributes_plain_names() {
        let root = std::env::temp_dir().join(format!("kn9t-gitignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("logs")).expect("mkdir logs");
        std::fs::create_dir_all(root.join("keep")).expect("mkdir keep");
        std::fs::write(root.join("logs/a.txt"), "x").expect("write");
        std::fs::write(root.join("keep/b.txt"), "x").expect("write");
        // A negation and a glob: neither is in the supported subset, and both must be
        // ignored rather than half-applied.
        std::fs::write(root.join(".gitignore"), "# comment\nlogs/\n!keep\n*.tmp\n")
            .expect("write");

        let mut idx = FileIndex::new();
        idx.rebuild(&root);
        let found: Vec<&str> = idx.paths().collect();
        assert_eq!(
            found,
            vec![".gitignore", "keep/b.txt"],
            "logs/ is ignored; .gitignore itself is a file like any other"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
