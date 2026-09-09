use kn9t_core::Cancel;
use std::thread;
use std::time::Duration;

#[test]
fn test_cancel_new_is_not_cancelled() {
    let cancel = Cancel::new();
    assert!(!cancel.cancelled());
}

#[test]
fn test_cancel_after_cancel() {
    let cancel = Cancel::new();
    cancel.cancel();
    assert!(cancel.cancelled());
}

#[test]
fn test_cancel_is_idempotent() {
    let cancel = Cancel::new();
    cancel.cancel();
    cancel.cancel();
    cancel.cancel();
    assert!(cancel.cancelled());
}

#[test]
fn test_cancel_clones_share_state() {
    let cancel1 = Cancel::new();
    let cancel2 = cancel1.clone();

    assert!(!cancel1.cancelled());
    assert!(!cancel2.cancelled());

    cancel1.cancel();

    assert!(cancel1.cancelled());
    assert!(cancel2.cancelled());
}

#[test]
fn test_cancel_across_threads() {
    let cancel = Cancel::new();
    let cancel_clone = cancel.clone();

    let handle = thread::spawn(move || {
        // Wait a bit then cancel
        thread::sleep(Duration::from_millis(10));
        cancel_clone.cancel();
    });

    // Should eventually become cancelled
    while !cancel.cancelled() {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(cancel.cancelled());

    handle.join().unwrap();
}

#[test]
fn test_wait_timeout_returns_true_if_already_cancelled() {
    let cancel = Cancel::new();
    cancel.cancel();

    let result = cancel.wait_timeout(Duration::from_secs(10));
    assert!(result); // Should return immediately, not wait 10s
}

#[test]
fn test_wait_timeout_returns_false_on_timeout() {
    let cancel = Cancel::new();

    let start = std::time::Instant::now();
    let result = cancel.wait_timeout(Duration::from_millis(10));
    let elapsed = start.elapsed();

    assert!(!result); // Not cancelled
                      // wait_timeout loops until the deadline, so it must honour the full 10ms
                      // even if the condvar wakes spuriously. A small tolerance absorbs
                      // Instant sampling jitter only.
    assert!(
        elapsed >= Duration::from_millis(9),
        "returned too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(100),
        "waited too long: {elapsed:?}"
    );
}

#[test]
fn test_wait_timeout_wakes_on_cancel() {
    let cancel = Cancel::new();
    let cancel_clone = cancel.clone();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        cancel_clone.cancel();
    });

    let start = std::time::Instant::now();
    let result = cancel.wait_timeout(Duration::from_secs(10));
    let elapsed = start.elapsed();

    assert!(result); // Was cancelled
    assert!(
        elapsed < Duration::from_secs(1),
        "did not wake on cancel: {elapsed:?}"
    );

    handle.join().unwrap();
}

#[test]
fn test_cancel_default() {
    let cancel = Cancel::default();
    assert!(!cancel.cancelled());
}
