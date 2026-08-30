//! R-TOOL-080 / R-TOOL-090 acceptance tests — the `bash` command classifier.
//!
//! Restored verbatim from `crates/kn9t-tools/tests/acceptance.rs` as it stood at commit
//! 5b65819^, before the classifier was deleted during the tools-to-plugin migration.
//! The classifier now lives in `kn9t-server` (ADR-0001), so these run as
//! `cargo test -p kn9t-server --test classify` rather than the spec's original
//! `tool::classify_*` names.
//!
//! Every assertion is preserved, including the `sh -c` / `iex` interpreter-bypass cases
//! (R-TOOL-090 rule 5) and the `HardDeny` cases (rule 6) — these are the reason the
//! pipeline evaluation order is a MUST.

use kn9t_server::classify::{classify, BashPolicy, Classification, Shell};

// ------------------------- R-TOOL-080 -------------------------

#[test]
fn classify_posix() {
    let p = BashPolicy::default();
    // read-only
    assert_eq!(classify("cat foo.txt", Shell::Posix, &p), Classification::AllowReadOnly);
    assert_eq!(classify("rg needle src", Shell::Posix, &p), Classification::AllowReadOnly);
    // mutating form of an otherwise-read command via redirection -> Ask
    assert_eq!(classify("cat x > y", Shell::Posix, &p), Classification::Ask);
}

#[test]
fn classify_pwsh() {
    let p = BashPolicy::default();
    assert_eq!(
        classify("Get-Content foo.txt", Shell::PowerShell, &p),
        Classification::AllowReadOnly
    );
    // mutating form via redirection -> Ask
    assert_eq!(
        classify("Get-Content x > y", Shell::PowerShell, &p),
        Classification::Ask
    );
}

// ------------------------- R-TOOL-090 -------------------------

#[test]
fn classify_pipeline() {
    let p = BashPolicy::default();

    // Rule 1: unknown argv0 -> Ask (both grammars).
    assert_eq!(classify("frobnicate x", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("Frobnicate x", Shell::PowerShell, &p), Classification::Ask);

    // Rule 2: redirection / substitution / subshell -> Ask.
    assert_eq!(classify("cat x >> y", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("echo $(rm -rf /)", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("(cat x)", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("cat `whoami`", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("Get-Content x 2> y", Shell::PowerShell, &p), Classification::Ask);

    // Rule 3: in-place flag -> Ask.
    assert_eq!(classify("sed -i s/a/b/ f", Shell::Posix, &p), Classification::Ask);

    // Rule 4: git subcommand outside allow_read_sub -> Ask; allowed sub -> Allow.
    assert_eq!(classify("git push", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("git status", Shell::Posix, &p), Classification::AllowReadOnly);

    // Rule 5: interpreter in always_ask -> Ask (the sh -c and iex bypass cases).
    assert_eq!(classify("sh -c 'rm -rf /'", Shell::Posix, &p), Classification::Ask);
    assert_eq!(classify("bash -c 'echo hi'", Shell::Posix, &p), Classification::Ask);
    assert_eq!(
        classify("iex 'Remove-Item x'", Shell::PowerShell, &p),
        Classification::Ask
    );
    assert_eq!(
        classify("Invoke-Expression 'Get-Content x'", Shell::PowerShell, &p),
        Classification::Ask
    );

    // Rule 6: never -> HardDeny (both grammars).
    match classify("sudo rm -rf /", Shell::Posix, &p) {
        Classification::HardDeny(_) => {}
        other => panic!("expected HardDeny, got {other:?}"),
    }
    match classify("shutdown -h now", Shell::PowerShell, &p) {
        Classification::HardDeny(_) => {}
        other => panic!("expected HardDeny, got {other:?}"),
    }

    // Rule 7: plain read-only -> Allow.
    assert_eq!(classify("ls", Shell::Posix, &p), Classification::AllowReadOnly);
}
