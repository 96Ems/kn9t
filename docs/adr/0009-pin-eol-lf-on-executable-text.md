# ADR-0009: Pin `eol=lf` on executable and generated text

## Status
Accepted — supersedes ADR-0007

## Date
2026-09-03

## Context
ADR-0007 set `* text=auto` to stop CRLF phantom diffs. Its Context named the real
stake precisely:

> "This is the same class as GI-1 and ADR-0005: an invariant claimed in docs that
> nothing enforced. […] breaks `check-schema.sh`/`check-gi1.sh` drift gates."

Its Consequences then claimed:

> "Windows contributors can keep `core.autocrlf=true` locally; the repo is canonical LF."

That claim is false for executable text, and the failure it predicted happened anyway.

`text=auto` normalizes line endings in the **index**. With `core.autocrlf=true` a
checkout writes **CRLF into the working tree** — and `bash` executes the working tree,
not the index. A CRLF shebang makes `#!/bin/bash\r` an unknown interpreter:

```
$ bash scripts/check-gi1.sh
line 4: $'\r': command not found
line 12: syntax error near unexpected token `$'do\r''
$ echo $?
2
```

```
$ git ls-files --eol scripts/check-gi1.sh
i/lf    w/crlf    attr/text=auto    scripts/check-gi1.sh
```

All six guard scripts were unrunnable on the primary development machine. The
pre-commit hook was additionally never installed (`.git/hooks/` held only `.sample`
files), so no gate ran at commit time either. GI-1, GI-6, schema drift, mojibake, and
the unwrap trend were unenforced for an unknown period.

The same mechanism broke `xtask --check` from the other side: the generator writes
`\n`, the working tree held `\r\n`, and `check()` compares byte-for-byte — so all five
generated files reported as drifted when `git diff --ignore-cr-at-eol` showed zero
content difference. A gate that is permanently red trains people to ignore it, which
is worse than no gate.

## Decision

1. Pin `eol=lf` on text whose *bytes in the working tree* are semantically load-bearing:

   ```
   *.sh    text eol=lf      # shebang + line continuations
   *.hook  text eol=lf
   *.py    text eol=lf      # plugin entry points, also shebang-executed
   crates/kn9t-server/src/api.rs  text eol=lf   # byte-compared by xtask --check
   crates/kn9t-tui/src/wire.rs    text eol=lf
   API.md                         text eol=lf
   schema/generated/**            text eol=lf
   ```

2. Install the hook via **`core.hooksPath`**, not a copy into `.git/hooks/`, so the
   hook is version-controlled instead of being an untracked artifact every fresh
   clone silently lacks. `scripts/install-hooks.sh` does this in one command.

3. Guard scripts resolve `cargo` through `scripts/_cargo.sh`, which checks `$CARGO`,
   `cargo`, `cargo.exe`, then `*/.cargo/bin/cargo.exe` under the WSL mount, and exits
   **2** when the toolchain is absent. Exit 2 (cannot check) is distinct from exit 1
   (invariant broken): a missing tool must not read as a failing gate.

`* text=auto` stays as the default for everything else.

## Consequences

- All six guard scripts run on a Windows checkout with `core.autocrlf=true`.
  `scripts/check-ci.sh` passes end-to-end for the first time.
- `xtask --check` reports no drift without any change to the comparison logic — the
  false positive was the line endings, not the generator.
- Renormalizing produced **no content churn**: the index was already LF, confirming
  this was purely a checkout-side defect.
- A fresh clone still has no hook until someone runs `scripts/install-hooks.sh`.
  That step belongs in onboarding; CI runs `check-ci.sh` regardless, so the hook is
  a fast local signal rather than the only line of defence.
- `.githooks/` is generated and therefore git-ignored; `scripts/pre-commit.hook`
  remains the tracked source.

## Lesson

ADR-0007 reached the right diagnosis and stopped one attribute short. The gap was not
in the reasoning but in the verification: nothing ran `bash scripts/check-gi1.sh` after
the change to confirm the gates it named actually worked. Per TRACKING.md:31-35 —
*prefer a script over an assertion* — an ADR that claims a gate now works should show
the gate's exit code.
