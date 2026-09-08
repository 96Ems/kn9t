//! `kn9t serve` — the server process entry point (DESIGN §12, §14).
//!
//! Non-blocking startup: the HTTP server binds and writes the port file immediately,
//! then plugins load in a background thread. Session creation and prompts are blocked
//! (503) until all plugins are ready. This prevents client timeouts when plugins are
//! slow to load (e.g., WSL with files on /mnt/c/).

use kn9t_core::ToolRegistry;
use kn9t_server::{auth, config, log, spawn, ServerHandle, ServerState};
use std::sync::Arc;

fn main() {
    let log_path = auth::kn9t_home().join("server.log");
    log::init(&log_path);

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
        eprintln!("\n[kn9t] Configuration error:\n{e}\n");
        config::ResolvedConfig {
            providers: Vec::new(),
            provider_hosts: Vec::new(),
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
        kn9t_server::log!(
            "{} provider(s), {} model(s) loaded",
            resolved.providers.len(),
            resolved.models.len()
        );
    }

    // ── Store ─────────────────────────────────────────────────────────────────
    let store = kn9t_store::SqliteStore::open_default()
        .map_err(|e| std::io::Error::other(format!("open store: {e}")))?;
    kn9t_server::log!("store opened at {}", store.path().display());
    let store = Arc::new(store);

    // ── Auth token ────────────────────────────────────────────────────────────
    let token = auth::generate_token();
    auth::write_token(&auth::token_path(), &token)?;

    // ── Build ServerState with EMPTY tools/hosts (non-blocking startup) ───────
    // Plugins load in background; session creation blocked until ready.
    let mut state = ServerState::new(store.clone(), token, ToolRegistry::new(), Vec::new());
    state.set_plugins_loading(true);

    // ADR-0008: policy decisions moved to plugin. Log mode for info.
    kn9t_server::log!(
        "policy: mode={:?} (ADR-0008: plugin decides)",
        resolved.policy_mode
    );

    if let Some(idle) = resolved.idle_exit {
        if idle.is_zero() {
            kn9t_server::log!("idle-exit: disabled by config");
        } else {
            kn9t_server::log!(
                "idle-exit: {}s grace after last client disconnects (from config)",
                idle.as_secs()
            );
        }
        state = state.with_idle_exit(idle);
    } else {
        kn9t_server::log!(
            "idle-exit: {}s grace after last client disconnects (default)",
            kn9t_server::DEFAULT_IDLE_EXIT.as_secs()
        );
    }

    // Store all providers for model switching.
    state = state.with_providers(resolved.providers.clone());

    // Default model: explicit config > first "small" model (haiku) > first model.
    let default_spec = config::pick_default_model(&resolved);

    if let Some(spec) = &default_spec {
        kn9t_server::log!("default model: {}:{}", spec.r#ref.provider, spec.r#ref.id);
        let provider = resolved
            .providers
            .iter()
            .find(|(name, _)| name == &spec.r#ref.provider)
            .map(|(_, p)| p.clone());
        if let Some(p) = provider {
            state = state.with_provider(p);
        }
        state = state.with_default_model(spec.clone());
    }
    state.set_models(resolved.models.clone());
    *state
        .provider_hosts
        .lock()
        .expect("provider_hosts poisoned") = resolved.provider_hosts.clone();

    // Register all models with the store so get_model_spec_for_session can find them.
    for spec in &resolved.models {
        store.register_model_spec(spec.clone());
    }

    let state = Arc::new(state);

    // ── Config watcher ────────────────────────────────────────────────────────
    kn9t_server::watch::spawn_config_watcher(state.clone(), cfg_path.clone());

    // ── Bind + start HTTP server IMMEDIATELY ──────────────────────────────────
    // Port is written before plugins load, so client never times out waiting.
    let handle = ServerHandle::spawn(state.clone())?;
    spawn::write_port(&auth::port_path(), handle.port)?;
    kn9t_server::log!("listening on 127.0.0.1:{}", handle.port);

    // ── Spawn plugins in background thread ────────────────────────────────────
    // Session creation returns 503 until this completes.
    let plugins_config = resolved.plugins.clone();
    let store_for_plugins = store.clone();
    let state_for_plugins = state.clone();
    std::thread::spawn(move || {
        load_plugins_background(plugins_config, store_for_plugins, state_for_plugins);
    });

    handle.wait();

    let _ = std::fs::remove_file(auth::port_path());
    kn9t_server::log!("idle-exit");
    Ok(())
}

/// Background plugin loading. Updates state atomically when complete.
fn load_plugins_background(
    plugins_config: Vec<config::ResolvedPlugin>,
    store: Arc<kn9t_store::SqliteStore>,
    state: Arc<ServerState>,
) {
    match kn9t_server::tools::spawn_all_plugins_with_info(&plugins_config, store) {
        Ok((plugin_hosts, tools, spawn_info)) => {
            let tool_count = tools.len();

            // Update state with loaded plugins (order matters for install_* calls)
            *state.tools.lock().expect("tools poisoned") = tools;
            *state.plugin_hosts.lock().expect("plugin_hosts poisoned") = plugin_hosts;

            // Record spawn recipes for hot-reload (R-PLUG2-100).
            for (name, (cmd, env)) in spawn_info {
                state.set_plugin_spawn(name, cmd, env);
            }

            // Install host API handlers AFTER hosts are in the state.
            // This allows plugins to call session_read, ui_register_lua, etc.
            state.install_host_api();
            state.install_declare_callbacks();

            kn9t_server::log!("plugins ready: {} tools registered", tool_count);
        }
        Err(e) => {
            kn9t_server::log!("plugin loading failed: {e}");
            // Still mark as ready so server doesn't hang forever, but with 0 tools.
            // The user can hot-reload plugins later.
        }
    }
    state.set_plugins_loading(false);
}
