//! Serializes git state to JSON for the UI views.

mod ui_diff;
mod ui_files;

pub use ui_diff::LUA_DIFF;
pub use ui_files::LUA_FILES;

use crate::diff::DiffFile;
use crate::git::GitState;

/// JSON pushed via `ui_set_state` to both views.
pub fn state_to_json(state: Option<&GitState>, files: &[DiffFile]) -> serde_json::Value {
    let repo = match state {
        None => serde_json::Value::Null,
        Some(s) => serde_json::json!({
            "branch": s.branch,
            "ahead": s.ahead,
            "behind": s.behind,
            "changes": s.changes.iter().map(|c| serde_json::json!({
                "status": c.status,
                "path": c.path,
            })).collect::<Vec<_>>(),
            "recent": s.recent.iter().map(|l| serde_json::json!({
                "sha": l.sha,
                "subject": l.subject,
            })).collect::<Vec<_>>(),
        }),
    };

    serde_json::json!({
        "repo": repo,
        "diff": files.iter().map(diff_file_to_json).collect::<Vec<_>>(),
    })
}

fn diff_file_to_json(f: &DiffFile) -> serde_json::Value {
    serde_json::json!({
        "path": f.path,
        "status": f.status.tag(),
        "additions": f.additions,
        "deletions": f.deletions,
        "hunks": f.hunks.iter().map(|h| serde_json::json!({
            "header": h.header,
            "old_start": h.old_start,
            "new_start": h.new_start,
            "lines": h.lines.iter().map(|l| serde_json::json!({
                "kind": l.kind.tag(),
                "text": l.text,
                "new_lineno": l.new_lineno,
                "old_lineno": l.old_lineno,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}
