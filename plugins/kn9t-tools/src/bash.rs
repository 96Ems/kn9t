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
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::read::track_as_read;

pub struct Bash;

/// Prepended to every Windows command.
///
/// PowerShell writes its own stdout in `[Console]::OutputEncoding`, which on
/// Windows PowerShell 5.1 is the machine's ANSI codepage. That is why
/// `Get-Content` of a UTF-8 file shows mojibake and why a native command's
/// accented output is dropped by our reader (a line that is not valid UTF-8 is
/// an `Err` that `lines().flatten()` silently discards). On PowerShell 7 it is
/// already UTF-8, and setting it again is harmless.
///
/// `$OutputEncoding` is the other half: the encoding used when sending text to
/// a native child's stdin. `[Console]::OutputEncoding` is wrapped in try/catch
/// because assigning it throws when there is no console (output redirected).
const PS_UTF8_PREAMBLE: &str =
    "try { [Console]::OutputEncoding = [System.Text.Encoding]::UTF8 } catch {}; \
     $OutputEncoding = [System.Text.Encoding]::UTF8; ";

/// The shell executable to run commands through.
///
/// Windows prefers PowerShell 7 (`pwsh`) when it is installed: it reads and
/// writes UTF-8 without a BOM by default, so `Set-Content`/`Out-File` no longer
/// corrupt UTF-8 files. Windows PowerShell 5.1 is the fallback, and its ANSI
/// default plus `-Encoding UTF8` BOM is the source of the mojibake the `write`
/// and `edit` tools exist to avoid.
///
/// Resolved once: scanning `PATH` per command would be a per-call cost, and the
/// answer cannot change while the plugin runs.
fn shell_exe() -> &'static str {
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    *SHELL.get_or_init(|| {
        if cfg!(windows) {
            if executable_on_path("pwsh") {
                "pwsh"
            } else {
                "powershell"
            }
        } else {
            "sh"
        }
    })
}

/// True if `exe` (or `exe.exe` on Windows) exists in a `PATH` directory.
fn executable_on_path(exe: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| executable_in(&dir, exe))
}

/// True if `exe` exists directly inside `dir`.
fn executable_in(dir: &Path, exe: &str) -> bool {
    if dir.join(exe).is_file() {
        return true;
    }
    #[cfg(windows)]
    if dir.join(format!("{exe}.exe")).is_file() {
        return true;
    }
    false
}

/// Split a command line into tokens, honouring single/double quotes so that
/// `Get-Content "C:\path with spaces\a.rs"` yields one path token.
fn tokenize(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in cmd.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() || c == ';' || c == '|' || c == ',' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Register every existing file named in the command as observed.
///
/// A `Get-Content`/`cat`/`Select-String` puts a file's content in the transcript
/// exactly like the `read` tool does, so forcing a follow-up `read` before `edit`
/// only burns a round-trip. Recording (hash, mtime) *after* the command has run
/// keeps the guard's real job intact: it still fires when a third party changes
/// the file between this observation and the write.
fn track_paths_in(cmd: &str) {
    const MAX_TRACKED: usize = 32;
    let mut tracked = 0;
    for tok in tokenize(cmd) {
        if tracked >= MAX_TRACKED {
            break;
        }
        let p = Path::new(&tok);
        if p.is_file() && track_as_read(p) {
            tracked += 1;
        }
    }
}

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
            "Run a shell command (PowerShell on Windows; `pwsh` is used when installed). \
             Use PowerShell syntax: `Get-ChildItem` not `ls`, `Get-Content` not `cat`, \
             `Remove-Item` not `rm`, `$env:TEMP` for the temp folder, backslashes in paths. \
             Do NOT write files with PowerShell: `Set-Content`, `Out-File`, `Add-Content` \
             and `>`/`>>` corrupt UTF-8 (Windows PowerShell 5.1 adds a BOM and re-encodes \
             text through ANSI, producing mojibake) — use the `write` and `edit` tools, \
             which preserve the file's encoding. \
             Streams stdout lines as progress. Cancelled calls kill the process. \
             Existing files named in the command are registered as observed, so a \
             following `edit`/`write` needs no separate `read`."
        } else {
            "Run a shell command (sh on Unix). \
             Use POSIX syntax. $TMPDIR or /tmp for temp files. \
             For file writes prefer the `write` and `edit` tools, which preserve encoding. \
             Streams stdout lines as progress. Cancelled calls kill the process. \
             Existing files named in the command are registered as observed, so a \
             following `edit`/`write` needs no separate `read`."
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

        // Spawn the child process. On Windows `pwsh` (PowerShell 7) is
        // preferred over `powershell` (5.1), and the command carries a UTF-8
        // preamble — see `shell_exe` and `PS_UTF8_PREAMBLE`.
        let shell = shell_exe();

        let mut cmd_builder = Command::new(shell);
        let arg = if cfg!(windows) { "-Command" } else { "-c" };
        let full_cmd = if cfg!(windows) {
            format!("{PS_UTF8_PREAMBLE}{cmd}")
        } else {
            cmd.clone()
        };
        cmd_builder
            .arg(arg)
            .arg(&full_cmd)
            .stdin(Stdio::null()) // Don't inherit stdin — prevents hangs on interactive prompts
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Run in the *session's* directory, not the plugin process's. A plugin is a
        // long-lived subprocess spawned by the server, so its own cwd is wherever the
        // server was started — unrelated to the session the command belongs to, and shared
        // by every session at once. `ctx.cwd` is the per-session value the host sends with
        // each call. `None` (a host that reported none) keeps the inherited cwd.
        if let Some(cwd) = &ctx.cwd {
            cmd_builder.current_dir(cwd);
        }

        let mut child = match cmd_builder.spawn()
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

        // Files named in the command have now been observed (or produced) — record
        // them so `edit`/`write` don't demand a redundant `read` of content the
        // model already has. Done after the child exits so the recorded hash/mtime
        // reflect the post-command state.
        track_paths_in(&cmd);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_keeps_quoted_path_whole() {
        let toks = tokenize(r#"Get-Content "C:\a b\c.rs""#);
        assert_eq!(toks, vec!["Get-Content", r"C:\a b\c.rs"]);
    }

    #[test]
    fn tokenize_splits_on_pipe_and_semicolon() {
        let toks = tokenize("Set-Location x; Get-Content a.rs | Select-Object");
        assert_eq!(
            toks,
            vec!["Set-Location", "x", "Get-Content", "a.rs", "Select-Object"]
        );
    }

    #[test]
    fn bash_reading_a_file_satisfies_the_edit_guard() {
        let dir = std::env::temp_dir().join("kn9t_bash_track");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tracked.txt");
        std::fs::write(&file, b"hello\n").unwrap();

        let map = crate::read::read_map();
        map.lock().unwrap().remove(&file);
        assert!(!map.lock().unwrap().contains_key(&file));

        track_paths_in(&format!("Get-Content \"{}\"", file.display()));

        assert!(
            map.lock().unwrap().contains_key(&file),
            "a bash command naming the file must register it, so edit needs no extra read"
        );
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn tokens_that_are_not_files_are_ignored() {
        track_paths_in("Get-ChildItem -Recurse -Filter *.rs");
        // The map is process-global and other tests write to it, so assert on
        // these specific tokens rather than on the map's length.
        let map = crate::read::read_map();
        let map = map.lock().unwrap();
        for token in ["Get-ChildItem", "-Recurse", "-Filter", "*.rs"] {
            assert!(
                !map.contains_key(Path::new(token)),
                "non-file token {token:?} must not be tracked"
            );
        }
    }

    #[test]
    fn windows_preamble_sets_utf8() {
        assert!(PS_UTF8_PREAMBLE.contains("[Console]::OutputEncoding"));
        assert!(PS_UTF8_PREAMBLE.contains("$OutputEncoding"));
        // It must be a single statement-safe line: no newline in the -Command arg.
        assert!(!PS_UTF8_PREAMBLE.contains('\n'));
    }

    #[test]
    fn the_description_no_longer_recommends_a_lossy_write() {
        let spec = Bash.spec();
        let desc = &spec.description;
        // `Set-Content -Encoding UTF8` was the actual instruction that caused
        // the PowerShell mojibake; it must not come back.
        assert!(!desc.contains("-Encoding UTF8"), "{desc}");
        assert!(desc.contains("write"), "{desc}");
        assert!(desc.contains("edit"), "{desc}");
    }

    #[test]
    fn executable_in_finds_a_file_and_rejects_a_missing_one() {
        let dir = std::env::temp_dir().join("kn9t_bash_path_probe");
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let name = "kn9t-fake-pwsh.exe";
        #[cfg(not(windows))]
        let name = "kn9t-fake-pwsh";
        std::fs::write(dir.join(name), b"").unwrap();
        let stem = "kn9t-fake-pwsh";

        assert!(executable_in(&dir, stem));
        assert!(!executable_in(&dir, "kn9t-definitely-absent"));
        std::fs::remove_file(dir.join(name)).ok();
    }
}
