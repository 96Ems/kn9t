// Stage 07 acceptance tests — kn9t-tui

// ── tui::no_kn9t_deps (R-TUI-010 / GI-6) ────────────────────────────────────
// CI: kn9t-tui/Cargo.toml must contain no `kn9t-*` dependency in [dependencies].
// [dev-dependencies] are exempt — test support crates are allowed.
// This test is the machine-checkable gate for R-TUI-010.

#[test]
fn tui_no_kn9t_deps() {
    let manifest = include_str!("../Cargo.toml");
    let mut in_dev_deps = false;

    for line in manifest.lines() {
        let trimmed = line.trim();

        // Track section headers
        if trimmed.starts_with('[') {
            in_dev_deps = trimmed.contains("dev-dependencies");
            continue;
        }

        // Skip comments
        if trimmed.starts_with('#') {
            continue;
        }

        // Skip dev-dependencies — they're exempt from GI-6
        if in_dev_deps {
            continue;
        }

        // A dependency on a kn9t-* crate would start with `kn9t-` as a key.
        assert!(
            !trimmed.starts_with("kn9t-"),
            "GI-6 violated: kn9t-tui/Cargo.toml contains a kn9t-* dependency in [dependencies]: {line:?}"
        );
    }
}
