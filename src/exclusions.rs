//! Which parts of the drive the scanner skips, and why.
//!
//! The goal is "everything except the operating system". That is a narrower
//! cut than "everything made by the OS vendor": leftovers such as
//! `C:\Windows.old` or the Recycle Bin are reclaimable, so they stay in the
//! scan. Only the live OS itself and paths that cannot be read without
//! elevated rights are skipped by default. The tables live in `platform`.

use std::collections::{HashMap, HashSet};

use crate::platform;

/// Comparison key for a directory path: trailing separators dropped and the
/// case folded according to the platform's filesystem conventions.
pub fn normalize_key(display_path: &str) -> String {
    platform::fold_case(display_path.trim_end_matches(['\\', '/']))
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
            dirs.insert(normalize_key(path), "user-excluded");
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
            return Some("user-excluded name");
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
            Some("user-excluded")
        );
        assert_eq!(set.reason_for("/tmp/skip-me/child", "child"), None);
        assert_eq!(set.reason_for("/tmp/skip-me-not", "skip-me-not"), None);
    }

    #[test]
    fn name_exclusions_are_case_insensitive_and_trimmed() {
        let set = ExclusionSet::new(true, &[], &[" Node_Modules ".to_string(), "".to_string()]);
        assert_eq!(
            set.reason_for("/x/node_modules", "node_modules"),
            Some("user-excluded name")
        );
        assert_eq!(set.reason_for("/x/src", "src"), None);
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
