/// kn9t-tools — built-in tools (bash, read, write, edit) as a subprocess plugin.
///
/// Auto-spawned by kn9t at startup. Speaks the kn9t plugin protocol v2.
fn main() {
    kn9t_plugin_sdk::Plugin::new("kn9t-tools")
        .tool(bash::Bash)
        .tool(read::Read)
        .tool(write::Write)
        .tool(edit::Edit)
        .run();
}

mod bash;
mod edit;
mod read;
mod write;
