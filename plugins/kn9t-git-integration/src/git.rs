//! Runs `git` and parses its output into a small, TUI-friendly shape.
//!
//! Kept free of any plugin-protocol concerns (no `ToolCallCtx`, no JSON
//! serialization here) so the parsing can be unit tested against captured
//! `git` output without spawning a process or a plugin runtime.

use std::path::Path;
use std::process::Command;

/// One line from `git status --porcelain=v2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// "M" (modified), "A" (added), "D" (deleted), "R" (renamed), "?" (untracked), ...
    pub status: String,
    pub path: String,
}

/// One line from `git log --oneline`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub sha: String,
    pub subject: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitState {
    /// `None` when `cwd` is not inside a git repository at all — distinct
    /// from `Some(GitState::default())`, which is a clean repo with no
    /// history yet. The tool/UI need to tell these apart to avoid claiming
    /// "clean" for a directory that was never a repo.
    pub branch: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub changes: Vec<FileChange>,
    pub recent: Vec<LogEntry>,
}

/// How many `git log` entries to keep. Bounded so the sidebar (and the
/// `ui_set_state` payload) cannot grow with repository age.
pub const LOG_LIMIT: usize = 12;

/// Collect git state for the repository containing `cwd`, or `None` if
/// `cwd` is not inside a git repository (or `git` is not on PATH).
pub fn collect(cwd: &Path) -> Option<GitState> {
    let branch_out = run_git(cwd, &["status", "--branch", "--porcelain=v2"])?;
    let (branch, ahead, behind, changes) = parse_status(&branch_out);
    // `run_git` already returns `None` on a non-zero exit (the real case for
    // "not a repo" — `git status` there exits 128). This second check is a
    // narrower defense: `git` exiting 0 with output that has no
    // `# branch.head` line, which should not happen with a real `git` but
    // costs nothing to guard against.
    branch.as_ref()?;

    let log_out = run_git(
        cwd,
        &["log", &format!("-{LOG_LIMIT}"), "--pretty=format:%h\x1f%s"],
    )
    .unwrap_or_default();
    let recent = parse_log(&log_out);

    Some(GitState {
        branch,
        ahead,
        behind,
        changes,
        recent,
    })
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Like [`run_git`] but tolerant of a non-zero exit.
///
/// `git diff` exits non-zero in normal, non-error situations (notably with
/// `--exit-code` semantics in some configs), and its stdout is still the diff.
/// Treating that as failure would silently show an empty review panel.
///
/// Uses `from_utf8_lossy`: a diff of a file with mixed encodings must degrade
/// to replacement characters rather than discarding the whole hunk.
pub fn run_git_raw(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse `git status --branch --porcelain=v2` output.
///
/// Format reference (porcelain v2, not the human-readable default):
///   `# branch.head <name>`
///   `# branch.ab +<ahead> -<behind>`
///   `1 <xy> ... <path>`        (ordinary changed entry)
///   `2 <xy> ... <path>\t<orig>` (renamed/copied entry — path is after the tab)
///   `? <path>`                  (untracked)
fn parse_status(out: &str) -> (Option<String>, u32, u32, Vec<FileChange>) {
    let mut branch = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut changes = Vec::new();

    for line in out.lines() {
        if let Some(name) = line.strip_prefix("# branch.head ") {
            branch = Some(name.to_string());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            // "+N -M"
            for part in ab.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(rest) = line.strip_prefix("1 ") {
            if let Some(change) = parse_ordinary(rest) {
                changes.push(change);
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            if let Some(change) = parse_renamed(rest) {
                changes.push(change);
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            changes.push(FileChange {
                status: "?".to_string(),
                path: path.to_string(),
            });
        }
        // "u " (unmerged) and other line kinds are ignored — a sidebar summary
        // does not need conflict-resolution detail.
    }

    (branch, ahead, behind, changes)
}

/// `<xy> <sub> <mH> <mI> <mW> <hH> <hI> <path>` — we only need `xy` and the
/// trailing path, which is always the last whitespace-separated field for an
/// ordinary (non-renamed) entry.
fn parse_ordinary(rest: &str) -> Option<FileChange> {
    let mut parts = rest.splitn(8, ' ');
    let xy = parts.next()?;
    let path = parts.last()?;
    Some(FileChange {
        status: xy_summary(xy),
        path: path.to_string(),
    })
}

/// Renamed/copied entries carry `<origPath>\t<newPath>`-style suffix after
/// the fixed fields; the path we display is the new one, before the tab.
fn parse_renamed(rest: &str) -> Option<FileChange> {
    let mut parts = rest.splitn(9, ' ');
    let xy = parts.next()?;
    let tail = parts.last()?;
    let path = tail.split('\t').next()?;
    Some(FileChange {
        status: xy_summary(xy),
        path: path.to_string(),
    })
}

/// Porcelain v2's two-character XY code, reduced to the single letter a
/// sidebar actually wants to show. Staged (X) takes priority over unstaged
/// (Y) for display purposes — "modified and staged" is still "modified".
fn xy_summary(xy: &str) -> String {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');
    let letter = if x != '.' { x } else { y };
    letter.to_string()
}

fn parse_log(out: &str) -> Vec<LogEntry> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\u{1f}');
            let sha = parts.next()?.to_string();
            let subject = parts.next().unwrap_or("").to_string();
            if sha.is_empty() {
                return None;
            }
            Some(LogEntry { sha, subject })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_extracts_branch_and_ahead_behind() {
        let out = "# branch.oid abc123\n# branch.head main\n# branch.ab +2 -1\n";
        let (branch, ahead, behind, changes) = parse_status(out);
        assert_eq!(branch, Some("main".to_string()));
        assert_eq!(ahead, 2);
        assert_eq!(behind, 1);
        assert!(changes.is_empty());
    }

    #[test]
    fn parse_status_with_no_branch_head_returns_none() {
        // What `collect` sees when `cwd` is not a git repo at all: git exits
        // non-zero, so `run_git` returns None before this function even
        // runs — but if it somehow got empty/unrelated output, there must be
        // no branch to distinguish "not a repo" from "clean repo".
        let (branch, _, _, _) = parse_status("");
        assert_eq!(branch, None);
    }

    #[test]
    fn parse_status_ordinary_modified_file() {
        let out = "1 .M N... 100644 100644 100644 abc123 def456 src/main.rs\n";
        let (_, _, _, changes) = parse_status(out);
        assert_eq!(
            changes,
            vec![FileChange {
                status: "M".to_string(),
                path: "src/main.rs".to_string(),
            }]
        );
    }

    #[test]
    fn parse_status_staged_takes_priority_over_unstaged() {
        // "MM" = modified in index AND modified in worktree since. The
        // sidebar shows one letter; staged (X, first char) wins.
        let out = "1 MM N... 100644 100644 100644 abc123 def456 src/main.rs\n";
        let (_, _, _, changes) = parse_status(out);
        assert_eq!(changes[0].status, "M");
    }

    #[test]
    fn parse_status_untracked_file() {
        let out = "? new_file.txt\n";
        let (_, _, _, changes) = parse_status(out);
        assert_eq!(
            changes,
            vec![FileChange {
                status: "?".to_string(),
                path: "new_file.txt".to_string(),
            }]
        );
    }

    #[test]
    fn parse_status_renamed_file_uses_new_path() {
        // Porcelain v2 renamed entry: score field, then "R100", then the
        // orig\tnew path pair as the final field.
        let out =
            "2 R. N... 100644 100644 100644 abc123 def456 R100 old_name.rs\told_name.rs\tnew_name.rs\n";
        let (_, _, _, changes) = parse_status(out);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].status, "R");
    }

    #[test]
    fn parse_log_splits_sha_and_subject() {
        let out = "abc1234\u{1f}Fix the thing\ndef5678\u{1f}Add another thing\n";
        let entries = parse_log(out);
        assert_eq!(
            entries,
            vec![
                LogEntry {
                    sha: "abc1234".to_string(),
                    subject: "Fix the thing".to_string()
                },
                LogEntry {
                    sha: "def5678".to_string(),
                    subject: "Add another thing".to_string()
                },
            ]
        );
    }

    #[test]
    fn parse_log_ignores_blank_lines() {
        let out = "abc1234\u{1f}One\n\ndef5678\u{1f}Two\n";
        let entries = parse_log(out);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn xy_summary_prefers_x_over_y() {
        assert_eq!(xy_summary("M."), "M");
        assert_eq!(xy_summary(".D"), "D");
        assert_eq!(xy_summary(".."), ".");
    }

    /// End-to-end against a real temp repo — proves `collect` actually shells
    /// out correctly, not just that the parsers handle canned strings.
    #[test]
    fn collect_against_a_real_temp_repo() {
        let dir = std::env::temp_dir().join(format!(
            "kn9t_git_status_test_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("git must be on PATH for this test")
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "initial commit"]);
        std::fs::write(dir.join("a.txt"), "hello world").unwrap();
        std::fs::write(dir.join("b.txt"), "new file").unwrap();

        let state = collect(&dir).expect("must detect a real repo");
        assert_eq!(state.branch.as_deref(), Some("main"));
        assert_eq!(state.recent.len(), 1);
        assert_eq!(state.recent[0].subject, "initial commit");

        let statuses: Vec<&str> = state.changes.iter().map(|c| c.status.as_str()).collect();
        assert!(statuses.contains(&"M"), "a.txt must show modified");
        assert!(statuses.contains(&"?"), "b.txt must show untracked");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A directory that is not a git repo at all must yield `None`, not an
    /// empty-but-"clean" `GitState` — the UI needs to tell these apart.
    #[test]
    fn collect_outside_any_repo_returns_none() {
        let dir = std::env::temp_dir().join(format!(
            "kn9t_git_status_notrepo_{}_{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(collect(&dir), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
