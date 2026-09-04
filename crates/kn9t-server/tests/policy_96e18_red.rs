//! 96E-18 TDD red: bare unwrap / malformed config panic must be fixed.
//! These tests are expected to FAIL until policy.rs is hardened.

use kn9t_server::policy::ApprovalCache;

#[test]
fn policy_rs_has_no_bare_unwrap_in_non_test() {
    let text = std::fs::read_to_string("crates/kn9t-server/src/policy.rs")
        .or_else(|_| std::fs::read_to_string("src/policy.rs"))
        .expect("policy.rs readable");
    let src = text.split("#[cfg(test)]").next().unwrap();
    let bare: Vec<String> = src
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            if l.contains(".unwrap()") {
                Some(format!("{}: {}", i + 1, l.trim()))
            } else {
                None
            }
        })
        .collect();
    assert!(
        bare.is_empty(),
        "policy.rs still has {} bare .unwrap() in non-test code (must be .expect(\"reason\") or handled):\n{}",
        bare.len(),
        bare.join("\n")
    );
}

#[test]
fn approve_persistent_malformed_policy_shape_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // Malformed shape: policy is a string, not a table — current code panics at as_table_mut().unwrap()
    std::fs::write(&path, "policy = \"not a table\"\n").unwrap();
    let cache = ApprovalCache::new(path.clone());
    // Must not panic; should return Err or Ok, but not unwind.
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cache.approve_persistent("bash:rm -rf /tmp/x".into())
    }));
    assert!(
        res.is_ok(),
        "approve_persistent panicked on malformed policy shape — must return Result"
    );
    // If it returned Ok/Err without panicking, the shape is handled. We accept either,
    // but it must not have left a poisoned file.
    let _ = res.unwrap();
}

#[test]
fn approve_persistent_malformed_approvals_shape_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[policy]\napprovals = 123\n").unwrap();
    let cache = ApprovalCache::new(path);
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cache.approve_persistent("bash:echo hi".into())
    }));
    assert!(res.is_ok(), "panicked on approvals=123 shape");
    let _ = res.unwrap();
}

#[test]
fn approval_cache_property_random_fingerprints_do_not_panic() {
    // Property-like: many random fingerprints/config contents must not panic.
    for i in 0..50 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Random-ish content: sometimes empty, sometimes garbage
        let content = match i % 4 {
            0 => String::new(),
            1 => "[[broken".to_string(),
            2 => format!("policy.approvals.always = [\"fp{i}\"]"),
            _ => format!("random{i} = \"val\""),
        };
        let _ = std::fs::write(&path, content);
        let cache = ApprovalCache::new(path);
        let fp = format!("bash:cmd-{}-{}", i, "x".repeat(i % 20));
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = cache.is_approved(Some("sess"), &fp);
            let _ = cache.has_persistent(&fp);
            let _ = cache.approve_persistent(fp.clone());
        }));
        assert!(res.is_ok(), "panic on iteration {i}");
    }
}
