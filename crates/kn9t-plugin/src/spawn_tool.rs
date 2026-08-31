//! R-PLUG-110/120/130 — SpawnTool: built-in tool that creates a child session
//! (fork_reason=subagent), runs a child ReAct loop, returns result as ToolResult.

use kn9t_core::{Cancel, Content, ToolCtx, ToolErr, ToolOutput, ToolSpec};
use kn9t_core::Tool;

/// Budget cap error message.
const BUDGET_EXCEEDED_MSG: &str = "budget cap exceeded: child budget would exceed parent remaining";

/// A child session executor. In production this would run a real ReAct loop;
/// in tests it is replaced by a mock via the `executor` field.
pub type ChildExecutor = Box<dyn Fn(&str, f64, Option<Vec<String>>) -> Result<String, String> + Send + Sync>;

/// SpawnTool: spawns a child agent session.
pub struct SpawnTool {
    spec: ToolSpec,
    /// Available tool names for child (None = inherit).
    child_tools: Option<Vec<String>>,
    /// Parent's remaining budget in USD.
    parent_budget_remaining: Option<f64>,
    /// The actual child executor (mock in tests, real loop in production).
    executor: ChildExecutor,
}

impl SpawnTool {
    pub fn new(
        child_tools: Option<Vec<String>>,
        parent_budget_remaining: Option<f64>,
        executor: ChildExecutor,
    ) -> Self {
        let spec = ToolSpec {
            name: "spawn_session".to_string(),
            description: "Spawn a child agent session (subagent) to handle a subtask. \
                Returns the child session's result."
                .to_string(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "The task for the child agent to perform."
                    },
                    "budget_usd": {
                        "type": "number",
                        "description": "Maximum budget in USD for the child session."
                    }
                },
                "required": ["task"]
            }),
            hidden: false, effects: vec![], policy: Default::default(),
        };
        SpawnTool {
            spec,
            child_tools,
            parent_budget_remaining,
            executor,
        }
    }
}

impl Tool for SpawnTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn execute(
        &self,
        args: &serde_json::Value,
        _ctx: &ToolCtx,
        _cancel: &Cancel,
    ) -> Result<ToolOutput, ToolErr> {
        let task = args
            .get("task")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        // R-PLUG-130: budget cap
        let requested_budget = args
            .get("budget_usd")
            .and_then(|b| b.as_f64())
            .unwrap_or(f64::INFINITY);

        let child_budget = match self.parent_budget_remaining {
            Some(remaining) => {
                let cap = requested_budget.min(remaining);
                if cap <= 0.0 {
                    return Ok(ToolOutput {
                        content: vec![Content::Text {
                            text: BUDGET_EXCEEDED_MSG.to_string(),
                        }],
                        details: None,
                        is_error: true,
                    });
                }
                cap
            }
            None => requested_budget,
        };

        // Execute child session
        match (self.executor)(&task, child_budget, self.child_tools.clone()) {
            Ok(result) => Ok(ToolOutput {
                content: vec![Content::Text { text: result }],
                details: None,
                is_error: false,
            }),
            Err(e) => Ok(ToolOutput {
                content: vec![Content::Text { text: e }],
                details: None,
                is_error: true,
            }),
        }
    }

    fn parallel_safe(&self) -> bool {
        false
    }
}
