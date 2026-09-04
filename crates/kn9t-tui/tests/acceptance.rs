// Stage 07 acceptance tests — kn9t-tui

// ── tui::no_kn9t_deps (R-TUI-010 / GI-6) ────────────────────────────────────
// CI: kn9t-tui/Cargo.toml must contain no `kn9t-*` dependency.
// This test is the machine-checkable gate for R-TUI-010.

#[test]
fn tui_no_kn9t_deps() {
    let manifest = include_str!("../Cargo.toml");
    // Every `kn9t-*` dependency line would look like:
    //   kn9t-core = ...   or   kn9t-... = { path = ... }
    for line in manifest.lines() {
        let trimmed = line.trim();
        // Skip comments.
        if trimmed.starts_with('#') {
            continue;
        }
        // A dependency on a kn9t-* crate would start with `kn9t-` as a key.
        assert!(
            !trimmed.starts_with("kn9t-"),
            "GI-6 violated: kn9t-tui/Cargo.toml contains a kn9t-* dependency: {line:?}"
        );
    }
}
