//! 96E-15 regression: double-UTF8 mojibake must not exist in crate sources.
//! Run: cargo test -p kn9t-core --test mojibake -- --nocapture

use std::path::Path;

#[test]
fn p1_96e15_no_mojibake() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = manifest.join("..");
    // actual workspace crates dir is at repo root /crates
    let repo_crates = manifest
        .ancestors()
        .find(|p| p.join("crates").exists())
        .map(|p| p.join("crates"))
        .unwrap_or_else(|| crates);

    // Mojibake byte patterns to forbid (described without raw mojibake chars to avoid self-trigger):
    // - C3 82 C2 A7 -> should be C2 A7 (section sign)
    // - Windows-1252 em dash C3 A2 E2 82 AC E2 80 9D -> E2 80 94 (em dash)
    // - Latin1 em dash C3 A2 C2 80 C2 94 -> E2 80 94
    let patterns: Vec<(&[u8], &str)> = vec![
        (b"\xc3\x82\xc2\xa7", "section-sign double-encode"),
        (b"\xc3\xa2\xe2\x82\xac\xe2\x80\x9d", "em-dash win double-encode"),
        (b"\xc3\xa2\xc2\x80\xc2\x94", "em-dash latin double-encode"),
        (b"\xc3\xa2\xe2\x82\xac\xe2\x80\x98", "left-single-quote double-encode"),
        (b"\xc3\xa2\xe2\x82\xac\xe2\x80\x99", "right-single-quote double-encode"),
        (b"\xc3\xa2\xe2\x82\xac\xc5\x93", "left-double-quote double-encode"),
        (b"\xc3\xa2\xe2\x82\xac\xc2\x9d", "right-double-quote latin variant"),
    ];

    let mut offenders = Vec::new();
    for entry in walkdir(&repo_crates) {
        if entry.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let bytes = std::fs::read(&entry).unwrap();
        for (pat, name) in &patterns {
            if bytes.windows(pat.len()).any(|w| w == *pat) {
                offenders.push(format!("{}: contains {} ({:?})", entry.display(), name, pat));
                break;
            }
        }
        // Also flag any generic double-encode prefix C3 A2 / C3 82 that indicates mojibake,
        // but only if it decodes to known mojibake chars to avoid false positives on correct UTF-8.
        // The above patterns cover the reported cases; generic check is supplementary.
    }

    if !offenders.is_empty() {
        for o in &offenders {
            eprintln!("mojibake: {}", o);
        }
        panic!(
            "96E-15: found {} file(s) with double-UTF8 mojibake (section/em-dash). Fix: replace with correct UTF-8 and check toolchain. Offenders:\n{}",
            offenders.len(),
            offenders.join("\n")
        );
    }
}

fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}
