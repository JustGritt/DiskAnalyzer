//! Which parts of the drive the scanner skips, and why.
//!
//! The goal is "everything except the operating system". That is a narrower
//! cut than "everything made by the OS vendor": leftovers such as
//! `C:\Windows.old` or the Recycle Bin are reclaimable, so they stay in the
//! scan. Only the live OS itself and paths that cannot be read without
//! elevated rights are skipped by default. The tables live in `platform`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::platform;

/// Skip reason for a folder named with `--exclude`.
pub const USER_EXCLUDED: &str = "user-excluded";
/// Skip reason for a folder matched by `--exclude-name`.
pub const USER_EXCLUDED_NAME: &str = "user-excluded name";
/// Skip reason for a mount point the walk does not cross by default.
pub const OTHER_FILESYSTEM: &str = "different filesystem (--cross-filesystems to descend)";

/// Comparison key for a directory path: trailing separators dropped and the
/// case folded according to the platform's filesystem conventions.
pub fn normalize_key(display_path: &str) -> String {
    platform::fold_case(display_path.trim_end_matches(['\\', '/']))
}

/// The absolute display form of a path as the user typed it. Roots get the
/// same treatment in the scanner, so `--exclude src/x` run from `/proj`
/// matches the `/proj/src/x` the walk actually sees.
pub fn absolute_display(path: &str) -> String {
    platform::to_display_path(&platform::to_scan_path(Path::new(path)))
}

/// Whether a skip reason comes from the default OS tables, which is the only
/// case where `--include-os` would have scanned the path.
pub fn is_os_reason(reason: &str) -> bool {
    platform::os_exclusions().iter().any(|(_, r)| *r == reason)
}

/// Decides, per directory, whether the scanner descends into it.
#[derive(Debug, Default)]
pub struct ExclusionSet {
    dirs: HashMap<String, &'static str>,
    names: HashSet<String>,
}

impl ExclusionSet {
    pub fn new(include_os: bool, extra_dirs: &[String], extra_names: &[String]) -> ExclusionSet {
        let mut dirs = HashMap::new();
        if !include_os {
            for (path, reason) in platform::os_exclusions() {
                dirs.insert(normalize_key(path), *reason);
            }
        }
        for (path, reason) in platform::always_exclusions() {
            dirs.insert(normalize_key(path), *reason);
        }
        for path in extra_dirs {
            dirs.insert(normalize_key(path), USER_EXCLUDED);
        }
        let names = extra_names
            .iter()
            .map(|name| name.trim().to_lowercase())
            .filter(|name| !name.is_empty())
            .collect();
        ExclusionSet { dirs, names }
    }

    /// Why this directory is skipped, or `None` to descend into it.
    pub fn reason_for(&self, display_path: &str, name: &str) -> Option<&'static str> {
        if let Some(reason) = self.dirs.get(&normalize_key(display_path)) {
            return Some(reason);
        }
        if !self.names.is_empty() && self.names.contains(&name.to_lowercase()) {
            return Some(USER_EXCLUDED_NAME);
        }
        None
    }

    /// Volume-level files the OS sizes for itself (page files and the like),
    /// reported on their own line rather than mixed into folder totals.
    pub fn is_os_managed_file(&self, name: &str, parent_is_volume_root: bool) -> bool {
        if !parent_is_volume_root {
            return false;
        }
        let lower = name.to_lowercase();
        platform::os_managed_names().iter().any(|n| *n == lower)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_exclusions_match_exact_paths_only() {
        let set = ExclusionSet::new(true, &["/tmp/skip-me/".to_string()], &[]);
        assert_eq!(
            set.reason_for("/tmp/skip-me", "skip-me"),
            Some(USER_EXCLUDED)
        );
        assert_eq!(set.reason_for("/tmp/skip-me/child", "child"), None);
        assert_eq!(set.reason_for("/tmp/skip-me-not", "skip-me-not"), None);
    }

    #[test]
    fn name_exclusions_are_case_insensitive_and_trimmed() {
        let set = ExclusionSet::new(true, &[], &[" Node_Modules ".to_string(), "".to_string()]);
        assert_eq!(
            set.reason_for("/x/node_modules", "node_modules"),
            Some(USER_EXCLUDED_NAME)
        );
        assert_eq!(set.reason_for("/x/src", "src"), None);
    }

    #[test]
    fn relative_exclusions_resolve_against_the_current_directory() {
        let cwd = std::env::current_dir().unwrap();
        let cwd = platform::to_display_path(&cwd);
        let expected = platform::join_display(&cwd, "some-dir");
        assert_eq!(absolute_display("some-dir"), expected);
        assert_eq!(absolute_display("./some-dir"), expected);
        assert_eq!(absolute_display("other/../some-dir"), expected);

        let set = ExclusionSet::new(true, &[absolute_display("some-dir")], &[]);
        assert_eq!(set.reason_for(&expected, "some-dir"), Some(USER_EXCLUDED));
    }

    #[test]
    fn only_os_table_reasons_count_as_os() {
        assert!(!is_os_reason(USER_EXCLUDED));
        assert!(!is_os_reason(USER_EXCLUDED_NAME));
        assert!(!is_os_reason(OTHER_FILESYSTEM));
        if let Some((_, reason)) = platform::os_exclusions().first() {
            assert!(is_os_reason(reason));
        }
    }

    #[test]
    fn os_tables_only_apply_without_include_os() {
        let Some((path, reason)) = platform::os_exclusions().first() else {
            return;
        };
        let name = path.rsplit(['\\', '/']).next().unwrap();
        let default = ExclusionSet::new(false, &[], &[]);
        assert_eq!(default.reason_for(path, name), Some(*reason));
        let include = ExclusionSet::new(true, &[], &[]);
        assert_eq!(include.reason_for(path, name), None);
    }

    #[test]
    fn os_managed_files_only_at_volume_root() {
        let set = ExclusionSet::new(false, &[], &[]);
        let Some(name) = platform::os_managed_names().first() else {
            return;
        };
        assert!(set.is_os_managed_file(name, true));
        assert!(set.is_os_managed_file(&name.to_uppercase(), true));
        assert!(!set.is_os_managed_file(name, false));
    }
}
