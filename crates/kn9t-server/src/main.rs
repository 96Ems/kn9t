//! `kn9t serve` — the server process entry point (DESIGN §12, §14).

use std::sync::Arc;
use kn9t_server::{auth, config, log, spawn, ServerHandle, ServerState};

fn main() {
    // Init log file before anything else so even startup errors are captured.
    let log_path = auth::kn9t_home().join("server.log");
    log::init(&log_path);

    // Install a panic hook that writes the panic to the log file before dying.
    std::panic::set_hook(Box::new(|info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            format!("PANIC: {s} at {:?}", info.location())
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            format!("PANIC: {s} at {:?}", info.location())
        } else {
            format!("PANIC at {:?}", info.location())
        };
        kn9t_server::log::write(&msg);
    }));

    if let Err(e) = run() {
        kn9t_server::log!("fatal: {e}");
        std::process::exit(1);
    }
}

fn run() -> std::io::Result<()> {
    kn9t_server::log!("kn9t-server starting");

    // ── Config ───────────────────────────────────────────────────────────────
    let cfg_path = config::global_config_path();
    kn9t_server::log!("loading config from {}", cfg_path.display());

    let resolved = config::load(&cfg_path).unwrap_or_else(|e| {
        kn9t_server::log!("config warning: {e}; starting with no provider");
        config::ResolvedConfig {
            providers: Vec::new(),
            models: Vec::new(),
            default_model_id: None,
            idle_exit: None,
            policy_mode: config::PolicyMode::default(),
            plugins: Vec::new(),
        }
    });

    if resolved.providers.is_empty() {
        kn9t_server::log!(
            "no providers loaded — turns will be no-ops until {} is created",
            cfg_path.display()
        );
    } else {
        kn9t_server::log!("{} provider(s), {} model(s) loaded",
            resolved.providers.len(), resolved.models.len());
    }

    // ── Store ─────────────────────────────────────────────────────────────────
    let store = kn9t_store::SqliteStore::open_default()
        .map_err(|e| std::io::Error::other(format!("open store: {e}")))?;
    kn9t_server::log!("store opened at {}", store.path().display());
    let store = Arc::new(store);

    // ── Auth token ────────────────────────────────────────────────────────────
    let token = auth::generate_token();
    auth::write_token(&auth::token_path(), &token)?;

    // ── Spawn tool plugins (R-PLUG2-110: auto-discovered in ~/.kn9t/plugins/ + pinned [[plugin]]) ──
    let (plugin_hosts, tools, spawn_info) = kn9t_server::tools::spawn_all_plugins_with_info(&resolved.plugins, store.clone())
        .map_err(|e| std::io::Error::other(format!("tools plugin: {e}")))?;

    // ── Build ServerState ─────────────────────────────────────────────────────
    let mut state = ServerState::new(store.clone(), token, tools, plugin_hosts);
    // Record spawn recipes for hot-reload (R-PLUG2-100).
    for (name, (cmd, env)) in spawn_info {
        state.set_plugin_spawn(name, cmd, env);
    }
    // ADR-0008: policy decisions moved to plugin. Log mode for info.
    kn9t_server::log!("policy: mode={:?} (ADR-0008: plugin decides)", resolved.policy_mode);

    if let Some(idle) = resolved.idle_exit {
        if idle.is_zero() {
            kn9t_server::log!("idle-exit: disabled by config");
        } else {
            kn9t_server::log!("idle-exit: {}s grace after last client disconnects (from config)", idle.as_secs());
        }
        state = state.with_idle_exit(idle);
    } else {
        kn9t_server::log!("idle-exit: {}s grace after last client disconnects (default)", kn9t_server::DEFAULT_IDLE_EXIT.as_secs());
    }

    // Store all providers for model switching.
    state = state.with_providers(resolved.providers.clone());
    
    // Default model: explicit config > first "small" model (haiku) > first model.
    // Titling uses the default, so prefer a cheap model to avoid burning tokens.
    let default_spec = resolved.default_model_id.as_ref()
        .and_then(|id| resolved.models.iter().find(|m| &m.r#ref.id == id).cloned())
        .or_else(|| resolved.models.iter().find(|m| is_small_model(&m.r#ref.id)).cloned())
        .or_else(|| resolved.models.first().cloned());

    if let Some(spec) = &default_spec {
        kn9t_server::log!("default model: {}:{}", spec.r#ref.provider, spec.r#ref.id);
        let provider = resolved.providers.iter()
            .find(|(name, _)| name == &spec.r#ref.provider)
            .map(|(_, p)| p.clone());
        if let Some(p) = provider { state = state.with_provider(p); }
        state = state.with_default_model(spec.clone());
    }
    state.model_registry = resolved.models.clone();
    
    // Register all models with the store so get_model_spec_for_session can find them.
    for spec in &resolved.models {
        store.register_model_spec(spec.clone());
    }
    
    let state = Arc::new(state);
    state.install_host_api();
    state.install_builtin_tools();

    // ── Bind + start ──────────────────────────────────────────────────────────
    let handle = ServerHandle::spawn(state)?;
    spawn::write_port(&auth::port_path(), handle.port)?;
    kn9t_server::log!("listening on 127.0.0.1:{}", handle.port);

    handle.wait();

    let _ = std::fs::remove_file(auth::port_path());
    kn9t_server::log!("idle-exit");
    Ok(())
}

/// Returns true if the model ID suggests a small/cheap model (haiku, mini, flash).
/// Used to auto-select a default for titling when no explicit default is configured.
fn is_small_model(id: &str) -> bool {
    let id_lower = id.to_lowercase();
    id_lower.contains("haiku")
        || id_lower.contains("mini")
        || id_lower.contains("flash")
        || id_lower.contains("small")
}
