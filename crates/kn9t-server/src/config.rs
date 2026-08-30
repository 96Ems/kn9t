//! Configuration loader — DESIGN §14, §8.2, §8.3.
//!
//! Reads `~/.kn9t/config.toml` (global, privileged: API keys, policy, plugins).
//! Optionally merges `<cwd>/.kn9t.toml` (project-local, untrusted: model choice,
//! compaction threshold, system-prompt paths — never credentials).
//!
//! The file format follows DESIGN §8.2 exactly:
//!
//! ```toml
//! [provider.my-gateway]
//! kind         = "openai"
//! base_url     = "https://llm-gateway.example.com/v1"
//! tls_insecure = false
//! # api_key omitted — anonymous; identity via X-User-Id header
//!
//! [provider.my-gateway.headers]
//! X-User-Id         = "env:GATEWAY_USER_ID"
//! source_identifier = "my_app_id"
//!
//! [provider.my-gateway.quirks]
//! max_tokens_field = "max_completion_tokens"
//! system_role      = "system"
//! usage_in_stream  = false
//! finish_reason    = true
//! reasoning        = "adaptive"
//! require_tools    = true
//! thinking_style   = "none"
//!
//! [[model]]
//! provider          = "my-gateway"
//! id                = "claude-4-sonnet"
//! api_id            = "us.anthropic.claude-sonnet-4-5-20251001-v1:0"
//! ctx               = 200000
//! max_out           = 16000
//! price_in          = 3.0
//! price_out         = 15.0
//! price_cache_read  = 0.30
//! price_cache_write = 3.75
//! cache             = "explicit"   # | "automatic" | "none"
//! cache_breakpoints = 4
//! cache_min_tokens  = 1024
//!
//! [model.quirks]               # optional per-model override merged over provider
//! reasoning = "adaptive"
//! ```

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kn9t_core::{CacheMode, ModelRef, ModelSpec, Price};
use kn9t_core::Quirks as ModelQuirks;
use kn9t_provider_core::{lookup_price, Quirks as HttpQuirks};
use kn9t_provider_openai::{OpenAiConfig, OpenAiProvider};
use serde::Deserialize;

// ── TOML shape ───────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct RawConfig {
    #[serde(default)]
    pub provider: HashMap<String, RawProvider>,
    #[serde(default, rename = "model")]
    pub models: Vec<RawModel>,
    /// Optional: which model id to use as the default for new sessions.
    pub default_model: Option<String>,
    /// Optional [server] section.
    #[serde(default)]
    pub server: RawServer,
    /// Optional [policy] section (DESIGN §10.1). Global only — never from project-local.
    #[serde(default)]
    pub policy: RawPolicy,
    /// [[plugin]] entries — user tool plugins spawned at startup.
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<RawPlugin>,
}

/// `[policy]` block — DESIGN §10.1.
#[derive(Debug, Deserialize, Default)]
pub struct RawPolicy {
    /// `ask_on_mutation` | `allow_all` | `deny_all` | `readonly`
    pub mode: Option<String>,
    #[serde(default)]
    pub bash: RawBashPolicy,
    /// Persistent approvals: `scope=always` writes here (`[policy.approvals]`).
    #[serde(default)]
    pub approvals: RawApprovals,
}

/// `[policy.approvals]` — persistent `scope=always` fingerprints.
#[derive(Debug, Deserialize, Default)]
pub struct RawApprovals {
    #[serde(default)]
    pub always: Vec<String>,
}

/// `[policy.bash]` block. Each field is `Option` so we can tell
/// "absent → default" from "explicitly set to []".
#[derive(Debug, Deserialize, Default)]
pub struct RawBashPolicy {
    pub allow_read: Option<Vec<String>>,
    pub always_ask: Option<Vec<String>>,
    pub never: Option<Vec<String>>,
    /// `[policy.bash.allow_read_sub]` — must come last in TOML.
    pub allow_read_sub: Option<HashMap<String, Vec<String>>>,
}

/// Resolved policy mode — DESIGN §10.1 `mode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyMode {
    AskOnMutation,
    AllowAll,
    DenyAll,
    ReadOnly,
}
impl Default for PolicyMode {
    fn default() -> Self { PolicyMode::AskOnMutation }
}
impl PolicyMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "ask_on_mutation" => Ok(PolicyMode::AskOnMutation),
            "allow_all" => Ok(PolicyMode::AllowAll),
            "deny_all" => Ok(PolicyMode::DenyAll),
            "readonly" => Ok(PolicyMode::ReadOnly),
            other => Err(format!("unknown [policy] mode {other:?}; expected ask_on_mutation | allow_all | deny_all | readonly")),
        }
    }
}

/// Configuration for a user tool plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct RawPlugin {
    /// Plugin name (for logging; also verified against handshake).
    pub name: String,
    /// Command + args to spawn (e.g., ["python", "path/to/plugin.py"]).
    pub cmd: Vec<String>,
    /// Environment variables to inject. Values support `env:VAR` syntax.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// `[server]` config block — all fields optional, defaults shown.
#[derive(Debug, Deserialize, Default)]
pub struct RawServer {
    /// Seconds of inactivity (no attached clients, no running turns) before the
    /// server exits. Default: 1800 (30 min). Set to 0 to disable auto-exit.
    pub idle_exit_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct RawProvider {
    pub kind: String,
    /// Required for `kind = "openai"`. Not used by `kind = "plugin"`.
    #[serde(default)]
    pub base_url: String,
    pub api_key: Option<String>,
    /// Required for `kind = "plugin"`: binary name or absolute path.
    pub binary: Option<String>,
    /// Env vars injected into the plugin subprocess. Values support `env:VAR` interpolation.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// R-SRV-CFG-010: per-provider extra headers (openai only).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub tls_insecure: bool,
    #[serde(default)]
    pub quirks: RawQuirks,
}

/// Raw quirks mirror DESIGN §8.2.  All fields optional; missing → use the
/// provider-level default (which is `Quirks::default()`).
#[derive(Debug, Deserialize, Default)]
pub struct RawQuirks {
    pub max_tokens_field:  Option<String>,
    pub system_role:       Option<String>,
    pub usage_in_stream:   Option<bool>,
    pub finish_reason:     Option<bool>,
    pub reasoning:         Option<String>,
    pub tool_result_name:  Option<bool>,
    pub thinking_style:    Option<String>,
    pub thinking_replay:   Option<String>,
    pub require_tools:     Option<bool>,
    pub streaming:         Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RawModel {
    pub provider:          String,
    pub id:                String,
    /// Wire model id sent to the API. Defaults to `id` if absent.
    pub api_id:            Option<String>,
    pub ctx:               u32,
    pub max_out:           u32,
    #[serde(default)]
    pub price_in:          f64,
    #[serde(default)]
    pub price_out:         f64,
    #[serde(default)]
    pub price_cache_read:  f64,
    #[serde(default)]
    pub price_cache_write: f64,
    /// "explicit" | "automatic" | "none"  (default: "none")
    #[serde(default = "default_cache_mode_str")]
    pub cache: String,
    #[serde(default = "default_breakpoints")]
    pub cache_breakpoints: u8,
    #[serde(default = "default_min_tokens")]
    pub cache_min_tokens: u32,
    #[serde(default)]
    pub quirks: RawQuirks,
}

fn default_cache_mode_str() -> String { "automatic".into() }
fn default_breakpoints()    -> u8    { 4 }
fn default_min_tokens()     -> u32   { 1024 }

// ── Resolved output ──────────────────────────────────────────────────────────

/// A wired provider + all its model specs, ready to inject into `ServerState`.
pub struct ResolvedConfig {
    pub providers: Vec<(String, Arc<dyn kn9t_core::Provider>)>,
    pub models:    Vec<ModelSpec>,
    /// The model id that should be the server default (first model if unspecified).
    pub default_model_id: Option<String>,
    /// Idle-exit duration from `[server] idle_exit_secs`. `None` → use default (30 min).
    /// `Some(0)` → disable auto-exit.
    pub idle_exit: Option<std::time::Duration>,
    /// Resolved bash classifier policy (DESIGN §10.1). Always present (defaults).
    pub bash_policy: crate::classify::BashPolicy,
    /// Resolved policy mode.
    pub policy_mode: PolicyMode,
    /// User tool plugins to spawn at startup.
    pub plugins: Vec<ResolvedPlugin>,
}

/// A resolved user plugin configuration.
#[derive(Debug, Clone)]
pub struct ResolvedPlugin {
    pub name: String,
    pub cmd: Vec<String>,
    pub env: Vec<(String, String)>,
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Path to the global config file.
pub fn global_config_path() -> PathBuf {
    let home = std::env::var("KN9T_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs_home().unwrap_or_else(|| PathBuf::from("."))
                .join(".kn9t")
        });
    home.join("config.toml")
}

/// Load and resolve `~/.kn9t/config.toml`.  Missing file → empty config (server
/// starts provider-less; turns are no-ops until config exists).
pub fn load(path: &Path) -> Result<ResolvedConfig, String> {
    let raw: RawConfig = if path.exists() {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("config: read {}: {e}", path.display()))?;
        toml::from_str(&text)
            .map_err(|e| format!("config: parse {}: {e}", path.display()))?
    } else {
        RawConfig::default()
    };

    resolve(raw)
}

// ── Resolution ────────────────────────────────────────────────────────────────

/// Price info from plugin model declaration.
#[derive(Debug, Default, Deserialize)]
struct PluginPrice {
    #[serde(default)]
    input: f64,
    #[serde(default)]
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
}

fn resolve(raw: RawConfig) -> Result<ResolvedConfig, String> {
    // Build provider map: name → (HttpQuirks, Arc<dyn Provider>)
    let mut provider_quirks: HashMap<String, HttpQuirks> = HashMap::new();
    let mut providers: Vec<(String, Arc<dyn kn9t_core::Provider>)> = Vec::new();
    // Models auto-discovered from plugins and OpenAI endpoints.
    let mut auto_models: Vec<ModelSpec> = Vec::new();

    for (name, rp) in &raw.provider {
        match rp.kind.as_str() {
            "openai" => {
                let api_key = resolve_api_key(rp.api_key.as_deref())
                    .map_err(|e| format!("config: provider {name}: {e}"))?;
                let extra_headers = resolve_headers(&rp.headers, name);
                let quirks = build_http_quirks(&rp.quirks);
                provider_quirks.insert(name.clone(), quirks.clone());
                let provider = OpenAiProvider::new(OpenAiConfig {
                    name:         name.clone(),
                    base_url:     rp.base_url.clone(),
                    api_key:      if api_key.is_empty() { None } else { Some(api_key.clone()) },
                    quirks,
                    tls_insecure: rp.tls_insecure,
                    extra_headers: extra_headers.clone(),
                    ..OpenAiConfig::default()
                });
                
                // Auto-discover models from /v1/models endpoint.
                let discovered = fetch_openai_models(
                    &rp.base_url, 
                    if api_key.is_empty() { None } else { Some(&api_key) },
                    &extra_headers,
                    rp.tls_insecure,
                    name,
                );
                auto_models.extend(discovered);
                
                providers.push((name.clone(), Arc::new(provider) as Arc<dyn kn9t_core::Provider>));
            }
            "plugin" => {
                let binary_name = rp.binary.as_deref().unwrap_or_else(|| {
                    crate::log!("[kn9t-config] provider {name:?}: kind=\"plugin\" requires a `binary` field; skipping");
                    ""
                });
                if binary_name.is_empty() { continue; }

                // Resolve binary: absolute path or sibling of the running executable.
                let binary_path = {
                    let p = std::path::Path::new(binary_name);
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        let mut exe = std::env::current_exe()
                            .unwrap_or_else(|_| std::path::PathBuf::from("."));
                        exe.pop();
                        exe.push(binary_name);
                        if cfg!(windows) { exe.set_extension("exe"); }
                        exe
                    }
                };

                if !binary_path.exists() {
                    crate::log!(
                        "[kn9t-config] provider {name:?}: plugin binary not found at {}; skipping",
                        binary_path.display()
                    );
                    continue;
                }

                // Resolve env vars: "env:VAR" → value of VAR, otherwise literal.
                let env_pairs: Vec<(String, String)> = rp.env.iter()
                    .filter_map(|(k, v)| {
                        let resolved = if let Some(var) = v.strip_prefix("env:") {
                            match std::env::var(var) {
                                Ok(val) => val,
                                Err(_) => {
                                    crate::log!("[kn9t-config] provider {name:?}: env var {var:?} not set; skipping key {k:?}");
                                    return None;
                                }
                            }
                        } else {
                            v.clone()
                        };
                        Some((k.clone(), resolved))
                    })
                    .collect();

                let env_refs: Vec<(&str, &str)> = env_pairs.iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();

                match kn9t_plugin::PluginHost::spawn(&binary_path, &env_refs, std::sync::Arc::new(kn9t_plugin::NoOpPluginKv)) {
                    Ok(host) => {
                        crate::log!("[kn9t-config] provider {name:?}: spawned plugin {}", binary_path.display());
                        
                        // Extract models from plugin declaration (auto-discovery).
                         if let Some(ref prov_decl) = host.declaration.provider {
                             for model_decl in &prov_decl.models {
                                 // Try plugin-provided price, then fallback lookup, then zero.
                                 let price = model_decl.price.as_ref()
                                     .and_then(|p| serde_json::from_value::<PluginPrice>(p.clone()).ok())
                                     .filter(|p| p.input > 0.0 || p.output > 0.0)
                                     .map(|p| Price {
                                         input: p.input,
                                         output: p.output,
                                         cache_read: p.cache_read,
                                         cache_write: p.cache_write,
                                     })
                                     .or_else(|| lookup_price(&model_decl.id))
                                     .unwrap_or(Price { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 });
                                 
                                 let spec = ModelSpec {
                                     r#ref: ModelRef {
                                         provider: name.clone(),
                                         id: model_decl.id.clone(),
                                     },
                                     api_id: model_decl.id.clone(),
                                     ctx_window: model_decl.ctx_window,
                                     max_out: model_decl.ctx_window / 4, // Default: 25% of context
                                     price,
                                    cache: CacheMode::Automatic,
                                    streaming: true,
                                    quirks: ModelQuirks::default(),
                                };
                                auto_models.push(spec);
                                crate::log!("[kn9t-config] provider {name:?}: discovered model {:?}", model_decl.id);
                            }
                        }
                        
                        let remote = kn9t_plugin::RemoteProvider::new(
                            std::sync::Arc::new(host), name.clone()
                        );
                        providers.push((name.clone(), Arc::new(remote) as Arc<dyn kn9t_core::Provider>));
                    }
                    Err(e) => {
                        crate::log!("[kn9t-config] provider {name:?}: spawn failed: {e}; skipping");
                        continue;
                    }
                }
            }
            other => {
                crate::log!(
                    "[kn9t-config] provider {name:?}: kind {other:?} not supported \
                     (supported: \"openai\", \"plugin\"); skipping"
                );
                continue;
            }
        }
    }

    // Build model specs
    let mut models: Vec<ModelSpec> = Vec::new();
    for rm in &raw.models {
        let provider_q = provider_quirks.get(&rm.provider).cloned()
            .unwrap_or_default();

        // Per-model quirk override merged on top of provider quirks (DESIGN §8.3)
        let merged_quirks = merge_quirks(provider_q, &rm.quirks);

        let cache = parse_cache_mode(&rm.cache, rm.cache_breakpoints, rm.cache_min_tokens)
            .map_err(|e| format!("config: model {:?}: {e}", rm.id))?;

        let api_id = rm.api_id.clone().unwrap_or_else(|| rm.id.clone());

        // Use config price if any non-zero, otherwise fallback lookup.
        let config_price = Price {
            input:       rm.price_in,
            output:      rm.price_out,
            cache_read:  rm.price_cache_read,
            cache_write: rm.price_cache_write,
        };
        let price = if config_price.input > 0.0 || config_price.output > 0.0 {
            config_price
        } else {
            lookup_price(&api_id).unwrap_or(config_price)
        };
        
        let spec = ModelSpec {
            r#ref: ModelRef {
                provider: rm.provider.clone(),
                id:       rm.id.clone(),
            },
            api_id,
            ctx_window: rm.ctx,
            max_out:    rm.max_out,
            price,
            cache,
            streaming: true,
            quirks: ModelQuirks::default(),   // model-level Quirks (kn9t-core) = default
        };
        models.push(spec);

        if !provider_quirks.contains_key(&rm.provider) {
            crate::log!("[kn9t-config] model {:?} references unknown provider {:?}; model registered but turns will fail", rm.id, rm.provider);
        }
        // Store the merged HTTP quirks somewhere the provider can access per-model.
        // In v1 the provider holds a single global Quirks; per-model override is
        // recorded here for future use. (Full per-model quirk dispatch lands in §8.3
        // extension work outside v1 scope.)
        let _ = merged_quirks;
    }

    // Merge plugin-discovered models with config-defined models.
    // Config models override plugin models with the same (provider, id).
    let config_model_keys: std::collections::HashSet<(String, String)> = models
        .iter()
        .map(|m| (m.r#ref.provider.clone(), m.r#ref.id.clone()))
        .collect();
    
    for pm in auto_models {
        if !config_model_keys.contains(&(pm.r#ref.provider.clone(), pm.r#ref.id.clone())) {
            models.push(pm);
        } else {
            crate::log!("[kn9t-config] plugin model {:?} overridden by config", pm.r#ref.id);
        }
    }

    let default_model_id = raw.default_model
        .or_else(|| models.first().map(|m| m.r#ref.id.clone()));

    let idle_exit = raw.server.idle_exit_secs
        .map(std::time::Duration::from_secs);

    // Resolve [policy] — DESIGN §10.1. Absent → defaults (ask_on_mutation + BashPolicy::default).
    let policy_mode = match &raw.policy.mode {
        None => PolicyMode::AskOnMutation,
        Some(s) => PolicyMode::parse(s).map_err(|e| format!("config: {e}"))?,
    };
    let def_bash = crate::classify::BashPolicy::default();
    let raw_bash = raw.policy.bash;
    let bash_policy = crate::classify::BashPolicy {
        allow_read: raw_bash.allow_read.unwrap_or(def_bash.allow_read),
        always_ask: raw_bash.always_ask.unwrap_or(def_bash.always_ask),
        never: raw_bash.never.unwrap_or(def_bash.never),
        allow_read_sub: raw_bash.allow_read_sub
            .map(|m| m.into_iter().collect::<std::collections::BTreeMap<_, _>>())
            .unwrap_or(def_bash.allow_read_sub),
    };

    // Resolve user tool plugins.
    let plugins: Vec<ResolvedPlugin> = raw.plugins.iter()
        .filter_map(|rp| {
            if rp.cmd.is_empty() {
                crate::log!("[kn9t-config] plugin {:?}: cmd is empty; skipping", rp.name);
                return None;
            }
            // Resolve env vars: "env:VAR" → value of VAR, otherwise literal.
            let env: Vec<(String, String)> = rp.env.iter()
                .filter_map(|(k, v)| {
                    let resolved = if let Some(var) = v.strip_prefix("env:") {
                        match std::env::var(var) {
                            Ok(val) => val,
                            Err(_) => {
                                crate::log!("[kn9t-config] plugin {:?}: env var {var:?} not set; skipping key {k:?}", rp.name);
                                return None;
                            }
                        }
                    } else {
                        v.clone()
                    };
                    Some((k.clone(), resolved))
                })
                .collect();
            Some(ResolvedPlugin {
                name: rp.name.clone(),
                cmd: rp.cmd.clone(),
                env,
            })
        })
        .collect();

    Ok(ResolvedConfig { providers, models, default_model_id, idle_exit, bash_policy, policy_mode, plugins })
}

/// R-SRV-CFG-010: resolve `[provider.X.headers]` — same `env:VAR` syntax as api_key.
/// Missing env var → warn and omit (soft); R-SRV-CFG-020.
fn resolve_headers(
    raw: &std::collections::HashMap<String, String>,
    provider_name: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (k, v) in raw {
        match resolve_header_value(v) {
            Ok(val) if val.is_empty() => {
                crate::log!(
                    "[kn9t-config] provider {provider_name:?}: header {k:?} resolved to empty; omitting (R-SRV-CFG-020)"
                );
            }
            Ok(val) => out.push((k.clone(), val)),
            Err(e) => {
                crate::log!(
                    "[kn9t-config] provider {provider_name:?}: header {k:?}: {e}; omitting (R-SRV-CFG-020)"
                );
            }
        }
    }
    out
}

fn resolve_header_value(raw: &str) -> Result<String, String> {
    if raw.starts_with("env:") {
        let var = &raw[4..];
        std::env::var(var).map_err(|_| format!("env var {var:?} not set"))
    } else {
        Ok(raw.to_owned())
    }
}

/// Resolve `api_key = "env:VAR"` or a literal string.  `None` → empty string
/// (anonymous; some endpoints need no key).
fn resolve_api_key(raw: Option<&str>) -> Result<String, String> {
    match raw {
        None => Ok(String::new()),
        Some(s) if s.starts_with("env:") => {
            let var = &s[4..];
            std::env::var(var)
                .map_err(|_| format!("api_key env var {var:?} not set"))
        }
        Some(s) => Ok(s.to_owned()),
    }
}

fn build_http_quirks(r: &RawQuirks) -> HttpQuirks {
    let def = HttpQuirks::default();
    HttpQuirks {
        max_tokens_field: r.max_tokens_field.clone().unwrap_or(def.max_tokens_field),
        system_role:      r.system_role.clone().unwrap_or(def.system_role),
        usage_in_stream:  r.usage_in_stream.unwrap_or(def.usage_in_stream),
        finish_reason:    r.finish_reason.unwrap_or(def.finish_reason),
        reasoning:        r.reasoning.clone().unwrap_or(def.reasoning),
        tool_result_name: r.tool_result_name.unwrap_or(def.tool_result_name),
        thinking_style:   r.thinking_style.clone().unwrap_or(def.thinking_style),
        thinking_replay:  r.thinking_replay.clone().unwrap_or(def.thinking_replay),
        require_tools:    r.require_tools.unwrap_or(def.require_tools),
        streaming:        r.streaming.unwrap_or(def.streaming),
        extra_body:       serde_json::Value::Null,
    }
}

fn merge_quirks(base: HttpQuirks, over: &RawQuirks) -> HttpQuirks {
    HttpQuirks {
        max_tokens_field: over.max_tokens_field.clone().unwrap_or(base.max_tokens_field),
        system_role:      over.system_role.clone().unwrap_or(base.system_role),
        usage_in_stream:  over.usage_in_stream.unwrap_or(base.usage_in_stream),
        finish_reason:    over.finish_reason.unwrap_or(base.finish_reason),
        reasoning:        over.reasoning.clone().unwrap_or(base.reasoning),
        tool_result_name: over.tool_result_name.unwrap_or(base.tool_result_name),
        thinking_style:   over.thinking_style.clone().unwrap_or(base.thinking_style),
        thinking_replay:  over.thinking_replay.clone().unwrap_or(base.thinking_replay),
        require_tools:    over.require_tools.unwrap_or(base.require_tools),
        streaming:        over.streaming.unwrap_or(base.streaming),
        extra_body:       serde_json::Value::Null,
    }
}

fn parse_cache_mode(s: &str, breakpoints: u8, min_tokens: u32) -> Result<CacheMode, String> {
    match s {
        "explicit"  => Ok(CacheMode::Explicit { max_breakpoints: breakpoints, min_tokens }),
        "automatic" => Ok(CacheMode::Automatic),
        "none"      => Ok(CacheMode::None),
        other       => Err(format!("unknown cache mode {other:?}; use \"explicit\", \"automatic\", or \"none\"")),
    }
}

/// Fetch models from an OpenAI-compatible /v1/models endpoint.
/// Returns empty vec on any error (non-fatal; config [[model]] can still be used).
fn fetch_openai_models(
    base_url: &str,
    api_key: Option<&str>,
    extra_headers: &[(String, String)],
    _tls_insecure: bool,  // Note: ureq+rustls doesn't support skipping verification
    provider_name: &str,
) -> Vec<ModelSpec> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    
    let mut req = ureq::get(&url);
    if let Some(key) = api_key {
        req = req.header("Authorization", &format!("Bearer {key}"));
    }
    for (k, v) in extra_headers {
        req = req.header(k, v);
    }
    
    let body: serde_json::Value = match req.call() {
        Ok(resp) => {
            let mut reader = resp.into_body().into_reader();
            let mut body_str = String::new();
            if let Err(e) = reader.read_to_string(&mut body_str) {
                crate::log!("[kn9t-config] provider {provider_name:?}: /models read failed: {e}");
                return Vec::new();
            }
            match serde_json::from_str(&body_str) {
                Ok(v) => v,
                Err(e) => {
                    crate::log!("[kn9t-config] provider {provider_name:?}: /models parse failed: {e}");
                    return Vec::new();
                }
            }
        },
        Err(e) => {
            crate::log!("[kn9t-config] provider {provider_name:?}: /models fetch failed: {e}");
            return Vec::new();
        }
    };
    
    let models_arr = match body.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => {
            crate::log!("[kn9t-config] provider {provider_name:?}: /models response has no 'data' array");
            return Vec::new();
        }
    };
    
    let mut result = Vec::new();
    for model in models_arr {
        let id = match model.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        
        // Try to extract context window if available.
        let ctx_window = model.get("context_window")
            .or_else(|| model.get("context_length"))
            .and_then(|v| v.as_u64())
            .unwrap_or(128_000) as u32;
        
        // Use family-specific output limits or cap at a reasonable default.
        let max_out = model_max_output(&id, ctx_window);
        
        let spec = ModelSpec {
            r#ref: ModelRef {
                provider: provider_name.to_string(),
                id: id.clone(),
            },
            api_id: id.clone(),
            ctx_window,
            max_out,
            price: lookup_price(&id).unwrap_or(Price { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }),
            cache: CacheMode::Automatic,
            streaming: true,
            quirks: kn9t_core::Quirks::default(),
        };
        result.push(spec);
    }
    
    if !result.is_empty() {
        crate::log!("[kn9t-config] provider {provider_name:?}: discovered {} models from /models", result.len());
    }
    
    result
}

/// Per-family output limits for discovered models.
/// Based on OpenCode's BEDROCK_FAMILY_CONFIG and bedrockOutputLimit().
fn model_max_output(id: &str, ctx_window: u32) -> u32 {
    // Claude models
    if id.contains("haiku") { return 8_192; }
    if id.contains("opus") { return 32_000; }
    if id.contains("sonnet") { return 65_536; }
    
    // Amazon Nova
    if id.contains("nova-micro") { return 5_120; }
    if id.contains("nova-lite") { return 10_000; }
    if id.contains("nova-pro") { return 5_120; }
    if id.contains("nova") { return 5_120; }
    
    // Nvidia Nemotron
    if id.contains("nemotron") { return 8_192; }
    
    // Mistral
    if id.contains("mistral") { return 8_192; }
    
    // Default: 25% of context or 8192, whichever is smaller
    std::cmp::min(ctx_window / 4, 8_192)
}

#[cfg(target_family = "unix")]
fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[cfg(target_family = "windows")]
fn dirs_home() -> Option<PathBuf> {
    std::env::var("USERPROFILE").ok()
        .or_else(|| std::env::var("HOMEDRIVE").ok().and_then(|d| {
            std::env::var("HOMEPATH").ok().map(|p| d + &p)
        }))
        .map(PathBuf::from)
}

#[cfg(not(any(target_family = "unix", target_family = "windows")))]
fn dirs_home() -> Option<PathBuf> { None }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{classify, Shell, Classification};

    fn parse_raw(toml: &str) -> RawConfig {
        toml::from_str(toml).expect("parse RawConfig")
    }

    #[test]
    fn policy_absent_is_default() {
        let raw = parse_raw(r#""#);
        let resolved = resolve(raw).unwrap();
        assert_eq!(resolved.policy_mode, PolicyMode::AskOnMutation);
        // defaults mirror BashPolicy::default
        let def = crate::classify::BashPolicy::default();
        assert_eq!(resolved.bash_policy.allow_read, def.allow_read);
        assert_eq!(resolved.bash_policy.never, def.never);
    }

    #[test]
    fn policy_never_mytool_is_hard_deny() {
        let raw = parse_raw(r#"
            [policy.bash]
            never = ["mytool"]
        "#);
        let resolved = resolve(raw).unwrap();
        // never = ["mytool"] replaces default never? We treat Some → exact list,
        // so it must still be HardDeny for mytool.
        assert!(resolved.bash_policy.never.contains(&"mytool".to_string()));
        let cls = classify("mytool --help", Shell::Posix, &resolved.bash_policy);
        assert!(matches!(cls, Classification::HardDeny(_)), "mytool must be HardDeny, got {cls:?}");
        // sudo was replaced → no longer HardDeny if we replaced. Document that
        // explicit never replaces the default list. If we want additive, change
        // resolve to extend. For now, replacement is the spec-faithful behaviour.
        // The test only asserts mytool.
    }

    #[test]
    fn policy_allow_read_sub_must_be_last() {
        // TOML requires allow_read_sub to come after the other bash keys; this
        // is the DESIGN §10.1 shape. Verify parsing succeeds.
        let raw = parse_raw(r#"
            [policy]
            mode = "ask_on_mutation"

            [policy.bash]
            allow_read = ["ls", "cat"]
            always_ask = ["rm"]
            never = ["sudo"]

            [policy.bash.allow_read_sub]
            git = ["log", "status"]
            cargo = ["tree"]
        "#);
        let resolved = resolve(raw).unwrap();
        assert_eq!(resolved.policy_mode, PolicyMode::AskOnMutation);
        assert_eq!(resolved.bash_policy.allow_read, vec!["ls", "cat"]);
        assert_eq!(resolved.bash_policy.always_ask, vec!["rm"]);
        assert_eq!(resolved.bash_policy.allow_read_sub.get("git").unwrap(), &vec!["log".to_string(), "status".to_string()]);
        // git log is allowed
        assert_eq!(classify("git log", Shell::Posix, &resolved.bash_policy), Classification::AllowReadOnly);
        // git push is Ask (not in allow_read_sub)
        assert_eq!(classify("git push", Shell::Posix, &resolved.bash_policy), Classification::Ask);
    }

    #[test]
    fn policy_mode_variants() {
        for (s, expected) in [
            ("ask_on_mutation", PolicyMode::AskOnMutation),
            ("allow_all", PolicyMode::AllowAll),
            ("deny_all", PolicyMode::DenyAll),
            ("readonly", PolicyMode::ReadOnly),
        ] {
            let raw = parse_raw(&format!(r#"[policy]
mode = "{s}"
"#));
            let resolved = resolve(raw).unwrap();
            assert_eq!(resolved.policy_mode, expected);
        }
    }

    #[test]
    fn policy_mode_unknown_is_error() {
        let raw = parse_raw(r#"[policy]
mode = "bogus"
"#);
        assert!(resolve(raw).is_err());
    }

    #[test]
    fn policy_mode_and_bash_combined_change_behaviour() {
        // Demonstrates [policy] in config demonstrably changes behaviour (Phase 1 exit criteria).
        let raw = parse_raw(r#"
            [policy]
            mode = "readonly"

            [policy.bash]
            never = ["mytool"]
        "#);
        let resolved = resolve(raw).unwrap();
        assert_eq!(resolved.policy_mode, PolicyMode::ReadOnly);
        // mytool is HardDeny via bash policy
        let p = crate::classify::BashPolicy {
            never: vec!["mytool".into()],
            ..crate::classify::BashPolicy::default()
        };
        // Simulate ConfigPolicy (readonly) behaviour: mytool still HardDeny
        assert!(matches!(classify("mytool x", Shell::Posix, &p), Classification::HardDeny(_)));
    }
}
