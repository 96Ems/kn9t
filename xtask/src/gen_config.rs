//! Generator for `docs/CONFIG.md` — the `~/.kn9t/config.toml` reference.
//!
//! Derived entirely from `schema/config.json` (ADR-0005). Like `API.md`, the output is a
//! **generated file**: `scripts/check-schema.sh` (via `xtask --check`) fails on any drift, so
//! it can never disagree with the schema. The parser in `crates/kn9t-server/src/config.rs` is
//! authoritative for behaviour (R-TUI-012); this document is the schema, rendered.

use std::path::Path;

use serde_json::Value;

const GENERATED_BANNER: &str = "> **GENERATED FILE — do not edit by hand.** Regenerate with
> `cargo run -p xtask -- generate`. Source of truth: `schema/config.json` (ADR-0005).
> The parser in `crates/kn9t-server/src/config.rs` is the authoritative implementation
> (R-TUI-012); this document is derived from the schema and cannot drift from it. Any
> mismatch is a bug in the schema or the parser, not in this file.

";

pub fn write(root: &Path, config: &Value) -> Result<(), String> {
    let out = generate(config)?;
    let path = root.join("docs/CONFIG.md");
    std::fs::write(&path, out.as_bytes()).map_err(|e| format!("write docs/CONFIG.md: {e}"))?;
    Ok(())
}

pub fn generate(config: &Value) -> Result<String, String> {
    let groups = config
        .get("groups")
        .and_then(Value::as_array)
        .ok_or("schema/config.json: missing 'groups' array")?;
    if groups.is_empty() {
        return Err("schema/config.json: 'groups' is empty".into());
    }
    let file = config
        .get("file")
        .and_then(Value::as_str)
        .ok_or("schema/config.json: missing 'file'")?;

    let mut s = String::new();
    s.push_str(&format!("# kn9t Configuration Reference — `{file}`\n\n"));
    s.push_str(GENERATED_BANNER);

    // Contents — keeps the long reference navigable.
    s.push_str("## Contents\n\n");
    for group in groups {
        let heading = group
            .get("heading")
            .and_then(Value::as_str)
            .ok_or("schema/config.json: a group is missing 'heading'")?;
        s.push_str(&format!("- {heading}\n"));
    }
    s.push('\n');

    for group in groups {
        let heading = group
            .get("heading")
            .and_then(Value::as_str)
            .ok_or("schema/config.json: a group is missing 'heading'")?;
        let table = group.get("table").and_then(Value::as_str).unwrap_or("");
        let description = group
            .get("description")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("group {heading}: missing 'description'"))?;
        let fields = group
            .get("fields")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("group {heading}: missing 'fields' array"))?;
        if fields.is_empty() {
            return Err(format!("group {heading}: 'fields' is empty"));
        }

        s.push_str(&format!("## {heading}\n\n"));
        s.push_str(description);
        s.push_str("\n\n");
        if !table.is_empty() {
            s.push_str(&format!("*Table: `{table}`*\n\n"));
        }
        s.push_str("| Key | Type | Default | Required | Description |\n");
        s.push_str("|-----|------|---------|----------|-------------|\n");
        for field in fields {
            let name = field
                .get("field")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("group {heading}: a field is missing 'field'"))?;
            let ty = render_type(field).map_err(|e| format!("group {heading}, key {name}: {e}"))?;
            let default = field.get("default").and_then(Value::as_str).unwrap_or("—");
            let required = match field.get("required").and_then(Value::as_bool) {
                Some(true) => "yes",
                Some(false) => "no",
                None => return Err(format!("group {heading}, key {name}: missing 'required'")),
            };
            let description = field
                .get("description")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("group {heading}, key {name}: missing 'description'"))?;
            s.push_str(&format!(
                "| `{name}` | {ty} | {default} | {required} | {} |\n",
                escape_cell(description)
            ));
        }
        s.push('\n');
    }

    Ok(s)
}

/// Render a field's type cell. Unknown types are a schema bug, surfaced loudly rather than
/// silently rendered as an empty cell.
fn render_type(field: &Value) -> Result<String, String> {
    let ty = field
        .get("type")
        .and_then(Value::as_str)
        .ok_or("missing 'type'")?;
    Ok(match ty {
        "string" => "string".to_string(),
        "integer" => "integer".to_string(),
        "float" => "float".to_string(),
        "boolean" => "boolean".to_string(),
        "array<string>" => "array<string>".to_string(),
        "map<string,string>" => "map<string, string>".to_string(),
        "table" => "table".to_string(),
        "enum" => {
            let values = field
                .get("enum")
                .and_then(Value::as_array)
                .ok_or("type 'enum' without 'enum' array")?;
            let rendered: Vec<String> = values
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(|s| format!("\"{s}\""))
                        .ok_or_else(|| "enum value is not a string".to_string())
                })
                .collect::<Result<_, _>>()?;
            rendered.join(" \\| ")
        }
        other => return Err(format!("unknown type '{other}'")),
    })
}

/// Markdown table cells cannot contain a raw `|`; escape it so descriptions stay readable.
fn escape_cell(text: &str) -> String {
    text.replace('|', "\\|")
}
