//! Everything that differs between Windows, macOS and Linux: default roots,
//! what the OS owns and should be skipped, how links and devices are
//! detected, disk usage, terminal facts, and Ctrl-C.
//!
//! The rest of the crate calls these free functions and never touches an
//! OS API directly.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// Fold case the way the platform's filesystem compares names.
pub fn fold_case(text: &str) -> String {
    if CASE_INSENSITIVE {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// Append a name to a display path in place using the platform separator.
pub fn push_display(out: &mut String, name: &str) {
    if !(out.ends_with(PATH_SEP) || out.ends_with('/')) {
        out.push(PATH_SEP);
    }
    out.push_str(name);
}

/// Append a name to a display path using the platform separator.
pub fn join_display(parent: &str, name: &str) -> String {
    let mut out = String::with_capacity(parent.len() + name.len() + 1);
    out.push_str(parent);
    push_display(&mut out, name);
    out
}

/// Append a name to the lowercased, slash-normalized path used for category
/// matching. Produces the same string Python got by lowercasing the full
/// path and replacing backslashes, but without rebuilding it per directory.
pub fn join_lower_slash(parent: &str, name_lower: &str) -> String {
    let mut out = String::with_capacity(parent.len() + name_lower.len() + 1);
    out.push_str(parent);
    if !parent.ends_with('/') {
        out.push('/');
    }
    out.push_str(name_lower);
    out
}

/// The category-matching form of a display path.
pub fn lower_slash(display: &str) -> String {
    display.to_lowercase().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joining_never_doubles_separators() {
        assert_eq!(join_lower_slash("/", "users"), "/users");
        assert_eq!(join_lower_slash("c:/", "users"), "c:/users");
        assert_eq!(join_lower_slash("/users", "me"), "/users/me");
        let root = format!("x{PATH_SEP}");
        assert_eq!(join_display(&root, "a"), format!("x{PATH_SEP}a"));
        assert_eq!(join_display("x", "a"), format!("x{PATH_SEP}a"));
    }
}
