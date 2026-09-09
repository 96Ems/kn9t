//! Tools plugin helper for integration tests (R-PLUG2-110).

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use kn9t_core::{Tool, ToolRegistry, ToolSpec};
use kn9t_plugin::{PluginHost, RemoteTool, NoOpPluginKv};

/// Locate the built `kn9t-tools` plugin binary.
///
/// NOTE — this is **build-time artifact location, NOT runtime plugin discovery**.
/// The server discovers plugins only in `~/.kn9t/plugins/` (ADR-0004); it NEVER
/// scans the repo's `plugins/` directory. These tests handshake the plugin
/// *directly* (no server) to validate the ReAct loop, so they must locate the
/// cargo build artifact. Searching `plugins/kn9t-tools/target/` here is fine —
/// that is a build artifact path, not a runtime scan.
///
/// Search order:
///   1. `plugins/kn9t-tools/target/{debug,release}/kn9t-tools[.exe]` — the
///      standalone crate build. `kn9t-tools` is no longer a workspace member;
///      build it with `cd plugins/kn9t-tools && cargo build`.
///   2. `target/{debug,release}/kn9t-tools[.exe]` — legacy: in case the binary
///      was built into the workspace target dir instead (pre-move layout).
///   3. `<KN9T_HOME|~/.kn9t>/plugins/kn9t-tools[.exe]` — installed location
///      where bootstrap copies it on first run.
fn locate_tools_binary() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .unwrap() // crates/
        .parent()
        .unwrap() // <repo root>
        .to_path_buf();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let ext = if cfg!(windows) { ".exe" } else { "" };
    let name = format!("kn9t-tools{ext}");

    // (a) the external standalone build — the plugin now lives at
    //     plugins/kn9t-tools and is built on its own.
    let external = workspace_root
        .join("plugins")
        .join("kn9t-tools")
        .join("target")
        .join(profile)
        .join(&name);
    // (b) legacy workspace-target build.
    let legacy = workspace_root.join("target").join(profile).join(&name);

    // (c) installed location — where bootstrap puts it.
    let installed = {
        let home = std::env::var("KN9T_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .ok()
                    .map(|h| PathBuf::from(h).join(".kn9t"))
            });
        match home {
            Some(h) => h.join("plugins").join(&name),
            None => external.clone(), // placeholder; no install location known
        }
    };

    [&external, &legacy, &installed]
        .iter()
        .find(|p| p.is_file())
        .map(|p| (*p).clone())
        .unwrap_or(external)
}

/// Spawn the `kn9t-tools` plugin and return a `ToolRegistry` with all declared tools.
/// Panics if the binary is not found. Build it first:
/// `cd plugins/kn9t-tools && cargo build` (it is a standalone crate, not a workspace
/// member — `cargo build -p kn9t-tools` from the root does not work).
pub fn spawn_tools_registry() -> (Arc<PluginHost>, ToolRegistry) {
    let binary = locate_tools_binary();
    if !binary.exists() {
        panic!(
            "kn9t-tools binary not found at {}. Build it with \
             `cd plugins/kn9t-tools && cargo build`, or install it into \
             ~/.kn9t/plugins/ (first `kn9t` run does this automatically).",
            binary.display()
        );
    }

    let host = PluginHost::spawn(&binary, &[], Arc::new(NoOpPluginKv))
        .expect("failed to spawn kn9t-tools plugin");

    let host = Arc::new(host);
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let decl = host.declaration();

    for spec in &decl.tools {
        let tool_spec = ToolSpec {
            name: spec.name.clone(),
            description: spec.description.clone(),
            schema: spec.schema.clone(),
            hidden: false,
            effects: spec.effects.clone(),
            policy: Default::default(),
        };
        let remote = RemoteTool::new(tool_spec, host.clone());
        tools.push(Arc::new(remote));
    }

    (host, ToolRegistry::from_tools(tools))
}

/// Get a specific tool from the registry by name.
pub fn get_tool(registry: &ToolRegistry, name: &str) -> Arc<dyn Tool> {
    registry
        .get(name)
        .cloned()
        .unwrap_or_else(|| panic!("tool '{}' not found in registry", name))
}
