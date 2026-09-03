#!/usr/bin/env python3
"""kn9t-policy: User-configurable tool approval plugin.

This plugin intercepts tool calls via before_tool_call hook and decides
whether to allow, deny, or pass through to interactive approval.

Policy rules are loaded from ~/.kn9t/policy.py which users can edit.

Usage: python kn9t-policy.py
"""

import json
import sys
import importlib.util
from pathlib import Path

# ── Default policy (created at ~/.kn9t/policy.py if missing) ──────────────────

DEFAULT_POLICY = '''
"""User-editable policy rules for kn9t tool approval.

Edit this file to customize which tool calls are auto-allowed or denied.

The `check(tool, args, cwd)` function is called for every tool call.
Return: "allow", "deny", "ask", or {"action": "deny", "reason": "..."}
"""

import re
import fnmatch

def split_commands(cmd):
    """Split shell command into parts (handles ; && || |)."""
    return [p.strip() for p in re.split(r"\\s*(?:;|&&|\\|\\||\\|)\\s*", cmd) if p.strip()]

def matches(value, patterns):
    """Check if value matches any fnmatch pattern."""
    return any(fnmatch.fnmatch(value, p) for p in patterns)

# Safe commands (read-only, no side effects)
ALLOW = [
    # Navigation & listing
    "cd *", "pwd", "ls *", "dir *", "tree *",
    # File reading
    "cat *", "head *", "tail *", "less *", "more *", "bat *",
    # Search & find
    "grep *", "rg *", "ag *", "find *", "fd *", "locate *",
    "which *", "where *", "type *", "whereis *",
    # Echo & test
    "echo *", "printf *", "test *", "[*",
    # Git read-only (NOT checkout, reset, clean, push, rebase)
    "git status*", "git log*", "git diff*", "git show*", "git branch*",
    "git remote*", "git stash list*", "git tag*", "git describe*",
    # Rust
    "cargo check*", "cargo test*", "cargo build*", "cargo clippy*",
    "cargo fmt --check*", "rustc --version*", "rustup show*",
    # Python read-only
    "python --version*", "python3 --version*", "python -c *",
    "pip list*", "pip show*", "pip freeze*", "pip --version*",
    "uv pip list*", "uv pip show*",
    # Node read-only
    "node --version*", "npm list*", "npm view*", "npm --version*",
    "bun --version*", "pnpm list*",
    # System info
    "uname *", "hostname", "whoami", "id", "env", "printenv*",
    "date", "uptime", "df *", "du *", "free *", "ps *", "top -bn1*",
    # PowerShell read-only
    "Get-ChildItem*", "Get-Content*", "Get-Location", "Set-Location*",
    "Get-Process*", "Get-Service*", "Get-Item*", "Test-Path*",
]

# Dangerous commands (hard deny, never allow)
DENY = ["sudo *", "su *", "shutdown*", "reboot*", "mkfs*", "rm -rf /*"]

# Destructive git commands that need prompt (not in ALLOW = ask)
# git checkout, git reset, git clean, git push, git rebase, git merge


# Tools that are always allowed (fnmatch patterns)
ALLOW_TOOLS = [
    "read", "write", "edit", "glob", "grep",  # file ops
    "mcp_*",  # MCP tools
]

def check(tool, args, cwd):
    if tool == "bash":
        cmd = args.get("cmd", "")
        for part in split_commands(cmd):
            if matches(part, DENY):
                return {"action": "deny", "reason": f"Blocked: {part}"}
            if not matches(part, ALLOW):
                return "ask"
        return "allow"
    
    # Tools in allow list (supports wildcards)
    if matches(tool, ALLOW_TOOLS):
        return "allow"
    
    return "ask"  # unknown tools need approval
'''


# ── Plugin implementation ─────────────────────────────────────────────────────

def load_policy():
    """Load policy from ~/.kn9t/policy.py or create default."""
    policy_path = Path.home() / ".kn9t" / "policy.py"
    
    if not policy_path.exists():
        policy_path.parent.mkdir(parents=True, exist_ok=True)
        policy_path.write_text(DEFAULT_POLICY)
        print(f"Created {policy_path}", file=sys.stderr)
    
    try:
        spec = importlib.util.spec_from_file_location("policy", policy_path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        if hasattr(module, "check"):
            print(f"Loaded {policy_path}", file=sys.stderr)
            return module.check
    except Exception as e:
        print(f"Policy load error: {e}", file=sys.stderr)
    
    return None


def read_msg():
    """Read JSON line from stdin."""
    line = sys.stdin.readline()
    return json.loads(line) if line else None


def write_msg(msg):
    """Write JSON line to stdout."""
    sys.stdout.write(json.dumps(msg, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def run():
    """Main plugin loop."""
    check_fn = load_policy()
    
    # Handshake
    hello = read_msg()
    if not hello or hello.get("t") != "hello":
        return
    
    print(f"Connected to kn9t {hello.get('kn9t', '?')}", file=sys.stderr)
    
    write_msg({
        "t": "hello",
        "name": "kn9t-policy",
        "capabilities": [],
        "hooks": ["before_tool_call"],
        "tools": [],
    })
    
    # Main loop
    while True:
        msg = read_msg()
        if not msg:
            break
        
        if msg.get("t") == "shutdown":
            break
        
        if msg.get("t") == "hook" and msg.get("hook") == "before_tool_call":
            hook_id = msg.get("id", 0)
            payload = msg.get("payload", {})
            
            tool = payload.get("tool", "")
            args = payload.get("args", {})
            cwd = payload.get("cwd", "")
            
            # Run policy
            result = {"action": "allow"}
            if check_fn:
                try:
                    r = check_fn(tool, args, cwd)
                    if r == "allow":
                        result = {"action": "allow"}
                    elif r == "deny":
                        result = {"action": "deny", "reason": "Denied by policy"}
                    elif r == "ask":
                        result = {"action": "ask", "reason": "Approval required"}
                    elif isinstance(r, dict):
                        result = r  # pass through {action, reason}
                except Exception as e:
                    print(f"Policy error: {e}", file=sys.stderr)
                    result = {"action": "deny", "reason": f"Policy error: {e}"}
            
            write_msg({"t": "result", "id": hook_id, **result})
        
        elif msg.get("t") == "hook":
            # Other hooks: allow
            write_msg({"t": "result", "id": msg.get("id", 0), "action": "allow"})


if __name__ == "__main__":
    run()
