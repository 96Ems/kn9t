//! `kn9t install-plugins` — install project plugins into ~/.kn9t/plugins/.
//!
//! Scans `<project>/plugins/` for plugin crates, auto-builds if needed, and
//! copies executables to `~/.kn9t/plugins/`. Python plugins get a `[[plugin]]`
//! entry in `~/.kn9t/config.toml` instead.
//!
//! This is an **explicit user action** (not auto-discovery), so it respects
//! ADR-0004: clone-and-run is safe, but `kn9t install-plugins` trusts the project.
//!
//! GI-5: no async.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::bootstrap::kn9t_home_path;

// ── Plugin types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum PluginKind {
    Rust,   // Cargo.toml
    Go,     // go.mod
    Node,   // package.json
    Python, // pyproject.toml
}

impl PluginKind {
    fn detect(dir: &Path) -> Option<Self> {
        if dir.join("Cargo.toml").is_file() {
            Some(Self::Rust)
        } else if dir.join("go.mod").is_file() {
            Some(Self::Go)
        } else if dir.join("package.json").is_file() {
            Some(Self::Node)
        } else if dir.join("pyproject.toml").is_file() {
            Some(Self::Python)
        } else {
            None
        }
    }
}

// ── Plugin discovery ──────────────────────────────────────────────────────────

struct DiscoveredPlugin {
    name: String,
    dir: PathBuf,
    kind: PluginKind,
}

fn discover_plugins(project_root: &Path) -> Vec<DiscoveredPlugin> {
    let plugins_dir = project_root.join("plugins");
    if !plugins_dir.is_dir() {
        return Vec::new();
    }

    let mut plugins = Vec::new();
    if let Ok(entries) = fs::read_dir(&plugins_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
                if let (Some(name), Some(kind)) = (name, PluginKind::detect(&path)) {
                    plugins.push(DiscoveredPlugin { name, dir: path, kind });
                }
            }
        }
    }
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

// ── Build logic ───────────────────────────────────────────────────────────────

fn exe_suffix() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

/// Find existing executable for a plugin (pre-built).
fn find_existing_exe(plugin: &DiscoveredPlugin) -> Option<PathBuf> {
    let exe_name = format!("{}{}", plugin.name, exe_suffix());
    
    match plugin.kind {
        PluginKind::Rust => {
            // Check target/release first, then target/debug
            for profile in ["release", "debug"] {
                let path = plugin.dir.join("target").join(profile).join(&exe_name);
                if path.is_file() {
                    return Some(path);
                }
            }
            None
        }
        PluginKind::Go => {
            // Go builds in current dir by default
            let path = plugin.dir.join(&exe_name);
            if path.is_file() {
                return Some(path);
            }
            None
        }
        PluginKind::Node => {
            // Check for compiled output (varies by project)
            // Common patterns: dist/<name>.js, bin/<name>
            let dist_js = plugin.dir.join("dist").join(format!("{}.js", plugin.name));
            if dist_js.is_file() {
                return Some(dist_js);
            }
            None
        }
        PluginKind::Python => {
            // Python plugins don't have executables
            None
        }
    }
}

/// Build a plugin if no executable exists.
fn build_plugin(plugin: &DiscoveredPlugin) -> Result<Option<PathBuf>, String> {
    eprintln!("[install-plugins] building {} ({:?})...", plugin.name, plugin.kind);
    
    match plugin.kind {
        PluginKind::Rust => build_rust(plugin),
        PluginKind::Go => build_go(plugin),
        PluginKind::Node => build_node(plugin),
        PluginKind::Python => Ok(None), // Python doesn't need building
    }
}

fn build_rust(plugin: &DiscoveredPlugin) -> Result<Option<PathBuf>, String> {
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&plugin.dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    
    if !status.success() {
        return Err(format!("cargo build failed with exit code {:?}", status.code()));
    }
    
    let exe_name = format!("{}{}", plugin.name, exe_suffix());
    let exe_path = plugin.dir.join("target").join("release").join(&exe_name);
    if exe_path.is_file() {
        Ok(Some(exe_path))
    } else {
        Err(format!("build succeeded but executable not found at {}", exe_path.display()))
    }
}

fn build_go(plugin: &DiscoveredPlugin) -> Result<Option<PathBuf>, String> {
    let exe_name = format!("{}{}", plugin.name, exe_suffix());
    let output_path = plugin.dir.join(&exe_name);
    
    let status = Command::new("go")
        .args(["build", "-o", &exe_name, "."])
        .current_dir(&plugin.dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("failed to run go: {e}"))?;
    
    if !status.success() {
        return Err(format!("go build failed with exit code {:?}", status.code()));
    }
    
    if output_path.is_file() {
        Ok(Some(output_path))
    } else {
        Err(format!("build succeeded but executable not found at {}", output_path.display()))
    }
}

/// Run npm command, using cmd /c on Windows (npm.cmd needs shell).
fn run_npm(args: &[&str], dir: &Path) -> Result<(), String> {
    let status = if cfg!(windows) {
        let npm_args = std::iter::once("npm")
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        Command::new("cmd")
            .args(["/c", &npm_args])
            .current_dir(dir)
            .stdin(Stdio::null())
            .status()
    } else {
        Command::new("npm")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .status()
    };
    
    let status = status.map_err(|e| format!("failed to run npm {}: {e}", args.join(" ")))?;
    if !status.success() {
        return Err(format!("npm {} failed with exit code {:?}", args.join(" "), status.code()));
    }
    Ok(())
}

fn build_node(plugin: &DiscoveredPlugin) -> Result<Option<PathBuf>, String> {
    // npm install
    run_npm(&["install"], &plugin.dir)?;
    
    // npm run build (if build script exists)
    let pkg_json = plugin.dir.join("package.json");
    if let Ok(content) = fs::read_to_string(&pkg_json) {
        if content.contains("\"build\"") {
            run_npm(&["run", "build"], &plugin.dir)?;
        }
    }
    
    // For Node plugins, we return None and handle them via config
    // (they're typically run as `node dist/index.js`)
    Ok(None)
}

// ── Installation logic ────────────────────────────────────────────────────────

/// Detect Node.js entry point from package.json (main, scripts.start, or default dist/main.js).
fn detect_node_entry(dir: &Path) -> Option<PathBuf> {
    let pkg_json = dir.join("package.json");
    let content = fs::read_to_string(&pkg_json).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    
    // 1. Check "main" field
    if let Some(main) = json.get("main").and_then(|v| v.as_str()) {
        return Some(dir.join(main));
    }
    
    // 2. Check "scripts.start" for "node <path>"
    if let Some(start) = json.get("scripts")
        .and_then(|s| s.get("start"))
        .and_then(|v| v.as_str())
    {
        // Parse "node dist/main.js" or similar
        let parts: Vec<&str> = start.split_whitespace().collect();
        if parts.len() >= 2 && parts[0] == "node" {
            return Some(dir.join(parts[1]));
        }
    }
    
    // 3. Fallback: check dist/main.js, then dist/index.js
    let main_js = dir.join("dist").join("main.js");
    if main_js.is_file() {
        return Some(main_js);
    }
    let index_js = dir.join("dist").join("index.js");
    if index_js.is_file() {
        return Some(index_js);
    }
    
    None
}

fn install_executable(exe_path: &Path, dest_dir: &Path, name: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(dest_dir)
        .map_err(|e| format!("cannot create {}: {e}", dest_dir.display()))?;
    
    let dest_name = format!("{}{}", name, exe_suffix());
    let dest_path = dest_dir.join(&dest_name);
    
    fs::copy(exe_path, &dest_path)
        .map_err(|e| format!("cannot copy {} to {}: {e}", exe_path.display(), dest_path.display()))?;
    
    Ok(dest_path)
}

/// Add or update a [[plugin]] entry in config.toml for Python/Node plugins.
fn add_plugin_config(plugin: &DiscoveredPlugin) -> Result<(), String> {
    let config_path = kn9t_home_path().join("config.toml");
    
    // Read existing config
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    
    // Check if plugin already configured
    let marker = format!("name = \"{}\"", plugin.name);
    if existing.contains(&marker) {
        eprintln!("[install-plugins] {} already in config.toml, skipping", plugin.name);
        return Ok(());
    }
    
    // Generate the [[plugin]] entry
    let entry = match plugin.kind {
        PluginKind::Python => {
            // Find the Python module name (usually same as plugin name with _ instead of -)
            let module_name = plugin.name.replace('-', "_");
            let pythonpath = plugin.dir.to_string_lossy();
            format!(
                r#"
# ── {} (installed by kn9t install-plugins) ──
[[plugin]]
name = "{}"
cmd  = ["python", "-m", "{}"]

[plugin.env]
PYTHONPATH = "{}"
"#,
                plugin.name, plugin.name, module_name, pythonpath
            )
        }
        PluginKind::Node => {
            // Node plugins: detect entry point from package.json
            let entry_point = detect_node_entry(&plugin.dir)
                .unwrap_or_else(|| plugin.dir.join("dist").join("index.js"));
            let entry_str = entry_point.to_string_lossy();
            format!(
                r#"
# ── {} (installed by kn9t install-plugins) ──
[[plugin]]
name = "{}"
cmd  = ["node", "{}"]
"#,
                plugin.name, plugin.name, entry_str
            )
        }
        _ => return Ok(()), // Rust/Go don't need config entries
    };
    
    // Append to config
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_path)
        .map_err(|e| format!("cannot open {}: {e}", config_path.display()))?;
    
    file.write_all(entry.as_bytes())
        .map_err(|e| format!("cannot write to {}: {e}", config_path.display()))?;
    
    eprintln!("[install-plugins] added {} to config.toml", plugin.name);
    Ok(())
}

// ── Main entry point ──────────────────────────────────────────────────────────

pub fn run(args: &[String]) {
    let mut project_path: Option<PathBuf> = None;
    let mut no_build = false;
    let mut force = false;
    let mut rebuild = false;
    
    // Parse args
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" => {
                i += 1;
                if i < args.len() {
                    project_path = Some(PathBuf::from(&args[i]));
                } else {
                    eprintln!("error: --from requires a path argument");
                    std::process::exit(1);
                }
            }
            "--no-build" => no_build = true,
            "--force" => force = true,
            "--rebuild" => {
                rebuild = true;
                force = true; // rebuild implies force
            }
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                print_help();
                std::process::exit(1);
            }
        }
        i += 1;
    }
    
    // Determine project root
    let project_root = project_path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    
    eprintln!("[install-plugins] scanning {}/plugins/", project_root.display());
    
    // Discover plugins
    let plugins = discover_plugins(&project_root);
    if plugins.is_empty() {
        eprintln!("[install-plugins] no plugins found in {}/plugins/", project_root.display());
        return;
    }
    
    eprintln!("[install-plugins] found {} plugin(s):", plugins.len());
    for p in &plugins {
        eprintln!("  - {} ({:?})", p.name, p.kind);
    }
    eprintln!();
    
    let dest_dir = kn9t_home_path().join("plugins");
    let mut installed = 0;
    let mut configured = 0;
    let mut failed = 0;
    
    for plugin in &plugins {
        eprintln!("[install-plugins] processing {}...", plugin.name);
        
        // Check for existing executable (skip if --rebuild)
        let mut exe_path = if rebuild { None } else { find_existing_exe(plugin) };
        
        // Build if needed (or forced with --rebuild)
        if exe_path.is_none() && !no_build && plugin.kind != PluginKind::Python {
            match build_plugin(plugin) {
                Ok(path) => exe_path = path,
                Err(e) => {
                    eprintln!("[install-plugins] ERROR building {}: {}", plugin.name, e);
                    failed += 1;
                    continue;
                }
            }
        }
        
        // Install executable or add config
        match plugin.kind {
            PluginKind::Rust | PluginKind::Go => {
                if let Some(exe) = exe_path {
                    // Check if already installed
                    let dest_path = dest_dir.join(format!("{}{}", plugin.name, exe_suffix()));
                    if dest_path.exists() && !force {
                        eprintln!("[install-plugins] {} already installed (use --force to overwrite)", plugin.name);
                        continue;
                    }
                    
                    match install_executable(&exe, &dest_dir, &plugin.name) {
                        Ok(dest) => {
                            eprintln!("[install-plugins] installed {} → {}", plugin.name, dest.display());
                            installed += 1;
                        }
                        Err(e) => {
                            eprintln!("[install-plugins] ERROR installing {}: {}", plugin.name, e);
                            failed += 1;
                        }
                    }
                } else {
                    eprintln!("[install-plugins] no executable found for {} (use cargo/go build or remove --no-build)", plugin.name);
                    failed += 1;
                }
            }
            PluginKind::Python | PluginKind::Node => {
                match add_plugin_config(plugin) {
                    Ok(()) => configured += 1,
                    Err(e) => {
                        eprintln!("[install-plugins] ERROR configuring {}: {}", plugin.name, e);
                        failed += 1;
                    }
                }
            }
        }
    }
    
    // Summary
    eprintln!();
    eprintln!("[install-plugins] done: {} installed, {} configured, {} failed",
        installed, configured, failed);
    
    if installed > 0 || configured > 0 {
        eprintln!("[install-plugins] restart kn9t server to load new plugins (kn9t stop && kn9t)");
    }
    
    if failed > 0 {
        std::process::exit(1);
    }
}

fn print_help() {
    println!("kn9t install-plugins — install project plugins into ~/.kn9t/");
    println!();
    println!("Usage: kn9t install-plugins [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --from <path>   Project root (default: current directory)");
    println!("  --no-build      Don't auto-build; only copy existing executables");
    println!("  --force         Overwrite existing plugins in ~/.kn9t/plugins/");
    println!("  --rebuild       Force rebuild even if executable exists (implies --force)");
    println!("  -h, --help      Show this help");
    println!();
    println!("Plugin types supported:");
    println!("  Rust (Cargo.toml)    -> cargo build --release -> copy exe");
    println!("  Go (go.mod)          -> go build -> copy exe");
    println!("  Node (package.json)  -> npm install && npm run build -> add to config.toml");
    println!("  Python (pyproject)   -> add [[plugin]] cmd to config.toml");
    println!();
    println!("Examples:");
    println!("  kn9t install-plugins              # build & install from cwd");
    println!("  kn9t install-plugins --rebuild   # force rebuild all plugins");
    println!("  kn9t install-plugins --no-build  # only copy pre-built executables");
    println!();
    println!("Security: This is an explicit user action (ADR-0004 compliant).");
    println!("          You are trusting the project's plugins directory.");
}
