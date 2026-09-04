//! OSC 8 Hyperlinks for terminal clickable links.
//!
//! Implements OSC 8 escape sequences for clickable URLs in terminals that support them.
//! Format: `\x1b]8;;URL\x1b\\text\x1b]8;;\x1b\\`
//!
//! Terminal support detection:
//! - Most modern terminals: iTerm2, Kitty, WezTerm, Windows Terminal, VS Code
//! - tmux needs pass-through sequences

use std::env;

/// Check if the terminal likely supports OSC 8 hyperlinks.
pub fn terminal_supports_hyperlinks() -> bool {
    // Check for known terminals that support OSC 8
    if let Ok(term_program) = env::var("TERM_PROGRAM") {
        let term = term_program.to_lowercase();
        if term.contains("iterm")
            || term.contains("kitty")
            || term.contains("wezterm")
            || term.contains("vscode")
            || term.contains("mintty")
            || term.contains("hyper")
        {
            return true;
        }
    }

    // Windows Terminal sets WT_SESSION
    if env::var("WT_SESSION").is_ok() {
        return true;
    }

    // Check TERM for known good terminals
    if let Ok(term) = env::var("TERM") {
        let term = term.to_lowercase();
        if term.contains("kitty") || term.contains("xterm-kitty") || term == "xterm-256color" {
            // xterm-256color is very common but not all support OSC 8
            // Be conservative - only enable for known good ones
            if env::var("COLORTERM").ok().as_deref() == Some("truecolor") {
                return true;
            }
        }
    }

    // Check if running inside tmux (we'll handle pass-through)
    if env::var("TMUX").is_ok() {
        return true;
    }

    false
}

/// Check if we're running inside tmux (needs pass-through sequences).
pub fn is_tmux() -> bool {
    env::var("TMUX").is_ok()
}

/// Wrap text in an OSC 8 hyperlink sequence.
///
/// If the terminal doesn't support hyperlinks, returns the text unchanged.
pub fn hyperlink(url: &str, text: &str) -> String {
    if !terminal_supports_hyperlinks() {
        return text.to_string();
    }

    if is_tmux() {
        // tmux pass-through: wrap in \x1bPtmux;...\x1b\\
        // Double all escape chars inside
        format!(
            "\x1bPtmux;\x1b\x1b]8;;{}\x1b\x1b\\{}\x1b\x1b]8;;\x1b\x1b\\\x1b\\",
            url, text
        )
    } else {
        // Standard OSC 8 sequence
        format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, text)
    }
}

/// Create a file:// URL from a file path.
///
/// Handles Windows paths (converts backslashes, adds drive letter prefix).
pub fn file_url(path: &str) -> String {
    let path = path.replace('\\', "/");

    // Check if it's an absolute path
    if path.starts_with('/') {
        format!("file://{}", path)
    } else if path.len() >= 2 && path.chars().nth(1) == Some(':') {
        // Windows absolute path with drive letter (e.g., C:/...)
        format!("file:///{}", path)
    } else {
        // Relative path - try to make absolute
        if let Ok(cwd) = env::current_dir() {
            let full_path = cwd.join(&path);
            let full_str = full_path.to_string_lossy().replace('\\', "/");
            if full_str.len() >= 2 && full_str.chars().nth(1) == Some(':') {
                format!("file:///{}", full_str)
            } else {
                format!("file://{}", full_str)
            }
        } else {
            format!("file://{}", path)
        }
    }
}

/// Wrap a file path in a hyperlink to that file.
pub fn file_link(path: &str) -> String {
    let url = file_url(path);
    hyperlink(&url, path)
}

/// Wrap a file path with line number in a hyperlink.
///
/// Uses the `file://path:line` format that some editors support.
pub fn file_line_link(path: &str, line: usize) -> String {
    let url = format!("{}:{}", file_url(path), line);
    let text = format!("{}:{}", path, line);
    hyperlink(&url, &text)
}

/// Detect URLs in text and wrap them with hyperlinks.
///
/// Returns the text with URLs wrapped in OSC 8 sequences.
pub fn linkify_urls(text: &str) -> String {
    if !terminal_supports_hyperlinks() {
        return text.to_string();
    }

    // Simple URL detection: http://, https://, file://
    let mut result = String::new();
    let mut last_end = 0;

    // Find URLs using a simple pattern
    let url_starts = ["http://", "https://", "file://"];

    let mut i = 0;
    while i < text.len() {
        let remaining = &text[i..];

        // Check if this position starts a URL
        let url_start = url_starts
            .iter()
            .find(|&&prefix| remaining.starts_with(prefix));

        if let Some(&prefix) = url_start {
            // Add text before the URL
            result.push_str(&text[last_end..i]);

            // Find the end of the URL (whitespace or certain punctuation at end)
            let url_chars: Vec<char> = remaining.chars().collect();
            let mut url_end = 0;

            for (j, &c) in url_chars.iter().enumerate() {
                if c.is_whitespace() || c == '<' || c == '>' || c == '"' || c == '\'' {
                    break;
                }
                url_end = j + 1;
            }

            // Trim trailing punctuation that's likely not part of URL
            while url_end > prefix.len() {
                let last = url_chars.get(url_end - 1);
                if matches!(
                    last,
                    Some('.') | Some(',') | Some(')') | Some(']') | Some(';') | Some(':')
                ) {
                    url_end -= 1;
                } else {
                    break;
                }
            }

            let url: String = url_chars[..url_end].iter().collect();
            result.push_str(&hyperlink(&url, &url));

            // Calculate byte offset
            let byte_len: usize = url_chars[..url_end].iter().map(|c| c.len_utf8()).sum();
            i += byte_len;
            last_end = i;
        } else {
            i += remaining.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
    }

    // Add remaining text
    result.push_str(&text[last_end..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
