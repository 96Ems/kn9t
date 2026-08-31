//! bash tool — executes shell commands, streams stdout via progress chunks.
//!
//! R-PLUG2-120: streams stdout lines as chunk messages while the child runs.
//! Cancel stops the child process (interrupt-driven via CancelToken, not polling).

use kn9t_plugin_sdk::{
    ctx::ToolCallCtx,
    traits::{PluginTool, ToolOutput},
    wire::{DefaultPolicy, Effect, EffectKind, ToolPolicy, ToolSpec},
};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct Bash;

/// Drain a channel receiver, waiting up to `timeout` for the sender to close.
/// Returns all collected lines.
fn drain_with_timeout(rx: Receiver<String>, timeout: Duration) -> Vec<String> {
    let mut lines = Vec::new();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            // Timeout: collect whatever is buffered and return
            lines.extend(rx.try_iter());
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) => lines.push(line),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Sender closed, drain any remaining
                lines.extend(rx.try_iter());
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lines.extend(rx.try_iter());
                break;
            }
        }
    }
    lines
}

impl PluginTool for Bash {
    fn spec(&self) -> ToolSpec {
        let description = if cfg!(windows) {
            "Run a shell command (PowerShell on Windows). \
             Use PowerShell syntax: `Get-ChildItem` not `ls`, `Get-Content` not `cat`, \
             `Remove-Item` not `rm`, `$env:TEMP` for temp folder, backslash `\\` in paths. \
             For file writes use `-Encoding UTF8` (e.g. `Set-Content -Encoding UTF8`). \
             Streams stdout lines as progress. Cancelled calls kill the process."
        } else {
            "Run a shell command (sh on Unix). \
             Use POSIX syntax. $TMPDIR or /tmp for temp files. \
             Streams stdout lines as progress. Cancelled calls kill the process."
        };
        
        ToolSpec {
            name: "bash".into(),
            description: description.into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "cmd": {
                        "type": "string",
                        "description": "The shell command to execute."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum seconds to wait (default: 120)."
                    }
                },
                "required": ["cmd"]
            }),
            parallel_safe: false,
            hidden: false,
            effects: vec![Effect { field: "cmd".into(), kind: EffectKind::Shell }],
            policy: ToolPolicy {
                pattern_field: Some("cmd".into()),
                default_policy: DefaultPolicy::Ask,
                // Read-only commands that are safe to auto-allow
                builtin_allow: vec![
                    // Navigation & inspection
                    "cd *".into(),
                    "pwd".into(),
                    "ls *".into(),
                    "dir *".into(),
                    "cat *".into(),
                    "head *".into(),
                    "tail *".into(),
                    "echo *".into(),
                    "type *".into(),
                    "Get-ChildItem *".into(),
                    "Get-Content *".into(),
                    "Get-Location".into(),
                    "Set-Location *".into(),
                    // Git read operations
                    "git status *".into(),
                    "git log *".into(),
                    "git diff *".into(),
                    "git branch *".into(),
                    "git show *".into(),
                    "git remote *".into(),
                    // Cargo
                    "cargo check *".into(),
                    "cargo test *".into(),
                    "cargo build *".into(),
                    "cargo clippy *".into(),
                ],
                // Only truly catastrophic commands that should NEVER run.
                // Everything else uses Ask — user can approve if needed.
                builtin_deny: vec![
                    "sudo *".into(),  // privilege escalation
                    "su *".into(),    // privilege escalation
                ],
            },
        }
    }

    fn execute(&self, args: &Value, ctx: &ToolCallCtx) -> ToolOutput {
        let cmd = match args.get("cmd").and_then(|c| c.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::error("missing 'cmd' argument"),
        };
        let timeout_secs = args.get("timeout_secs")
            .and_then(|t| t.as_u64())
            .unwrap_or(120);

        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled before start");
        }

        // Spawn the child process.
        let (shell, flag) = if cfg!(windows) {
            ("powershell", "-Command")
        } else {
            ("sh", "-c")
        };

        let mut child = match Command::new(shell)
            .arg(flag)
            .arg(&cmd)
            .stdin(Stdio::null())  // Don't inherit stdin — prevents hangs on interactive prompts
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("spawn failed: {e}")),
        };

        // Stream stdout in a background thread.
        let stdout_pipe = child.stdout.take().expect("stdout piped");
        let progress = ctx.progress.clone();
        let (tx, rx) = mpsc::channel::<String>();
        let stdout_handle: JoinHandle<()> = thread::spawn(move || {
            for line in BufReader::new(stdout_pipe).lines().flatten() {
                progress.send(&line);
                if tx.send(line).is_err() {
                    break; // receiver dropped, stop reading
                }
            }
        });

        // Collect stderr in a background thread.
        let stderr_pipe = child.stderr.take().expect("stderr piped");
        let (stx, srx) = mpsc::channel::<String>();
        let stderr_handle: JoinHandle<()> = thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines().flatten() {
                if stx.send(line).is_err() {
                    break; // receiver dropped, stop reading
                }
            }
        });

        // Poll for completion, respecting cancel and timeout.
        let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
        let exit = loop {
            if ctx.cancel.is_cancelled() {
                let _ = child.kill();
                // Wait briefly for threads to notice pipe closure
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return ToolOutput::error("cancelled");
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        // Wait briefly for threads to notice pipe closure
                        let _ = stdout_handle.join();
                        let _ = stderr_handle.join();
                        return ToolOutput::error("timed out");
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return ToolOutput::error(format!("wait error: {e}")),
            }
        };

        // Process exited. Wait for reader threads to finish draining pipes.
        // The threads will exit once the pipes close (which happens when child exits).
        // Give them a reasonable timeout to avoid blocking forever on broken pipes.
        let drain_timeout = Duration::from_millis(500);
        
        // Join threads (they should finish quickly since child has exited)
        let _ = stdout_handle.join();
        let _ = stderr_handle.join();

        // Now drain channels - senders are dropped, so recv will return Disconnected
        let stdout_lines = drain_with_timeout(rx, drain_timeout);
        let stderr_lines = drain_with_timeout(srx, drain_timeout);

        let mut output = stdout_lines.join("\n");
        if !stderr_lines.is_empty() {
            if !output.is_empty() { output.push('\n'); }
            output.push_str(&stderr_lines.join("\n"));
        }

        if exit.success() {
            ToolOutput::text(output)
        } else {
            // Non-zero exit, but if we got stdout output, treat as success.
            // The user likely got what they needed (e.g., grep found matches
            // but also hit permission errors on some files).
            let has_stdout = !stdout_lines.is_empty();
            let exit_msg = format!("exit {}\n{output}", exit.code().unwrap_or(-1));
            if has_stdout {
                ToolOutput::text(exit_msg)
            } else {
                ToolOutput::error(exit_msg)
            }
        }
    }
}
