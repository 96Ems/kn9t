use kn9t_tui::prompt_stash::PromptStash;

#[test]
fn test_stash_unstash() {
    let mut stash = PromptStash::new();

    stash.stash("hello world", 0, 5);
    assert!(stash.has_content());
    assert_eq!(stash.peek(), Some("hello world"));

    let (text, row, col) = stash.unstash().unwrap();
    assert_eq!(text, "hello world");
    assert_eq!(row, 0);
    assert_eq!(col, 5);

    assert!(!stash.has_content());
}

#[test]
fn test_empty_stash() {
    let mut stash = PromptStash::new();
    assert!(!stash.has_content());
    assert!(stash.unstash().is_none());
}

#[test]
fn test_stash_empty_text_ignored() {
    let mut stash = PromptStash::new();
    stash.stash("", 0, 0);
    assert!(!stash.has_content());
}

#[test]
fn test_stash_overwrites() {
    let mut stash = PromptStash::new();

    stash.stash("first", 0, 1);
    stash.stash("second", 1, 2);

    let (text, row, col) = stash.unstash().unwrap();
    assert_eq!(text, "second");
    assert_eq!(row, 1);
    assert_eq!(col, 2);
}
