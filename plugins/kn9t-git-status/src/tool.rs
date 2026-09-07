//! The `git_status` tool: real, agent-callable git summary, and (as a side
//! effect on its first invocation per process) the bootstrap trigger for the
//! background sidebar poller — see `poller.rs` for why it has to be a real
//! tool call rather than something session-start-driven.

use kn9t_plugin_sdk::ctx::ToolCallCtx;
use kn9t_plugin_sdk::traits::{PluginTool, ToolOutput};
use kn9t_plugin_sdk::wire::{DefaultPolicy, ToolPolicy, ToolSpec};
use serde_json::{json, Value};

use crate::git;
use crate::poller;

pub struct GitStatus;

impl PluginTool for GitStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".into(),
            description: "Show the current git branch, ahead/behind counts, \
                working-tree changes, and recent commits for the session's \
                working directory."
                .into(),
            schema: json!({
                "type": "object",
                "properties": {},
            }),
            parallel_safe: true,
            hidden: false,
            effects: vec![],
            // Read-only shell out to `git`, same trust level as `read`.
            policy: ToolPolicy {
                pattern_field: None,
                default_policy: DefaultPolicy::Allow,
                builtin_allow: vec![],
                builtin_deny: vec![],
            },
        }
    }

    fn execute(&self, _args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let Some(cwd) = ctx.cwd.clone() else {
            return ToolOutput::error(
                "no working directory available for this session (host did not report cwd)",
            );
        };

        // Bootstrap the background poller for this repo. Cheap to call every
        // time: `ensure_started` is a no-op once a poller for this `cwd` is
        // already running (see its own doc comment for the per-cwd guard).
        poller::ensure_started(ctx.host.clone(), cwd.clone());

        match git::collect(&cwd) {
            None => ToolOutput::text(format!("{} is not inside a git repository.", cwd.display())),
            Some(state) => ToolOutput::text(format_for_agent(&state)),
        }
    }
}

/// Plain-text summary for the model — separate from `ui::state_to_json`,
/// which is the machine-readable shape the TUI sidebar consumes. The model
/// wants prose it can quote back to the user; the sidebar wants structured
/// rows it can colour per-field. Serving both from one format would make
/// neither good.
fn format_for_agent(state: &git::GitState) -> String {
    let mut out = String::new();
    let branch = state.branch.as_deref().unwrap_or("?");
    out.push_str(&format!("branch: {branch}"));
    if state.ahead > 0 || state.behind > 0 {
        out.push_str(&format!(" (+{} -{})", state.ahead, state.behind));
    }
    out.push('\n');

    if state.changes.is_empty() {
        out.push_str("working tree clean\n");
    } else {
        out.push_str(&format!("{} changed file(s):\n", state.changes.len()));
        for c in &state.changes {
            out.push_str(&format!("  {} {}\n", c.status, c.path));
        }
    }

    if !state.recent.is_empty() {
        out.push_str("\nrecent commits:\n");
        for l in &state.recent {
            out.push_str(&format!("  {} {}\n", l.sha, l.subject));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{FileChange, GitState, LogEntry};

    #[test]
    fn format_for_agent_reports_clean_tree() {
        let state = GitState {
            branch: Some("main".to_string()),
            ahead: 0,
            behind: 0,
            changes: vec![],
            recent: vec![],
        };
        let out = format_for_agent(&state);
        assert!(out.contains("branch: main"));
        assert!(out.contains("working tree clean"));
    }

    #[test]
    fn format_for_agent_lists_changes_and_ahead_behind() {
        let state = GitState {
            branch: Some("feature".to_string()),
            ahead: 3,
            behind: 1,
            changes: vec![FileChange {
                status: "M".to_string(),
                path: "src/main.rs".to_string(),
            }],
            recent: vec![LogEntry {
                sha: "abc1234".to_string(),
                subject: "wip".to_string(),
            }],
        };
        let out = format_for_agent(&state);
        assert!(out.contains("(+3 -1)"));
        assert!(out.contains("M src/main.rs"));
        assert!(out.contains("abc1234 wip"));
    }

    #[test]
    fn spec_has_no_required_arguments() {
        let spec = GitStatus.spec();
        assert_eq!(spec.name, "git_status");
        // An agent should be able to call this with `{}` — no args needed.
        assert_eq!(spec.schema["properties"].as_object().unwrap().len(), 0);
    }
}
