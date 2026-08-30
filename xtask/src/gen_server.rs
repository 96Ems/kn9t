//! Generator for `crates/kn9t-server/src/api.rs` — the typed request structs.

use std::path::Path;

use serde_json::Value;

use crate::schema::{prop_type, properties, req_name_for_path, required, routes};

const HEADER: &str = "//! GENERATED FILE — do not edit by hand.
//! Source of truth: `schema/http.json` (ADR-0005). Regenerate with
//! `cargo run -p xtask -- generate`.
//!
//! Typed request bodies for the JSON POST routes. `#[serde(deny_unknown_fields)]`
//! makes a mistyped or unknown field a **400** at parse time instead of the
//! silent-ignore that the old hand-poked `body.get()` routes performed (F6).
//!
//! The server implementation never duplicates these shapes: it deserializes into
//! them via `http_util::parse_json` and any drift is caught by
//! `scripts/check-schema.sh`.

";

pub fn write(root: &Path, http: &Value) -> Result<(), String> {
    let out = generate(http)?;
    let path = root.join("crates/kn9t-server/src/api.rs");
    std::fs::write(&path, out.as_bytes()).map_err(|e| format!("write api.rs: {e}"))?;
    Ok(())
}

/// Emit the whole module as a string (used by `--check` too).
pub fn generate(http: &Value) -> Result<String, String> {
    let mut s = String::new();
    s.push_str(HEADER);
    s.push_str("use serde::Deserialize;\n\n");

    // The well-known `{provider, id}` model reference, shared by every request.
    s.push_str("/// Model reference `{provider, id}`.\n");
    s.push_str("#[derive(Debug, Clone, Deserialize)]\n");
    s.push_str("#[serde(deny_unknown_fields)]\n");
    s.push_str("pub struct ModelRef {\n");
    s.push_str("    pub provider: String,\n");
    s.push_str("    pub id: String,\n");
    s.push_str("}\n\n");

    for route in routes(http) {
        if route.method != "POST" {
            continue;
        }
        let Some(req_obj) = route.request_object() else {
            // POST /blob is a raw binary body — not a JSON object, correctly absent.
            continue;
        };
        let name = req_name_for_path(route.path).ok_or_else(|| {
            format!(
                "schema/http.json: POST {path} has an object request but no codegen \
                 name in xtask (add it to req_name_for_path)",
                path = route.path
            )
        })?;
        s.push_str(&emit_struct(name, req_obj));
        s.push('\n');
    }
    Ok(s)
}

/// Emit one request struct with `deny_unknown_fields`.
fn emit_struct(name: &str, obj: &Value) -> String {
    let props = properties(obj);
    let req = required(obj);
    let mut s = String::new();

    let doc = obj
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("Request body.");
    s.push_str(&format!("/// {doc}\n"));
    s.push_str(&format!("#[derive(Debug, Clone, Deserialize)]\n"));
    s.push_str(&format!("#[serde(deny_unknown_fields)]\n"));
    s.push_str(&format!("pub struct {name} {{\n"));

    for (key, prop) in props {
        let ty = prop_type(prop, &req, &key, "ModelRef");
        if let Some(d) = prop.get("description").and_then(|d| d.as_str()) {
            s.push_str(&format!("    /// {d}\n"));
        }
        if let Some(en) = prop.get("enum").and_then(|e| e.as_array()) {
            let vals: Vec<&str> = en
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            if !vals.is_empty() {
                s.push_str(&format!(
                    "    /// Allowed values: {}.\n",
                    vals.join(" | ")
                ));
            }
        }
        s.push_str(&format!("    pub {key}: {ty},\n"));
    }
    s.push_str("}\n");
    s
}