//! Generator for `.agents/skills/kn9t-plugin-creation/references/`.
//!
//! The skill used to restate the wire protocol, the hook table and the host-API op
//! list by hand, in parallel with `API.md`. It drifted twice: the guide listed 5 of
//! the 20 host-API ops, then 9 of 20. A hand-maintained copy of a generated
//! contract is a copy that will be wrong; the fix is to derive it.
//!
//! So `references/` holds generated snapshots and nothing else:
//!
//! | output | source |
//! |---|---|
//! | `references/api.md` | `API.md` — the same bytes, so the skill cannot disagree with the contract |
//! | `references/sdk/**` | `crates/kn9t-plugin-sdk` (`Cargo.toml` + `src/**`), so an agent without the repo has the exact API |
//!
//! `SKILL.md` stays hand-written: it is guidance, not data. `xtask --check` fails on any
//! drift, so editing the SDK without regenerating the skill is a red gate, not a silent
//! stale copy.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::gen_markdown;

const SKILL_DIR: &str = ".agents/skills/kn9t-plugin-creation";
const SDK_DIR: &str = "crates/kn9t-plugin-sdk";

const README: &str = "<!-- GENERATED FILE — do not edit by hand. -->
<!-- Regenerate with: cargo run -p xtask -- generate -->

# references

Generated snapshots of the contract. Do not edit anything in this directory: an edit is
caught by `xtask --check` (pre-commit + CI) and overwritten by the next `generate`.

| file | source of truth |
|---|---|
| `api.md` | `schema/http.json` + `schema/plugin.json`, rendered by `xtask` |
| `sdk/` | `crates/kn9t-plugin-sdk` — the Rust SDK, byte-for-byte |

`SKILL.md` is hand-written and sits next to this directory.
";

/// Every file this generator produces, as (absolute path, exact content).
pub fn outputs(root: &Path, http: &Value, plugin: &Value) -> Result<Vec<(PathBuf, String)>, String> {
    let base = root.join(SKILL_DIR).join("references");
    let mut out: Vec<(PathBuf, String)> = Vec::new();

    out.push((base.join("api.md"), gen_markdown::generate(http, plugin)?));
    out.push((base.join("README.md"), README.to_string()));

    for rel in sdk_sources(root)? {
        let content = std::fs::read_to_string(root.join(SDK_DIR).join(&rel))
            .map_err(|e| format!("read {}: {e}", rel.display()))?;
        out.push((base.join("sdk").join(rel), content));
    }

    Ok(out)
}

/// Write every output (called by `xtask generate`).
pub fn write(root: &Path, http: &Value, plugin: &Value) -> Result<(), String> {
    for (path, content) in outputs(root, http, plugin)? {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, content.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// The SDK files worth shipping: the manifest and every `src/**` module, sorted for a
/// deterministic check. Tests are excluded — the skill needs the API, not the SDK's
/// own suite.
fn sdk_sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    let sdk = root.join(SDK_DIR);
    let mut out = vec![PathBuf::from("Cargo.toml")];

    let src = sdk.join("src");
    let mut rust = Vec::new();
    collect_rust(&src, &src, &mut rust)?;
    rust.sort();
    out.extend(rust.into_iter().map(|p| PathBuf::from("src").join(p)));

    Ok(out)
}

fn collect_rust(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust(base, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?;
            out.push(rel.to_path_buf());
        }
    }
    Ok(())
}
