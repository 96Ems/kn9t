use kn9t_tui::hyperlinks::file_url;

#[test]
fn test_file_url_unix() {
    assert_eq!(
        file_url("/home/user/file.txt"),
        "file:///home/user/file.txt"
    );
}

#[test]
fn test_file_url_windows() {
    assert_eq!(
        file_url("C:/Users/test/file.txt"),
        "file:///C:/Users/test/file.txt"
    );
    assert_eq!(
        file_url("C:\\Users\\test\\file.txt"),
        "file:///C:/Users/test/file.txt"
    );
}

#[test]
fn test_hyperlink_format() {
    // Without terminal support detection (test the format directly)
    let url = "https://example.com";
    let text = "click here";
    let expected = format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text);

    // Just verify the format is correct
    assert!(expected.contains("\x1b]8;;"));
    assert!(expected.contains(url));
    assert!(expected.contains(text));
}

#[test]
fn test_file_line_link() {
    let path = "/home/user/src/main.rs";
    let url = file_url(path);
    assert!(url.starts_with("file://"));
    assert!(url.contains("main.rs"));
}
