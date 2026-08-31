//! Spawn tool plugins at server startup — discovery + config overrides.
//!
//! All tools come from external plugins, merged into one `ToolRegistry` from two sources:
//!
//! 1. **Discovered plugins** scanned from `<KN9T_HOME|~/.kn9t>/plugins/` (ADR-0004).
//! 2. **User plugins** configured via `[[plugin]]` in the global config.toml
//!    (R-PLUG-100: never from a project-local file).
//!
//! Discovery scans **only** the user plugin dir and **never** a project-relative `plugins/`
//! directory (ADR-0004): a repo-committed file must not run arbitrary binaries — `git clone`
//! then `kn9t` must not be code execution. The repo's `plugins/` directory is *build source*;
//! `~/.kn9t/plugins/` is the *install target*.
//!
//! Step 3.3 — config overrides discovery (job/phase3.md 3.3):
//! - `enabled = false` / `disabled = true` → discovered plugin with same `name`
//!   (and file-stem fallback) is suppressed; the entry itself is not spawned.
//! - `cmd = [...]` → pinned plugin: spawned as a user plugin; discovered plugin
//!   with same declared name (or same binary path) is suppressed (config wins).
//! - `cmd` omitted + `env` set → env injection: when the discovered plugin with
//!   matching `name` (file-stem heuristic pre-handshake) is spawned, those env vars
//!   are injected.
//!
//! Each discovered executable is treated as a plugin binary and handshaked (single-element
//! command array). A plugin that fails to spawn or handshake is **soft-failed**: a warning is
//! logged and startup continues with the remaining plugins. An empty or missing plugin dir is
//! also a warning, not a crash (the loud-fail decision is deferred to Phase 3.4).

use crate::config::ResolvedPlugin;
use kn9t_core::{PluginKv, Tool, ToolRegistry};
use kn9t_plugin::{PluginHost, RemoteTool};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// The user plugin directory: `<KN9T_HOME|~/.kn9t>/plugins` (ADR-0004).
///
/// This is the ONLY directory scanned for plugin binaries at startup. It is derived
/// from `auth::kn9t_home()` (the same canonical home used for `token`/`port`/`spawn.lock`),
/// never from the current working directory or the executable's location.
pub fn plugin_dir() -> PathBuf {
    crate::auth::kn9t_home().join("plugins")
}

/// True if `path` is a plugin-binary candidate for discovery.
///
/// - Unix: a regular file with at least one execute bit set.
/// - Windows: a regular file with extension `.exe`.
///
/// The filter is deliberately minimal — the handshake is the real gate. A candidate that
/// fails to spawn or handshake is soft-failed with a warning.
fn is_plugin_candidate(path: &Path) -> bool {
    if !path.is_file() {
        return false; // skip directories and non-files
    }
    #[cfg(target_family = "unix")]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = path.metadata().map(|m| m.permissions().mode()).unwrap_or(0);
        return mode & 0o111 != 0;
    }
    #[cfg(target_family = "windows")]
    {
        return path.extension().map_or(false, |e| e == "exe");
    }
    #[cfg(not(any(target_family = "unix", target_family = "windows")))]
    {
        true
    }
}

/// List plugin-binary candidates in the user plugins dir `dir`.
///
/// Never called with a project-relative path — [`plugin_dir`] (the KN9T_HOME dir) is the
/// only source. Reading is best-effort: an unreadable or missing directory yields an empty
/// list and the caller logs the warning. Sorted for deterministic spawn order.
pub fn discover_plugin_binaries(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if is_plugin_candidate(&p) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// Spawn a pinned user plugin from [[plugin]] config (cmd is Some).
fn spawn_user_plugin(cfg: &ResolvedPlugin, kv: Arc<dyn PluginKv>) -> Result<(Arc<PluginHost>, Vec<Arc<dyn Tool>>), String> {
    let cmd = cfg.cmd.as_ref().expect("spawn_user_plugin called with no cmd");
    crate::log!("spawning user plugin '{}': {:?}", cfg.name, cmd);

    // Build env as slice of refs
    let env_refs: Vec<(&str, &str)> = cfg.env.iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Spawn using command + args
    let host = spawn_with_cmd(cmd, &env_refs, kv)
        .map_err(|e| format!("failed to spawn plugin '{}': {e}", cfg.name))?;

    // Verify plugin name matches config (warning only)
    if host.declaration.name != cfg.name {
        crate::log!("warning: plugin declared name '{}' differs from config name '{}'",
            host.declaration.name, cfg.name);
    }

    crate::log!("user plugin '{}' handshake complete: {} tools, {} hooks declared",
        cfg.name, host.declaration.tools.len(), host.declaration.hooks.len());

    let host = Arc::new(host);
    let tools = extract_tools(&host);

    for t in &tools {
        crate::log!("  registered user tool: {}", t.spec().name);
    }

    Ok((host, tools))
}

/// Handshake one auto-discovered plugin binary (single-element command array), with optional env.
fn spawn_discovered_plugin(bin: &Path, env_vars: &[(&str, &str)], kv: Arc<dyn PluginKv>) -> Result<(Arc<PluginHost>, Vec<Arc<dyn Tool>>), String> {
    if env_vars.is_empty() {
        crate::log!("spawning discovered plugin: {}", bin.display());
    } else {
        crate::log!("spawning discovered plugin: {} (with {} env vars)", bin.display(), env_vars.len());
    }

    // Single-element cmd: the binary itself, no args. spawn_with_cmd performs the
    // hello/hello handshake exactly as for configured user plugins.
    let cmd = vec![bin.to_string_lossy().into_owned()];
    let host = spawn_with_cmd(&cmd, env_vars, kv)
        .map_err(|e| format!("failed to spawn plugin '{}': {e}", bin.display()))?;

    crate::log!("discovered plugin '{}' handshake complete: {} tools, {} hooks declared",
        host.declaration.name, host.declaration.tools.len(), host.declaration.hooks.len());

    let host = Arc::new(host);
    let tools = extract_tools(&host);
    Ok((host, tools))
}

/// Spawn a plugin subprocess from a command + args array.
fn spawn_with_cmd(cmd: &[String], env_vars: &[(&str, &str)], kv: Arc<dyn PluginKv>) -> Result<PluginHost, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use kn9t_plugin::codec::{write_host_msg, HostMsg, PluginMsg};

    if cmd.is_empty() {
        return Err("empty command".to_string());
    }

    let program = &cmd[0];
    let args = &cmd[1..];

    let mut command = Command::new(program);
    command.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    for (k, v) in env_vars {
        command.env(k, v);
    }

    let mut child = command.spawn()
        .map_err(|e| format!("spawn '{program}': {e}"))?;

    let stdin = child.stdin.take().ok_or("stdin not captured")?;
    let stdout = child.stdout.take().ok_or("stdout not captured")?;

    let mut reader = BufReader::new(stdout);
    let mut writer: Box<dyn Write + Send> = Box::new(stdin);

    // Send host hello
    write_host_msg(&mut *writer, &HostMsg::Hello {
        proto: 1,
        kn9t: env!("CARGO_PKG_VERSION").to_string(),
    }).map_err(|e| format!("hello write: {e}"))?;

    // Read plugin hello
    let mut line = String::new();
    reader.read_line(&mut line)
        .map_err(|e| format!("hello read: {e}"))?;
    let plugin_hello: PluginMsg = serde_json::from_str(line.trim_end())
        .map_err(|e| format!("hello parse: {e}"))?;

    let declaration = match plugin_hello {
        PluginMsg::Hello { name, capabilities, hooks, tools, provider, events } => {
            use kn9t_plugin::codec::parse_hook_name;
            kn9t_plugin::PluginDeclaration {
                name,
                capabilities,
                hooks: hooks.iter().filter_map(|h| parse_hook_name(h)).collect(),
                tools,
                subscribed_events: events,
                provider,
            }
        }
        _ => return Err("expected hello from plugin".to_string()),
    };

    // Reap child in background
    std::thread::spawn(move || { let _ = child.wait(); });

    // Build PluginHost from handshaked I/O
    use std::io::Read;
    let read: Box<dyn Read + Send> = Box::new(reader.into_inner());
    Ok(PluginHost::from_io(read, writer, declaration, kv))
}

/// Extract RemoteTool wrappers from a PluginHost.
fn extract_tools(host: &Arc<PluginHost>) -> Vec<Arc<dyn Tool>> {
    host.declaration.tools.iter()
        .map(|spec| {
            let tool_spec = kn9t_core::ToolSpec {
                name: spec.name.clone(),
                description: spec.description.clone(),
                schema: spec.schema.clone(),
                hidden: spec.hidden,
                effects: spec.effects.clone(),
            };
            Arc::new(RemoteTool::new(tool_spec, host.clone())) as Arc<dyn Tool>
        })
        .collect()
}

/// Spawn all plugins and return a merged ToolRegistry.
///
/// Two sources, merged into one registry:
/// - pinned user plugins from `[[plugin]]` config with `cmd` (config wins over discovery)
/// - auto-discovered plugins from [`plugin_dir`] (`~/.kn9t/plugins/`, ADR-0004)
///
/// Config overrides (Phase 3.3):
/// - `disabled = true` / `enabled = false` → discovered plugin with matching `name`
///   (or file-stem) is suppressed; the entry itself is not spawned.
/// - `cmd` present → pinned: discovered with same declared name or same binary path is suppressed.
/// - `cmd` absent + `env` present → env injected into discovered spawn for matching file-stem.
///
/// Soft-fails per plugin: a plugin that fails to spawn or handshake is logged as a
/// warning and startup continues with the rest. An empty/missing discovery dir is
/// warned but does not fail startup (Phase 3.4 decides the loud-fail policy).
pub fn spawn_all_plugins(
    user_plugins: &[ResolvedPlugin],
    kv: Arc<dyn PluginKv>,
) -> Result<(Vec<Arc<PluginHost>>, ToolRegistry), String> {
    spawn_all_plugins_in_dir(user_plugins, &plugin_dir(), kv)
}

/// Like [`spawn_all_plugins`] but with an explicit discovery directory.
///
/// Production passes [`plugin_dir`]; tests inject a temp directory so discovery is
/// hermetic and never touches the developer's real `~/.kn9t/plugins/`.
fn spawn_all_plugins_in_dir(
    user_plugins: &[ResolvedPlugin],
    discovery_dir: &Path,
    kv: Arc<dyn PluginKv>,
) -> Result<(Vec<Arc<PluginHost>>, ToolRegistry), String> {
    let mut all_hosts: Vec<Arc<PluginHost>> = Vec::new();
    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();
    // For dedup: track tool names already registered (first wins, duplicate warns).
    let mut seen_tools: HashSet<String> = HashSet::new();

    // Partition config entries.
    let mut disabled_names: HashSet<String> = HashSet::new();
    let mut env_overrides: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut pinned_plugins: Vec<&ResolvedPlugin> = Vec::new();

    for cfg in user_plugins {
        if cfg.disabled {
            disabled_names.insert(cfg.name.clone());
            // Also record env if any? Disabled wins over env, so ignore.
            continue;
        }
        if let Some(cmd) = &cfg.cmd {
            if !cmd.is_empty() {
                pinned_plugins.push(cfg);
            } else if !cfg.env.is_empty() {
                // Empty cmd treated as env-only override.
                env_overrides.insert(cfg.name.clone(), cfg.env.clone());
            }
        } else if !cfg.env.is_empty() {
            env_overrides.insert(cfg.name.clone(), cfg.env.clone());
        }
    }

    // Track successful pinned plugin declared names and binary paths for dedup.
    let mut pinned_declared_names: HashSet<String> = HashSet::new();
    let mut pinned_paths: HashSet<PathBuf> = HashSet::new();

    // Pinned user plugins (soft-fail per plugin)
    for cfg in pinned_plugins {
        match spawn_user_plugin(cfg, Arc::clone(&kv)) {
            Ok((host, tools)) => {
                pinned_declared_names.insert(host.declaration.name.clone());
                if let Some(cmd) = &cfg.cmd {
                    if let Some(first) = cmd.first() {
                        pinned_paths.insert(PathBuf::from(first));
                    }
                }
                // Dedup tools by name (config wins, but pinned are first so they win naturally).
                let mut filtered = Vec::new();
                for t in tools {
                    let name = t.spec().name.clone();
                    if seen_tools.contains(&name) {
                        crate::log!("warning: duplicate tool '{}' from pinned plugin '{}' — keeping first, discarding duplicate", name, cfg.name);
                        continue;
                    }
                    seen_tools.insert(name);
                    filtered.push(t);
                }
                all_hosts.push(host);
                all_tools.extend(filtered);
            }
            Err(e) => {
                crate::log!("warning: user plugin '{}' failed to start: {e}", cfg.name);
                // Don't add to pinned_declared_names/paths on failure — allow discovered fallback.
            }
        }
    }

    // Auto-discovered plugins from the user plugin dir (ADR-0004). Never a
    // project-relative path. Soft-fail per plugin, same as user plugins.
    if !discovery_dir.is_dir() {
        crate::log!(
            "warning: plugin directory {} does not exist — server starts with zero \
             auto-discovered tools (bootstrap installs them; see `kn9t` first run)",
            discovery_dir.display()
        );
    } else {
        let discovered = discover_plugin_binaries(discovery_dir);
        if discovered.is_empty() {
            crate::log!(
                "warning: no plugins discovered in {} — server starts with zero auto-discovered tools",
                discovery_dir.display()
            );
        } else {
            crate::log!("plugin discovery: {} candidate(s) in {}", discovered.len(), discovery_dir.display());
        }
        for bin in &discovered {
            // Pre-handshake path dedup: if this exact path was pinned successfully, skip.
            if pinned_paths.contains(bin) {
                crate::log!("discovered plugin {} superseded by pinned config (same path) — skipping", bin.display());
                continue;
            }
            // Pre-handshake file-stem disabled check (cheap, covers typical case).
            let file_stem = bin.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if disabled_names.contains(&file_stem) {
                crate::log!("discovered plugin {} disabled via config (name '{}') — skipping", bin.display(), file_stem);
                continue;
            }
            // Determine env to inject for this discovered binary (file-stem heuristic).
            let env_for_bin: Vec<(&str, &str)> = env_overrides.get(&file_stem)
                .map(|v| v.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect())
                .unwrap_or_default();
            if !env_for_bin.is_empty() {
                crate::log!("discovered plugin {} will be spawned with {} env vars from config override for '{}'", bin.display(), env_for_bin.len(), file_stem);
            }
            match spawn_discovered_plugin(bin, &env_for_bin, Arc::clone(&kv)) {
                Ok((host, tools)) => {
                    let declared = host.declaration.name.clone();
                    // Post-handshake disabled check (declared name may differ from file stem).
                    if disabled_names.contains(&declared) {
                        crate::log!("discovered plugin '{}' ({}) disabled via config — discarding", declared, bin.display());
                        // Host will be dropped; child exits on pipe close. Best-effort shutdown.
                        host.shutdown();
                        continue;
                    }
                    // Pinned name supersedes discovered with same declared name.
                    if pinned_declared_names.contains(&declared) {
                        crate::log!("discovered plugin '{}' ({}) superseded by pinned config plugin '{}' — discarding", declared, bin.display(), declared);
                        host.shutdown();
                        continue;
                    }
                    // If declared name has an env override but file stem didn't match, warn (heuristic miss).
                    if env_overrides.contains_key(&declared) && !env_overrides.contains_key(&file_stem) {
                        crate::log!("warning: discovered plugin '{}' ({}) matches env override for '{}' but file name '{}' did not — env not injected (rename binary to match config name or use pinned cmd)", declared, bin.display(), declared, file_stem);
                    }
                    // Dedup tools by name.
                    let mut filtered = Vec::new();
                    for t in tools {
                        let name = t.spec().name.clone();
                        if seen_tools.contains(&name) {
                            crate::log!("warning: duplicate tool '{}' from discovered plugin '{}' ({}) — keeping first, discarding duplicate", name, declared, bin.display());
                            continue;
                        }
                        seen_tools.insert(name);
                        filtered.push(t);
                    }
                    let n = filtered.len();
                    all_hosts.push(host);
                    all_tools.extend(filtered);
                    crate::log!("  registered {} tool(s) from {}", n, bin.display());
                }
                Err(e) => {
                    crate::log!("warning: discovered plugin {} failed to start: {e}", bin.display());
                    // Continue with other discovered plugins
                }
            }
        }
    }

    let registry = ToolRegistry::from_tools(all_tools);
    crate::log!("total tools registered: {}", registry.len());

    Ok((all_hosts, registry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Unique temp dir under the OS temp dir (no tempfile dep in src tests; GI-1).
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir().join(format!(
                "kn9t-discovery-{tag}-{}-{n}", std::process::id()
            ));
            std::fs::create_dir_all(&p).expect("create temp dir");
            TempDir(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Write a minimal protocol-conformant plugin as a `/bin/sh` script. It consumes
    /// the host hello on stdin, replies with its own hello declaring one tool, then
    /// idles until its stdin pipe closes (host exit).
    #[cfg(unix)]
    fn write_dummy_plugin(path: &Path, name: &str, tool: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\n\
             # kn9t plugin fixture for discovery tests\n\
             IFS= read -r _host_hello\n\
             printf '%s\\n' '{{\"t\":\"hello\",\"name\":\"{name}\",\"capabilities\":[\"streaming\"],\"tools\":[{{\"name\":\"{tool}\",\"description\":\"dummy\",\"schema\":{{\"type\":\"object\"}},\"parallel_safe\":false}}]}}'\n\
             while IFS= read -r _line; do :; done\n"
        );
        std::fs::write(path, script).expect("write dummy plugin");
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o111)).unwrap();
    }

    /// Write a dummy plugin that declares its tool name based on an env var.
    /// If `env_key` is set to `env_val`, tool name is `tool_when_set`, else `tool_when_unset`.
    #[cfg(unix)]
    fn write_env_conditional_plugin(path: &Path, name: &str, env_key: &str, env_val: &str, tool_when_set: &str, tool_when_unset: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!(
            "#!/bin/sh\n\
             IFS= read -r _host_hello\n\
             if [ \"${env_key}\" = \"{env_val}\" ]; then TOOL=\"{tool_when_set}\"; else TOOL=\"{tool_when_unset}\"; fi\n\
             printf '%s\\n' \"{{\\\"t\\\":\\\"hello\\\",\\\"name\\\":\\\"{name}\\\",\\\"capabilities\\\":[\\\"streaming\\\"],\\\"tools\\\":[{{\\\"name\\\":\\\"$TOOL\\\",\\\"description\\\":\\\"dummy\\\",\\\"schema\\\":{{\\\"type\\\":\\\"object\\\"}},\\\"parallel_safe\\\":false}}]}}\"\n\
             while IFS= read -r _line; do :; done\n"
        );
        std::fs::write(path, script).expect("write env conditional plugin");
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o111)).unwrap();
    }

    fn noop_kv() -> Arc<dyn PluginKv> {
        Arc::new(kn9t_plugin::NoOpPluginKv)
    }

    /// Unix: a candidate must be a regular executable file.
    #[test]
    #[cfg(unix)]
    fn candidate_filter_unix() {
        let dir = TempDir::new("cand");
        let exe = dir.path().join("tool.sh");
        write_dummy_plugin(&exe, "x", "t");
        assert!(is_plugin_candidate(&exe));

        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "not a plugin").unwrap();
        assert!(!is_plugin_candidate(&plain));

        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        assert!(!is_plugin_candidate(&dir.path().join("subdir")));

        // Non-executable script is not a candidate (exec bit required on Unix).
        let noexec = dir.path().join("noexec.sh");
        std::fs::write(&noexec, "#!/bin/sh\n").unwrap();
        assert!(!is_plugin_candidate(&noexec));
    }

    /// ADR-0004 positive: an executable in the user plugin dir is discovered and
    /// handshaked; its tools land in the merged registry.
    #[test]
    #[cfg(unix)]
    fn discovery_spawns_executable_in_user_plugin_dir() {
        let dir = TempDir::new("pos");
        let bin = dir.path().join("dummy.sh");
        write_dummy_plugin(&bin, "dummy-tools", "dummy_tool");

        let (hosts, tools) = spawn_all_plugins_in_dir(&[], dir.path(), noop_kv()).unwrap();

        assert_eq!(hosts.len(), 1, "one discovered plugin host");
        assert_eq!(hosts[0].declaration.name, "dummy-tools");
        assert!(hosts[0].declaration.tools.iter().any(|t| t.name == "dummy_tool"));
        assert!(tools.iter().any(|t| t.spec().name == "dummy_tool"),
            "discovered tool must be registered in the merged registry");
    }

    /// ADR-0004 negative: a project-relative `plugins/` directory is NEVER scanned.
    /// Discovery reads only the user plugin dir; a valid handshake binary sitting in
    /// `./plugins/` under a project root must not be picked up.
    #[test]
    #[cfg(unix)]
    fn discovery_ignores_project_relative_plugins() {
        // Fake "project root" with a plugins/ subdir holding a perfectly valid plugin.
        // A `git clone && kn9t` scenario: scanning this dir would be code execution.
        let proj = TempDir::new("proj");
        let proj_plugins = proj.path().join("plugins");
        std::fs::create_dir_all(&proj_plugins).unwrap();
        let evil = proj_plugins.join("evil.sh");
        write_dummy_plugin(&evil, "evil-tools", "evil_tool");

        // Project-local config trying to honor the project plugin (R-PLUG-100 forbids
        // this on the config side; discovery must equally never read ./plugins/).
        std::fs::write(
            proj.path().join(".kn9t.toml"),
            "[[plugin]]\nname = 'evil'\ncmd = ['./plugins/evil.sh']\n",
        )
        .unwrap();

        // The user plugin dir — the ONLY directory discovery reads (ADR-0004).
        let user_dir = TempDir::new("user");

        let (hosts, tools) = spawn_all_plugins_in_dir(&[], user_dir.path(), noop_kv()).unwrap();

        assert!(hosts.is_empty(), "no plugin may be spawned from a project-relative dir");
        assert_eq!(tools.len(), 0, "project-relative plugins/ must never be discovered");
        assert!(discover_plugin_binaries(&proj_plugins).contains(&evil),
            "sanity: evil.sh IS a valid candidate — the point is the user dir scan never looks there");
    }

    /// A missing discovery dir warns but does not fail startup (server with zero
    /// tools runs; the loud-fail decision is deferred to Phase 3.4).
    #[test]
    fn discovery_missing_dir_is_not_fatal() {
        let ghost = std::env::temp_dir().join(format!("kn9t-no-such-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ghost); // ensure absent
        let (hosts, tools) = spawn_all_plugins_in_dir(&[], &ghost, noop_kv()).unwrap();
        assert!(hosts.is_empty());
        assert!(tools.is_empty());
    }

    /// Discovered binaries are spawned in deterministic (sorted) order.
    #[test]
    #[cfg(unix)]
    fn discovery_order_is_sorted() {
        let dir = TempDir::new("order");
        write_dummy_plugin(&dir.path().join("b.sh"), "b-tools", "b_tool");
        write_dummy_plugin(&dir.path().join("a.sh"), "a-tools", "a_tool");

        let discovered = discover_plugin_binaries(dir.path());
        let names: Vec<String> = discovered.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.sh".to_string(), "b.sh".to_string()]);

        let (hosts, _tools) = spawn_all_plugins_in_dir(&[], dir.path(), noop_kv()).unwrap();
        let host_names: Vec<&str> = hosts.iter().map(|h| h.declaration.name.as_str()).collect();
        assert_eq!(host_names, vec!["a-tools", "b-tools"]);
    }

    /// Phase 3.3: disabled config suppresses discovered plugin with same name (file-stem heuristic).
    #[test]
    #[cfg(unix)]
    fn discovery_disabled_via_config_suppresses() {
        let dir = TempDir::new("dis-disabled");
        // Binary file name "my-tools" declares name "my-tools".
        let bin = dir.path().join("my-tools");
        write_dummy_plugin(&bin, "my-tools", "my_tool");

        let disabled_cfg = ResolvedPlugin {
            name: "my-tools".to_string(),
            cmd: None,
            env: vec![],
            disabled: true,
        };
        let (hosts, tools) = spawn_all_plugins_in_dir(&[disabled_cfg], dir.path(), noop_kv()).unwrap();
        assert!(hosts.is_empty(), "disabled plugin must not be spawned");
        assert!(tools.is_empty(), "disabled plugin tools must not be registered");
    }

    /// Phase 3.3: pinned config (cmd) supersedes discovered plugin with same declared name.
    #[test]
    #[cfg(unix)]
    fn discovery_pinned_supersedes_discovered() {
        let dir = TempDir::new("dis-pinned");
        // Discovered binary declares "dup-tools" with tool "discovered_tool".
        let discovered_bin = dir.path().join("dup-tools");
        write_dummy_plugin(&discovered_bin, "dup-tools", "discovered_tool");

        // Pinned binary elsewhere declares same name "dup-tools" but tool "pinned_tool".
        let pinned_dir = TempDir::new("pinned-src");
        let pinned_bin = pinned_dir.path().join("pinned-dup");
        write_dummy_plugin(&pinned_bin, "dup-tools", "pinned_tool");

        let pinned_cfg = ResolvedPlugin {
            name: "dup-tools".to_string(),
            cmd: Some(vec![pinned_bin.to_string_lossy().into_owned()]),
            env: vec![],
            disabled: false,
        };
        let (hosts, tools) = spawn_all_plugins_in_dir(&[pinned_cfg], dir.path(), noop_kv()).unwrap();
        // Only pinned host should survive; discovered suppressed.
        assert_eq!(hosts.len(), 1, "only pinned host should be registered");
        assert_eq!(hosts[0].declaration.name, "dup-tools");
        assert!(tools.iter().any(|t| t.spec().name == "pinned_tool"), "pinned tool must be present");
        assert!(!tools.iter().any(|t| t.spec().name == "discovered_tool"), "discovered tool must be suppressed");
        assert_eq!(tools.len(), 1);
    }

    /// Phase 3.3: pinned config with same path as discovered dedups by path (no duplicate even if names differ).
    #[test]
    #[cfg(unix)]
    fn discovery_pinned_same_path_dedups() {
        let dir = TempDir::new("dis-pinned-path");
        let bin = dir.path().join("same-bin");
        write_dummy_plugin(&bin, "same-plugin", "tool_a");

        // Pinned config points to the exact same path.
        let pinned_cfg = ResolvedPlugin {
            name: "same-plugin".to_string(),
            cmd: Some(vec![bin.to_string_lossy().into_owned()]),
            env: vec![],
            disabled: false,
        };
        let (hosts, tools) = spawn_all_plugins_in_dir(&[pinned_cfg], dir.path(), noop_kv()).unwrap();
        assert_eq!(hosts.len(), 1, "same path should not duplicate");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools.iter().next().unwrap().spec().name, "tool_a");
    }

    /// Phase 3.3: env injection — config provides env for discovered plugin matched by file-stem.
    #[test]
    #[cfg(unix)]
    fn discovery_env_injection() {
        let dir = TempDir::new("dis-env");
        let bin = dir.path().join("env-tools");
        // Plugin checks INJECTED env var.
        write_env_conditional_plugin(&bin, "env-tools", "INJECTED", "yes", "when_set", "when_unset");

        // Without override, tool should be when_unset.
        let (hosts, tools) = spawn_all_plugins_in_dir(&[], dir.path(), noop_kv()).unwrap();
        assert_eq!(tools.iter().next().unwrap().spec().name, "when_unset");
        drop(hosts);

        // With env override matching file-stem "env-tools".
        let env_cfg = ResolvedPlugin {
            name: "env-tools".to_string(),
            cmd: None,
            env: vec![("INJECTED".to_string(), "yes".to_string())],
            disabled: false,
        };
        let (hosts, tools) = spawn_all_plugins_in_dir(&[env_cfg], dir.path(), noop_kv()).unwrap();
        assert!(tools.iter().any(|t| t.spec().name == "when_set"), "env injection should affect discovered plugin");
        assert!(!tools.iter().any(|t| t.spec().name == "when_unset"));
        // Cleanup: hosts dropped, children exit.
        drop(hosts);
    }

    /// Phase 3.3: duplicate tool names across different plugins are deduped (first wins).
    #[test]
    #[cfg(unix)]
    fn discovery_duplicate_tool_names_deduped() {
        let dir = TempDir::new("dis-dup-tool");
        write_dummy_plugin(&dir.path().join("a.sh"), "a-tools", "shared_tool");
        write_dummy_plugin(&dir.path().join("b.sh"), "b-tools", "shared_tool");

        let (hosts, tools) = spawn_all_plugins_in_dir(&[], dir.path(), noop_kv()).unwrap();
        assert_eq!(hosts.len(), 2, "both hosts spawned");
        // But tool registry dedupes by tool name: first wins.
        assert_eq!(tools.len(), 1, "duplicate tool name must be deduped");
        assert_eq!(tools.iter().next().unwrap().spec().name, "shared_tool");
    }

    /// Phase 3.3: regression — existing duplicate kn9t-tools config + discovered does NOT double-register.
    /// This is the bug that caused 8 tools (bash/read/write/edit x2) and 400 from strict duplicate check.
    #[test]
    #[cfg(unix)]
    fn discovery_kn9t_tools_config_does_not_duplicate() {
        let dir = TempDir::new("dis-kn9t-tools");
        let bin = dir.path().join("kn9t-tools");
        write_dummy_plugin(&bin, "kn9t-tools", "bash");

        // Simulate user's ~/.kn9t/config.toml entry that points to same binary.
        let cfg = ResolvedPlugin {
            name: "kn9t-tools".to_string(),
            cmd: Some(vec![bin.to_string_lossy().into_owned()]),
            env: vec![],
            disabled: false,
        };
        let (hosts, tools) = spawn_all_plugins_in_dir(&[cfg], dir.path(), noop_kv()).unwrap();
        assert_eq!(tools.len(), 1, "kn9t-tools must not duplicate: pinned supersedes discovered");
        assert_eq!(hosts.len(), 1);
    }
}
