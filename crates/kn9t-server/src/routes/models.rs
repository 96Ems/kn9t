//! R-SRV-010 — `GET /models`: the resolved model registry plus auth status
//! (DESIGN §12.1, §8.2). Returns all models loaded from `~/.kn9t/config.toml`.

use std::sync::Arc;

use crate::http_util::JsonResp;
use crate::state::ServerState;

pub fn list(state: &Arc<ServerState>) -> JsonResp {
    // Full registry from config (DESIGN §8.2). Snapshot both under their own locks
    // and drop the guards before building the JSON — a hot-reload writer must not
    // wait on response serialisation.
    let registry = state.models_snapshot();
    let default_id = state.default_model_snapshot().map(|d| d.r#ref.id);

    let models: Vec<_> = registry
        .iter()
        .map(|spec| {
            serde_json::json!({
                "provider": spec.r#ref.provider,
                "id":       spec.r#ref.id,
                "api_id":   spec.api_id,
                "ctx_window": spec.ctx_window,
                "max_out":    spec.max_out,
                "price": {
                    "input":       spec.price.input,
                    "output":      spec.price.output,
                    "cache_read":  spec.price.cache_read,
                    "cache_write": spec.price.cache_write,
                },
                "is_default": default_id.as_deref() == Some(spec.r#ref.id.as_str()),
            })
        })
        .collect();

    let provider = state.provider_snapshot();
    let provider_name = provider.as_ref().map(|p| p.name().to_owned());
    JsonResp::ok(serde_json::json!({
        "models": models,
        "auth": {
            "provider": provider_name,
            "authenticated": provider.is_some(),
        },
    }))
}
