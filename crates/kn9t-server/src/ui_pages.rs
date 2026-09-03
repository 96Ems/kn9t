//! 96E-24 — templated page primitive with writable placeholders.
//!
//! A page belongs to (plugin, session) and lives entirely in-memory (transient).
//! Layout is declared once; placeholders are updated cheaply without re-sending
//! the whole page. Host validates placeholder existence and value kind.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
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
            "text" => Some(PlaceholderKind::Text),
            "number" => Some(PlaceholderKind::Number),
            "bar" => Some(PlaceholderKind::Bar),
            "list" => Some(PlaceholderKind::List),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            PlaceholderKind::Text => "text",
            PlaceholderKind::Number => "number",
            PlaceholderKind::Bar => "bar",
            PlaceholderKind::List => "list",
        }
    }
    pub fn is_valid_value(&self, v: &Value) -> bool {
        match self {
            PlaceholderKind::Text => v.is_string(),
            PlaceholderKind::Number => v.is_number(),
            PlaceholderKind::Bar => {
                if let Some(n) = v.as_f64() { (0.0..=100.0).contains(&n) } else { false }
            }
            PlaceholderKind::List => v.is_array(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PlaceholderDef {
    pub id: String,
    pub kind: PlaceholderKind,
    pub default: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct UiPage {
    pub plugin: String,
    pub session: String,
    pub page_id: String,
    pub placeholders: HashMap<String, PlaceholderDef>,
    pub order: Vec<String>,
    pub values: HashMap<String, Value>,
}

pub struct UiPageRegistry {
    pages: Mutex<HashMap<(String, String, String), UiPage>>, // (session, plugin, page_id)
}

impl UiPageRegistry {
    pub fn new() -> Self {
        Self { pages: Mutex::new(HashMap::new()) }
    }

    fn key(session: &str, plugin: &str, page_id: &str) -> (String, String, String) {
        (session.to_string(), plugin.to_string(), page_id.to_string())
    }

    /// Declare a page. `layout` is an array of {placeholder_id|id, kind, default?}.
    pub fn declare(&self, plugin: &str, session: &str, page_id: &str, layout_val: &Value) -> Result<(), String> {
        if page_id.is_empty() { return Err("page_id must be non-empty".into()); }
        let arr = layout_val.as_array().ok_or_else(|| "layout must be an array".to_string())?;
        if arr.is_empty() { return Err("layout must be non-empty".into()); }
        let mut placeholders = HashMap::new();
        let mut order = Vec::new();
        let mut seen = HashSet::new();
        for item in arr {
            let obj = item.as_object().ok_or_else(|| "layout entry must be an object".to_string())?;
            let pid = obj.get("placeholder_id").or_else(|| obj.get("id")).and_then(|v| v.as_str()).ok_or_else(|| "layout entry requires \"placeholder_id\" or \"id\"".to_string())?;
            if pid.is_empty() { return Err("placeholder_id must be non-empty".into()); }
            if !seen.insert(pid.to_string()) { return Err(format!("duplicate placeholder_id {pid:?}")); }
            let kind_str = obj.get("kind").and_then(|v| v.as_str()).ok_or_else(|| format!("placeholder {pid:?} requires \"kind\" (text|number|bar|list)"))?;
            let kind = PlaceholderKind::parse(kind_str).ok_or_else(|| format!("unknown kind {kind_str:?} for {pid:?}"))?;
            let default = obj.get("default").cloned();
            if let Some(ref d) = default {
                if !kind.is_valid_value(d) {
                    return Err(format!("default for {pid:?} does not match kind {}: got {d}", kind.as_str()));
                }
            }
            let def = PlaceholderDef { id: pid.to_string(), kind, default: default.clone() };
            placeholders.insert(pid.to_string(), def);
            order.push(pid.to_string());
        }
        let k = Self::key(session, plugin, page_id);
        let mut pages = self.pages.lock().expect("ui_pages poisoned");
        if pages.contains_key(&k) {
            return Err(format!("page {page_id:?} already declared for plugin {plugin:?} session {session:?}"));
        }
        let mut values = HashMap::new();
        for (pid, def) in &placeholders {
            if let Some(d) = &def.default {
                values.insert(pid.clone(), d.clone());
            }
        }
        pages.insert(k, UiPage { plugin: plugin.to_string(), session: session.to_string(), page_id: page_id.to_string(), placeholders, order, values });
        Ok(())
    }

    /// Write a placeholder value.
    pub fn write(&self, plugin: &str, session: &str, page_id: &str, placeholder_id: &str, value: Value) -> Result<(), String> {
        let k = Self::key(session, plugin, page_id);
        let mut pages = self.pages.lock().expect("ui_pages poisoned");
        // Need to check ownership: page must exist under (session,plugin,page_id).
        // If not found but exists under different plugin/session, give specific error.
        if !pages.contains_key(&k) {
            // Check if same page_id exists under different owner for better error
            for ((sess, plug, pid), _) in pages.iter() {
                if pid == page_id {
                    if sess != session {
                        return Err(format!("page {page_id:?} not found in session {session:?} (exists in session {sess:?})"));
                    }
                    if plug != plugin {
                        return Err(format!("page {page_id:?} belongs to plugin {plug:?}, not {plugin:?}"));
                    }
                }
            }
            return Err(format!("page {page_id:?} not declared"));
        }
        let page = pages.get_mut(&k).unwrap();
        let def = page.placeholders.get(placeholder_id).ok_or_else(|| format!("placeholder {placeholder_id:?} not declared in page {page_id:?}"))?;
        if !def.kind.is_valid_value(&value) {
            return Err(format!("value for {placeholder_id:?} must be {} (kind {})", match def.kind { PlaceholderKind::Text => "string", PlaceholderKind::Number => "number", PlaceholderKind::Bar => "number 0..100", PlaceholderKind::List => "array" }, def.kind.as_str()));
        }
        page.values.insert(placeholder_id.to_string(), value);
        Ok(())
    }

    /// Clear a page (owner only).
    pub fn clear(&self, plugin: &str, session: &str, page_id: &str) -> Result<(), String> {
        let k = Self::key(session, plugin, page_id);
        let mut pages = self.pages.lock().expect("ui_pages poisoned");
        if pages.contains_key(&k) {
            pages.remove(&k);
            Ok(())
        } else {
            // Check cross-owner for better error
            for ((sess, plug, pid), _) in pages.iter() {
                if pid == page_id {
                    if sess != session {
                        return Err(format!("page {page_id:?} not found in session {session:?}"));
                    }
                    if plug != plugin {
                        return Err(format!("page {page_id:?} belongs to plugin {plug:?}, not {plugin:?}"));
                    }
                }
            }
            Err(format!("page {page_id:?} not declared"))
        }
    }

    /// Remove all pages for a session (teardown).
    pub fn clear_session(&self, session: &str) {
        let mut pages = self.pages.lock().expect("ui_pages poisoned");
        pages.retain(|(sess, _, _), _| sess != session);
    }

    /// Remove all pages for a plugin (unload).
    pub fn clear_plugin(&self, plugin: &str) {
        let mut pages = self.pages.lock().expect("ui_pages poisoned");
        pages.retain(|(_, plug, _), _| plug != plugin);
    }

    /// For tests: get snapshot.
    pub fn get(&self, plugin: &str, session: &str, page_id: &str) -> Option<UiPage> {
        let pages = self.pages.lock().expect("ui_pages poisoned");
        pages.get(&Self::key(session, plugin, page_id)).cloned()
    }

    pub fn count(&self) -> usize {
        self.pages.lock().expect("ui_pages poisoned").len()
    }
}

impl Default for UiPageRegistry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn placeholder_kind_validation() {
        assert!(PlaceholderKind::Text.is_valid_value(&json!("hello")));
        assert!(!PlaceholderKind::Text.is_valid_value(&json!(42)));
        assert!(PlaceholderKind::Number.is_valid_value(&json!(3.14)));
        assert!(PlaceholderKind::Bar.is_valid_value(&json!(50)));
        assert!(!PlaceholderKind::Bar.is_valid_value(&json!(150)));
        assert!(PlaceholderKind::List.is_valid_value(&json!([1,2])));
    }
}
