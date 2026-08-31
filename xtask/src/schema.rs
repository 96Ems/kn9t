//! Schema navigation helpers shared by all generators.
//!
//! All iteration is over the schema's own ordering (serde_json is built here with
//! `preserve_order` — a generator-only feature, see `xtask/Cargo.toml`), so every
//! output is deterministic and byte-stable across runs.

use serde_json::{Map, Value};

/// Rust struct name for a route's **request** type. Only POST routes with a JSON
/// object body appear here; a route missing from this map is a schema bug that the
/// generator surfaces loudly instead of silently emitting nothing.
pub fn req_name_for_path(path: &str) -> Option<&'static str> {
    match path {
        "/session" => Some("CreateSessionReq"),
        "/session/{id}/fork" => Some("ForkReq"),
        "/session/{id}/prompt" => Some("PromptReq"),
        "/session/{id}/steer" => Some("SteerReq"),
        "/session/{id}/model" => Some("SetModelReq"),
        "/session/{id}/rename" => Some("RenameReq"),
        "/session/{id}/compact" => Some("CompactReq"),
        "/approve" => Some("ApproveReq"),
        _ => None,
    }
}

/// A route entry as a lightweight struct (method, path, lease, and the request /
/// response / query subschemas).
pub struct Route<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub lease: bool,
    pub description: Option<&'a str>,
    pub request: Option<&'a Value>,
    pub response: Option<&'a Value>,
    pub query: Option<&'a Value>,
}

impl<'a> Route<'a> {
    /// POST routes whose request consumes a JSON object body (the typed set).
    pub fn request_object(&self) -> Option<&'a Value> {
        let req = self.request?;
        if req.get("type").and_then(|t| t.as_str()) == Some("object") {
            Some(req)
        } else {
            None
        }
    }

    /// Routes with a JSON object response (leaf types like raw binary are skipped).
    pub fn response_object(&self) -> Option<&'a Value> {
        let resp = self.response?;
        if resp.get("type").and_then(|t| t.as_str()) == Some("object") {
            Some(resp)
        } else {
            None
        }
    }
}

/// The ordered `routes` array from `schema/http.json`.
pub fn routes(http: &Value) -> Vec<Route<'_>> {
    http.get("routes")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| Route {
                    method: r.get("method").and_then(|m| m.as_str()).unwrap_or(""),
                    path: r.get("path").and_then(|p| p.as_str()).unwrap_or(""),
                    lease: r.get("lease").and_then(|l| l.as_bool()).unwrap_or(false),
                    description: r.get("description").and_then(|d| d.as_str()),
                    request: r.get("request"),
                    response: r.get("response"),
                    query: r.get("query"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ordered property map of an object subschema.
pub fn properties<'a>(obj: &'a Value) -> Vec<(String, &'a Value)> {
    obj.get("properties")
        .and_then(|p| p.as_object())
        .map(props_in_order)
        .unwrap_or_default()
}

fn props_in_order(map: &Map<String, Value>) -> Vec<(String, &Value)> {
    map.iter().map(|(k, v)| (k.clone(), v)).collect()
}

/// The `required` array of an object subschema (order preserved).
pub fn required(obj: &Value) -> Vec<String> {
    obj.get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

/// The well-known `{provider, id}` model-reference shape.
pub fn is_model_ref(prop: &Value) -> bool {
    let map = match prop.get("properties").and_then(|p| p.as_object()) {
        Some(m) => m,
        None => return false,
    };
    if map.len() != 2 {
        return false;
    }
    let str_prop = |v: Option<&Value>| {
        matches!(v, Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("string"))
    };
    str_prop(map.get("provider")) && str_prop(map.get("id"))
}

/// Map a JSON Schema type to a Rust type. `model_ref_ty` names the type generated
/// for the well-known `{provider, id}` object shape. `nullable` (a `["type","null"]`
/// union) yields `Option<T>`.
pub fn rust_type(prop: &Value, model_ref_ty: &str) -> String {
    let mut nullable = false;
    let mut prim: Option<&str> = None;
    match prop.get("type") {
        Some(t) if t.is_string() => prim = t.as_str(),
        Some(t) if t.is_array() => {
            for member in t.as_array().into_iter().flatten() {
                match member.as_str() {
                    Some("null") => nullable = true,
                    Some(other) => prim = Some(other),
                    None => {}
                }
            }
        }
        _ => {}
    }

    let base = match prim {
        Some("string") => "String".to_string(),
        Some("integer") => "u64".to_string(),
        Some("number") => "f64".to_string(),
        Some("boolean") => "bool".to_string(),
        Some("array") => {
            let item_ty = prop
                .get("items")
                .and_then(|i| i.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("object");
            match item_ty {
                "string" => "Vec<String>".to_string(),
                _ => "Vec<serde_json::Value>".to_string(),
            }
        }
        Some("object") if is_model_ref(prop) => model_ref_ty.to_string(),
        Some("object") => "serde_json::Value".to_string(),
        _ => "serde_json::Value".to_string(),
    };

    if nullable {
        format!("Option<{base}>")
    } else {
        base
    }
}

/// Rust type for a property, honoring requiredness: an optional non-nullable field
/// is wrapped in `Option`, everything else passes through `rust_type`.
pub fn prop_type(prop: &Value, required: &[String], key: &str, model_ref_ty: &str) -> String {
    let ty = rust_type(prop, model_ref_ty);
    if required.iter().any(|r| r.as_str() == key) || ty.starts_with("Option<") {
        ty
    } else {
        format!("Option<{ty}>")
    }
}

/// Human-readable type for API.md tables (e.g. `string`, `u64`, `string[]`, `ModelRef`).
/// Nullable unions (schema `["type","null"]`) render as `inner \| null`.
pub fn markdown_type(prop: &Value) -> String {
    let t = rust_type(prop, "ModelRef");
    let compact = match t.as_str() {
        "String" => "string".to_string(),
        "u64" => "u64".to_string(),
        "f64" => "number".to_string(),
        "bool" => "bool".to_string(),
        "Vec<String>" => "string[]".to_string(),
        "Vec<serde_json::Value>" => "object[]".to_string(),
        "serde_json::Value" => "object".to_string(),
        other => other.to_string(),
    };
    if let Some(inner) = compact.strip_prefix("Option<") {
        format!("{} \\| null", inner.trim_end_matches('>'))
    } else {
        compact
    }
}

/// The SSE event catalogue (`schema/http.json` → `sse.events`): kind + field map.
pub struct SseEvent<'a> {
    pub kind: &'a str,
    pub fields: Vec<(String, String)>,
}

pub fn sse_events(http: &Value) -> Vec<SseEvent<'_>> {
    http.get("sse")
        .and_then(|s| s.get("events"))
        .and_then(|e| e.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|ev| {
                    let kind = ev.get("kind").and_then(|k| k.as_str())?;
                    let fields = ev
                        .as_object()
                        .map(|o| {
                            o.iter()
                                .filter(|(k, _)| k.as_str() != "kind")
                                .map(|(k, v)| {
                                    (k.clone(), v.as_str().unwrap_or("Value").to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(SseEvent { kind, fields })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `message_appended` → `MessageAppended` (CamelCase variant name).
pub fn to_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = true;
    for c in snake.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}