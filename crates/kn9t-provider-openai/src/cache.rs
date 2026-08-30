//! R-OAI-040 — cache encoding for OpenAI family.

use kn9t_provider_core::CacheMode;

/// Under Automatic: send NO cache fields (R-OAI-040).
/// Under Explicit: provider attaches cache_control at named positions.
pub fn should_send_cache_fields(mode: &CacheMode) -> bool {
    matches!(mode, CacheMode::Explicit { .. })
}

/// For Explicit mode: attach `cache_control: { type: "ephemeral" }` at a given JSON object.
pub fn attach_cache_control(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.insert(
        "cache_control".to_owned(),
        serde_json::json!({ "type": "ephemeral" }),
    );
}
