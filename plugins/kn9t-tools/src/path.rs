//! Path resolution against the session's working directory.
//!
//! A plugin is a long-lived subprocess spawned by the server, so `std::env::current_dir()`
//! inside it is wherever the *server* was started. That is unrelated to the session a call
//! belongs to, and it is shared by every session at once — two sessions rooted in different
//! repos would resolve the same relative path to the same wrong file.
//!
//! The host sends the session's own directory as `cwd` in every tool payload
//! (`ToolCallCtx::cwd`). Relative paths must be resolved against that, never against the
//! process cwd.

use std::path::PathBuf;

/// Resolve a tool's `path` argument against the session's `cwd`.
///
/// Absolute paths are returned untouched — the model asked for a specific file and there is
/// nothing to resolve. A relative path is joined onto `cwd`. When the host reported no `cwd`
/// the path is returned as-is, which falls back to the process cwd: not right, but it is what
/// happened before this existed, and refusing the call would be worse.
pub fn resolve(path: &str, cwd: Option<&PathBuf>) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        return p;
    }
    match cwd {
        Some(base) => base.join(p),
        None => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_is_joined_onto_the_session_cwd() {
        let cwd = PathBuf::from("/work/repo");
        assert_eq!(
            resolve("src/main.rs", Some(&cwd)),
            PathBuf::from("/work/repo").join("src/main.rs")
        );
    }

    #[test]
    fn absolute_is_left_alone() {
        let cwd = PathBuf::from("/work/repo");
        // An absolute path must not be re-rooted under cwd.
        #[cfg(windows)]
        let abs = r"C:\elsewhere\file.txt";
        #[cfg(not(windows))]
        let abs = "/elsewhere/file.txt";
        assert_eq!(resolve(abs, Some(&cwd)), PathBuf::from(abs));
    }

    #[test]
    fn no_cwd_falls_back_to_the_bare_path() {
        assert_eq!(resolve("src/main.rs", None), PathBuf::from("src/main.rs"));
    }

    #[test]
    fn a_dot_relative_path_still_lands_under_cwd() {
        let cwd = PathBuf::from("/work/repo");
        // `./x` and `x` must agree about which directory they are relative to.
        assert!(resolve("./notes.md", Some(&cwd)).starts_with(&cwd));
    }
}
