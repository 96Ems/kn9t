# ADR-0007: CRLF normalization via .gitattributes

## Status
Superseded by [ADR-0009](0009-pin-eol-lf-on-executable-text.md)

> The Decision here is sound but incomplete, and the Consequences section is
> **known-false**: `core.autocrlf=true` was *not* safe for contributors, because
> `text=auto` normalizes the index while `bash` executes the working tree. Every
> guard script this ADR set out to protect was unrunnable as a result (96E-29).
> Left unedited as the historical record; see ADR-0009 for the fix.

## Date
2026-08-31

## Context
Four files had ~500-line phantom diffs from a Windows editor (CRLF vs LF). `.gitattributes`
previously only pinned replay fixtures (`crates/kn9t-provider-replay/tests/fixtures/* -text`),
so every other file's line ending depended on the committer's `core.autocrlf` and editor.

This is the same class as GI-1 and ADR-0005: an invariant claimed in docs that nothing
enforced. A `cargo fmt` or editor save on Windows silently creates a repo-wide diff that
obscures real changes and breaks `check-schema.sh`/`check-gi1.sh` drift gates.

## Decision
`* text=auto` in `.gitattributes` — normalize all text files to LF on commit, with explicit
exceptions for fixtures that must retain raw bytes (`-text` + `eol=lf` where needed).

```
* text=auto
crates/kn9t-provider-replay/tests/fixtures/* -text
*.md text
*.rs text
*.toml text
*.json text
```

Existing files will be renormalized once (`git add --renormalize .`).

## Consequences
- No more CRLF phantom diffs; `git diff --stat` stays honest.
- Windows contributors can keep `core.autocrlf=true` locally; the repo is canonical LF.
- Binary fixtures remain untouched (`-text`).
- One-time churn: a single commit will touch line endings, but subsequent commits are clean.
