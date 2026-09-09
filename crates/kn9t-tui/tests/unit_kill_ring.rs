use kn9t_tui::kill_ring::KillRing;

// MAX_RING_SIZE is private; mirror the value here so the max-size test can assert it.
const MAX_RING_SIZE: usize = 10;

#[test]
fn test_kill_and_yank() {
    let mut ring = KillRing::new();

    ring.kill("hello".into(), false);
    ring.kill("world".into(), false);

    // Most recent should be "world"
    assert_eq!(ring.yank(0), Some("world"));
}

#[test]
fn test_yank_pop() {
    let mut ring = KillRing::new();

    ring.kill("first".into(), false);
    ring.kill("second".into(), false);
    ring.kill("third".into(), false);

    // Yank most recent
    assert_eq!(ring.yank(0), Some("third"));

    // Pop to previous
    let (len, text) = ring.yank_pop().unwrap();
    assert_eq!(len, 5); // "third".len()
    assert_eq!(text, "second");

    // Pop again
    let (len, text) = ring.yank_pop().unwrap();
    assert_eq!(len, 6); // "second".len()
    assert_eq!(text, "first");

    // Pop wraps around
    let (len, text) = ring.yank_pop().unwrap();
    assert_eq!(len, 5); // "first".len()
    assert_eq!(text, "third");
}

#[test]
fn test_append_kill() {
    let mut ring = KillRing::new();

    ring.kill("hello".into(), false);
    ring.kill(" world".into(), true); // Append

    assert_eq!(ring.len(), 1);
    assert_eq!(ring.yank(0), Some("hello world"));
}

#[test]
fn test_max_size() {
    let mut ring = KillRing::new();

    for i in 0..15 {
        ring.kill(format!("entry{}", i), false);
    }

    assert_eq!(ring.len(), MAX_RING_SIZE);
    // Most recent should be entry14
    assert_eq!(ring.yank(0), Some("entry14"));
}

#[test]
fn test_yank_pop_without_yank() {
    let mut ring = KillRing::new();
    ring.kill("test".into(), false);

    // Yank pop without yank should return None
    assert!(ring.yank_pop().is_none());
}

#[test]
fn test_empty_kill_ignored() {
    let mut ring = KillRing::new();
    ring.kill("".into(), false);
    assert!(ring.is_empty());
}

#[test]
fn test_reset_yank() {
    let mut ring = KillRing::new();
    ring.kill("test".into(), false);
    ring.yank(0);

    assert!(ring.in_yank_state());
    ring.reset_yank();
    assert!(!ring.in_yank_state());
}
