//! R-TOOL-080 / R-TOOL-090 / R-TOOL-095 -- the `bash` command classifier.
//!
//! This is a heuristic, NOT a sandbox (R-TOOL-095, DESIGN 10.1). `AllowReadOnly` is
//! defense in depth against an unhelpful model, never a security boundary against an
//! adversarial one. Real isolation is a container and is out of scope. No downstream code
//! may treat `AllowReadOnly` as a security guarantee.
//!
//! Two shell grammars share one decision pipeline (R-TOOL-090); only tokenization differs
//! ([`Shell::Posix`] vs [`Shell::PowerShell`]). The pipeline evaluates in a fixed order so
//! `cat x > y` (a write) and `sh -c '...'` / `iex '...'` (interpreter bypasses) all resolve
//! to `Ask` rather than `AllowReadOnly`.
//!
//! # Why this lives in `kn9t-server` (ADR-0001)
//!
//! This module originally lived in the in-process `kn9t-tools` crate and was deleted in
//! commit 5b65819 when tools moved to a subprocess plugin; it was never reimplemented, so
//! `sh -c 'rm -rf /'` went unclassified. It is restored here rather than inside the tool
//! plugin because the **server** owns the approval UI, the write lease, and user config.
//! A plugin that classified its own calls would be self-approving, and a careless or
//! hostile plugin could mark everything safe. The server decides; plugins only describe.

use std::collections::BTreeMap;

/// R-TOOL-080 -- which grammar to tokenize with, selected at runtime by the configured or
/// detected shell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shell {
    Posix,
    PowerShell,
}

/// R-TOOL-080 -- the classifier verdict. `HardDeny` is never presented as an approval
/// prompt (R-TOOL-090 step 6).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Classification {
    AllowReadOnly,
    Ask,
    HardDeny(String),
}

/// R-TOOL-090 -- the `[policy.bash]` config shape, exactly DESIGN 10.1's TOML. Populated
/// from config in later stages; [`BashPolicy::default`] mirrors the design's example.
#[derive(Clone)]
pub struct BashPolicy {
    pub allow_read: Vec<String>,
    pub always_ask: Vec<String>,
    pub never: Vec<String>,
    /// Subcommand-sensitive commands (`git`, `cargo`, `npm`): allowed only with the listed
    /// subcommands.
    pub allow_read_sub: BTreeMap<String, Vec<String>>,
}

fn strvec(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

impl Default for BashPolicy {
    fn default() -> Self {
        let allow_read = strvec(&[
            "rg", "grep", "egrep", "fgrep", "find", "fd", "ls", "cat", "head", "tail", "wc",
            "file", "stat", "which", "type", "pwd", "echo", "sort", "uniq", "cut", "tr", "awk",
            "sed", "jq", "diff", "tree", "du", "df", "env", "date", "basename", "dirname",
            "realpath", "readlink", "nl", "column", "xxd", "strings",
            // PowerShell read-only cmdlets (parallel table for the pwsh grammar).
            "Get-Content", "Get-ChildItem", "Get-Item", "Select-String", "Select-Object",
            "Where-Object", "Measure-Object", "Test-Path", "Get-Location", "Resolve-Path",
            "Format-Table", "Format-List", "Sort-Object", "Write-Output",
        ]);

        let always_ask = strvec(&[
            "rm", "mv", "cp", "chmod", "chown", "kill", "dd", "curl", "wget", "ssh", "scp", "sh",
            "bash", "zsh", "python", "python3", "node", "perl", "ruby", "eval",
            // PowerShell interpreters / eval surfaces (closes the `iex '...'` bypass).
            "pwsh", "powershell", "Invoke-Expression", "iex",
            // PowerShell mutating cmdlets.
            "Set-Content", "Add-Content", "Remove-Item", "New-Item", "Move-Item", "Copy-Item",
        ]);

        let never = strvec(&["shutdown", "reboot", "mkfs*", "fdisk", "sudo"]);

        let mut allow_read_sub = BTreeMap::new();
        allow_read_sub.insert(
            "git".to_string(),
            strvec(&[
                "log", "diff", "show", "status", "branch", "blame", "describe", "rev-parse",
                "ls-files", "remote", "tag",
            ]),
        );
        allow_read_sub.insert("cargo".to_string(), strvec(&["tree", "metadata", "--version"]));
        allow_read_sub.insert("npm".to_string(), strvec(&["ls", "view", "outdated"]));

        BashPolicy {
            allow_read,
            always_ask,
            never,
            allow_read_sub,
        }
    }
}

/// One command segment: a whitespace-tokenized `argv`.
#[derive(Debug, Clone)]
struct Segment {
    argv: Vec<String>,
}

impl Segment {
    fn arg0(&self) -> Option<&str> {
        self.argv.first().map(|s| s.as_str())
    }
    fn base(&self) -> &str {
        self.arg0().map(basename).unwrap_or("")
    }
}

/// R-TOOL-080 / R-TOOL-090 -- classify a command string. Tokenizes per `shell`, then runs
/// the shared decision pipeline in the exact numbered order of DESIGN 10.1.
pub fn classify(cmd: &str, shell: Shell, policy: &BashPolicy) -> Classification {
    let segments = split_segments(cmd, shell);
    if segments.is_empty() {
        return Classification::Ask;
    }

    // Rule 1: any segment's argv[0] absent from allow_read -> Ask. Interpreters,
    // subcommand-sensitive commands, and never-entries are deliberately absent from
    // allow_read; they are "special" and pass this gate so the later, more specific rules
    // (4/5/6) can speak.
    for seg in &segments {
        if seg.arg0().is_none() {
            return Classification::Ask;
        }
        let base = seg.base();
        let is_special = policy.allow_read_sub.contains_key(base)
            || contains(&policy.always_ask, base)
            || matches_never(&policy.never, base);
        if !contains(&policy.allow_read, base) && !is_special {
            return Classification::Ask;
        }
    }

    // Rule 2: redirection / tee / dd / command substitution / subshell -> Ask.
    if has_redirection_or_substitution(cmd, shell)
        || segments.iter().any(|s| {
            let b = s.base();
            b == "tee" || b == "dd"
        })
    {
        return Classification::Ask;
    }

    // Rule 3: in-place edit flags -> Ask.
    if segments.iter().any(has_inplace_flag) {
        return Classification::Ask;
    }

    // Rule 4: git/cargo/npm with a subcommand outside allow_read_sub -> Ask.
    for seg in &segments {
        if let Some(allowed) = policy.allow_read_sub.get(seg.base()) {
            let sub = seg.argv.get(1).map(|s| s.as_str());
            match sub {
                Some(sub) if allowed.iter().any(|a| a == sub) => {}
                _ => return Classification::Ask,
            }
        }
    }

    // Rule 5: argv[0] in always_ask -> Ask (every interpreter lives here).
    for seg in &segments {
        if contains(&policy.always_ask, seg.base()) {
            return Classification::Ask;
        }
    }

    // Rule 6: argv[0] matches `never` -> HardDeny.
    for seg in &segments {
        if matches_never(&policy.never, seg.base()) {
            return Classification::HardDeny(format!("`{}` is never permitted", seg.base()));
        }
    }

    // Rule 7: otherwise Allow.
    Classification::AllowReadOnly
}

// --------------------------- tokenization ---------------------------

/// Split a command into segments on the grammar's statement/pipeline separators
/// (`&&`, `||`, `|`, `;`, newline). Both grammars share these; PowerShell 7 adds `&&`/`||`
/// as pipeline-chain operators, which are covered by the same set.
fn split_segments(cmd: &str, _shell: Shell) -> Vec<Segment> {
    let mut normalized = cmd.replace(['\n', '\r'], "\u{1}");
    for s in ["&&", "||", "|", ";"] {
        normalized = normalized.replace(s, "\u{1}");
    }
    normalized
        .split('\u{1}')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Segment {
            argv: tokenize_argv(s, _shell),
        })
        .filter(|s| !s.argv.is_empty())
        .collect()
}

/// Whitespace argv tokenizer that respects single/double quotes (best-effort; the
/// classifier is a heuristic, R-TOOL-095). A leading PowerShell call operator `&` is
/// dropped so `& rm x` classifies on `rm`.
fn tokenize_argv(seg: &str, shell: Shell) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in seg.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                _ => cur.push(ch),
            },
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    if shell == Shell::PowerShell {
        while out.first().map(|s| s == "&").unwrap_or(false) {
            out.remove(0);
        }
    }
    out
}

// --------------------------- structural predicates ---------------------------

/// Rule 2 detection over the raw command text, per grammar.
fn has_redirection_or_substitution(cmd: &str, shell: Shell) -> bool {
    // Command substitution / subexpression, common to both grammars.
    if cmd.contains("$(") || cmd.contains('`') {
        return true;
    }
    match shell {
        // Redirections `>`,`>>`,`<`,`>|`; subshell `(...)`.
        Shell::Posix => has_redir_char(cmd) || has_posix_subshell(cmd),
        // Redirections `>`,`>>`,`2>`,`*>`; array subexpression `@(...)`.
        Shell::PowerShell => has_redir_char(cmd) || cmd.contains("@("),
    }
}

/// A `>` or `<` outside quotes. Tracks quote state so a redirection char inside a quoted
/// string does not trip it. Covers `>>`, `2>`, `*>`, `>|` since all contain `>`.
fn has_redir_char(cmd: &str) -> bool {
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '>' | '<' => return true,
                _ => {}
            },
        }
    }
    false
}

/// A `(` that starts a subshell (not part of `$(`). Heuristic.
fn has_posix_subshell(cmd: &str) -> bool {
    let chars: Vec<char> = cmd.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch == '(' {
            let prev = if i == 0 { None } else { Some(chars[i - 1]) };
            if prev != Some('$') {
                return true;
            }
        }
    }
    false
}

/// Rule 3: in-place edit flags on `sed`/`perl` (`-i`). `awk` mutation is via redirection
/// (rule 2); PowerShell mutating cmdlets are in `always_ask`.
fn has_inplace_flag(seg: &Segment) -> bool {
    match seg.base() {
        "sed" | "perl" => seg
            .argv
            .iter()
            .skip(1)
            .any(|a| a == "-i" || a.starts_with("-i")),
        _ => false,
    }
}

// --------------------------- name helpers ---------------------------

/// Basename of an executable path, stripping directory components (POSIX `/` and
/// Windows `\\`). Case preserved (PowerShell cmdlet matching is case-insensitive via
/// [`contains`]).
fn basename(arg0: &str) -> &str {
    let after_slash = arg0.rsplit(['/', '\\']).next().unwrap_or(arg0);
    after_slash
}

/// Case-insensitive membership (PowerShell cmdlets and POSIX commands both matched).
fn contains(list: &[String], name: &str) -> bool {
    list.iter().any(|e| e.eq_ignore_ascii_case(name))
}

/// `never` supports a trailing `*` glob (e.g. `mkfs*`), case-insensitive.
fn matches_never(list: &[String], name: &str) -> bool {
    list.iter().any(|e| {
        if let Some(prefix) = e.strip_suffix('*') {
            name.to_ascii_lowercase()
                .starts_with(&prefix.to_ascii_lowercase())
        } else {
            e.eq_ignore_ascii_case(name)
        }
    })
}
