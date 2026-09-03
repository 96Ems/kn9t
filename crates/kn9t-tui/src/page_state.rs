//! 96E-24/96E-25 — TUI-side page state (plugin/session pages, no server dep).
//!
//! Mirrors `crates/kn9t-server/src/ui_pages.rs` kinds but purely for rendering.

use std::collections::HashMap;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum PlaceholderKind {
    Text,
    Number,
    Bar,
    List,
}

impl PlaceholderKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Self::Text),
            "number" => Some(Self::Number),
            "bar" => Some(Self::Bar),
            "list" => Some(Self::List),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Bar => "bar",
            Self::List => "list",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Placeholder {
    pub kind: PlaceholderKind,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct UiPage {
    pub plugin: String,
    pub page_id: String,
    pub order: Vec<String>,
    pub placeholders: HashMap<String, Placeholder>,
}

impl UiPage {
    pub fn new(plugin: String, page_id: String, order: Vec<String>, placeholders: HashMap<String, Placeholder>) -> Self {
        Self { plugin, page_id, order, placeholders }
    }
}

pub type PageKey = (String, String); // (plugin, page_id)

/// Apply a declare_page UiDirective payload to the pages map.
/// `layout` is Value::Array of {placeholder_id|id, kind, default?}.
pub fn apply_declare(pages: &mut HashMap<PageKey, UiPage>, plugin: &str, page_id: &str, layout: &Value) -> Result<(), String> {
    let key = (plugin.to_string(), page_id.to_string());
    if pages.contains_key(&key) {
        return Err(format!("page {page_id:?} already declared for {plugin:?}"));
    }
    let arr = layout.as_array().ok_or("layout must be array")?;
    let mut placeholders = HashMap::new();
    let mut order = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in arr {
        let obj = item.as_object().ok_or("layout entry must be object")?;
        let pid = obj.get("placeholder_id").or_else(|| obj.get("id")).and_then(|v| v.as_str()).ok_or("placeholder_id required")?;
        if !seen.insert(pid.to_string()) { return Err(format!("duplicate placeholder {pid:?}")); }
        let kind_str = obj.get("kind").and_then(|v| v.as_str()).ok_or(format!("kind required for {pid:?}"))?;
        let kind = PlaceholderKind::parse(kind_str).ok_or(format!("unknown kind {kind_str:?}"))?;
        let default = obj.get("default").cloned().unwrap_or_else(|| match &kind {
            PlaceholderKind::Text => Value::String(String::new()),
            PlaceholderKind::Number => Value::Number(serde_json::Number::from(0)),
            PlaceholderKind::Bar => Value::Number(serde_json::Number::from(0)),
            PlaceholderKind::List => Value::Array(vec![]),
        });
        placeholders.insert(pid.to_string(), Placeholder { kind, value: default });
        order.push(pid.to_string());
    }
    pages.insert(key, UiPage::new(plugin.to_string(), page_id.to_string(), order, placeholders));
    Ok(())
}

pub fn apply_write(pages: &mut HashMap<PageKey, UiPage>, plugin: &str, page_id: &str, placeholder_id: &str, value: Value) -> Result<(), String> {
    let key = (plugin.to_string(), page_id.to_string());
    let page = pages.get_mut(&key).ok_or_else(|| format!("page {page_id:?} not declared for {plugin:?}"))?;
    let ph = page.placeholders.get_mut(placeholder_id).ok_or_else(|| format!("placeholder {placeholder_id:?} not declared"))?;
    // Validate value matches kind (same as server)
    let valid = match &ph.kind {
        PlaceholderKind::Text => value.is_string(),
        PlaceholderKind::Number => value.is_number(),
        PlaceholderKind::Bar => value.as_f64().map(|n| (0.0..=100.0).contains(&n)).unwrap_or(false),
        PlaceholderKind::List => value.is_array(),
    };
    if !valid {
        return Err(format!("value for {placeholder_id:?} must be {}", ph.kind.as_str()));
    }
    ph.value = value;
    Ok(())
}

pub fn apply_clear(pages: &mut HashMap<PageKey, UiPage>, plugin: &str, page_id: &str) -> Result<(), String> {
    let key = (plugin.to_string(), page_id.to_string());
    if pages.remove(&key).is_some() { Ok(()) } else { Err(format!("page {page_id:?} not declared")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn declare_and_write_and_clear() {
        let mut pages = HashMap::new();
        let layout = json!([{"placeholder_id":"status","kind":"text","default":"idle"}, {"placeholder_id":"prog","kind":"bar","default":0}]);
        apply_declare(&mut pages, "p", "dash", &layout).unwrap();
        assert_eq!(pages.len(), 1);
        apply_write(&mut pages, "p", "dash", "prog", json!(42)).unwrap();
        assert_eq!(pages.get(&("p".to_string(),"dash".to_string())).unwrap().placeholders.get("prog").unwrap().value, json!(42));
        apply_clear(&mut pages, "p", "dash").unwrap();
        assert!(pages.is_empty());
    }

    #[test]
    fn wrong_type_rejected() {
        let mut pages = HashMap::new();
        let layout = json!([{"placeholder_id":"n","kind":"number"}]);
        apply_declare(&mut pages, "p", "pg", &layout).unwrap();
        assert!(apply_write(&mut pages, "p", "pg", "n", json!("not a number")).is_err());
    }

    #[test]
    fn cross_plugin_isolated() {
        let mut pages = HashMap::new();
        let layout = json!([{"placeholder_id":"x","kind":"text"}]);
        apply_declare(&mut pages, "p1", "shared", &layout).unwrap();
        // p2 cannot write to p1's page (key differs)
        assert!(apply_write(&mut pages, "p2", "shared", "x", json!("hi")).is_err());
    }
}
