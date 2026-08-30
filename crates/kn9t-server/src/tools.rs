//! R-PLUG2-110 — spawn tool plugins at server startup.
//!
//! All tools come from external plugins configured via [[plugin]] in config.toml.
//! No built-in tools — if no plugins are configured, the server runs with zero tools.

use crate::config::ResolvedPlugin;
use kn9t_core::{PluginKv, Tool, ToolRegistry};
use kn9t_plugin::{PluginHost, RemoteTool};
use std::process::Command;
use std::sync::Arc;

/// Spawn a user plugin from [[plugin]] config.
fn spawn_user_plugin(cfg: &ResolvedPlugin, kv: Arc<dyn PluginKv>) -> Result<(Arc<PluginHost>, Vec<Arc<dyn Tool>>), String> {
    crate::log!("spawning user plugin '{}': {:?}", cfg.name, cfg.cmd);

    // Build env as slice of refs
    let env_refs: Vec<(&str, &str)> = cfg.env.iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Spawn using command + args
    let host = spawn_with_cmd(&cfg.cmd, &env_refs, kv)
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
            };
            Arc::new(RemoteTool::new(tool_spec, host.clone())) as Arc<dyn Tool>
        })
        .collect()
}

/// Spawn all plugins (user plugins from [[plugin]] config) and return a merged ToolRegistry.
pub fn spawn_all_plugins(
    user_plugins: &[ResolvedPlugin],
    kv: Arc<dyn PluginKv>,
) -> Result<(Vec<Arc<PluginHost>>, ToolRegistry), String> {
    let mut all_hosts: Vec<Arc<PluginHost>> = Vec::new();
    let mut all_tools: Vec<Arc<dyn Tool>> = Vec::new();

    // User plugins (soft-fail per plugin)
    for cfg in user_plugins {
        match spawn_user_plugin(cfg, Arc::clone(&kv)) {
            Ok((host, tools)) => {
                all_hosts.push(host);
                all_tools.extend(tools);
            }
            Err(e) => {
                crate::log!("warning: user plugin '{}' failed to start: {e}", cfg.name);
                // Continue with other plugins
            }
        }
    }

    let registry = ToolRegistry::from_tools(all_tools);
    crate::log!("total tools registered: {}", registry.len());

    Ok((all_hosts, registry))
}
