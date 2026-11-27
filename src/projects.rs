//! Recognizing project folders by the files that define them.
//!
//! A build folder's own timestamps say nothing about whether its project is
//! still alive: builds rewrite it constantly, and package stores may keep
//! whatever mtimes the archive had. The project around it does say, and the
//! project is found by its markers (`Cargo.toml`, `.git`, `ProjectSettings`
//! and so on), recorded per directory during the walk.
//!
//! Called for every entry of every directory, so matching is exact and
//! case-sensitive against canonical spellings, with no allocation.

pub const GIT: u16 = 1 << 0;
pub const RUST: u16 = 1 << 1;
pub const NODE: u16 = 1 << 2;
pub const PYTHON: u16 = 1 << 3;
pub const GO: u16 = 1 << 4;
pub const GRADLE: u16 = 1 << 5;
pub const MAVEN: u16 = 1 << 6;
pub const CMAKE: u16 = 1 << 7;
pub const SWIFTPM: u16 = 1 << 8;
pub const DART: u16 = 1 << 9;
pub const GODOT: u16 = 1 << 10;
pub const DOTNET: u16 = 1 << 11;
pub const UNREAL: u16 = 1 << 12;
pub const UNITY: u16 = 1 << 13;
pub const XCODE: u16 = 1 << 14;
/// Recognizable project roots with no kind of their own here (PHP, Ruby).
pub const OTHER: u16 = 1 << 15;

/// Directory names, sorted by byte order for binary search.
const DIR_MARKERS: &[(&str, u16)] = &[(".git", GIT), ("ProjectSettings", UNITY)];

const DIR_SUFFIXES: &[(&str, u16)] = &[(".xcodeproj", XCODE), (".xcworkspace", XCODE)];

/// File names, sorted by byte order for binary search. A `.git` *file* marks
/// a worktree or submodule checkout.
#[rustfmt::skip]
const FILE_MARKERS: &[(&str, u16)] = &[
    (".git", GIT),
    ("CMakeLists.txt", CMAKE),
    ("Cargo.toml", RUST),
    ("Gemfile", OTHER),
    ("Package.swift", SWIFTPM),
    ("build.gradle", GRADLE),
    ("build.gradle.kts", GRADLE),
    ("composer.json", OTHER),
    ("deno.json", NODE),
    ("go.mod", GO),
    ("package.json", NODE),
    ("pom.xml", MAVEN),
    ("project.godot", GODOT),
    ("pubspec.yaml", DART),
    ("pyproject.toml", PYTHON),
    ("settings.gradle", GRADLE),
    ("settings.gradle.kts", GRADLE),
    ("setup.py", PYTHON),
];

const FILE_SUFFIXES: &[(&str, u16)] =
    &[(".csproj", DOTNET), (".sln", DOTNET), (".uproject", UNREAL)];

/// Display names, in bit order.
const KIND_LABELS: [&str; 16] = [
    "git", "rust", "node", "python", "go", "gradle", "maven", "cmake", "swiftpm", "dart", "godot",
    ".net", "unreal", "unity", "xcode", "other",
];

fn lookup(names: &[(&str, u16)], suffixes: &[(&str, u16)], name: &str) -> u16 {
    if let Ok(idx) = names.binary_search_by(|(marker, _)| (*marker).cmp(name)) {
        return names[idx].1;
    }
    for (suffix, bits) in suffixes {
        if name.len() > suffix.len() && name.ends_with(suffix) {
            return *bits;
        }
    }
    0
}

/// The project-marker bits one directory entry contributes to its parent.
pub fn marker_bits(name: &str, is_dir: bool) -> u16 {
    if is_dir {
        lookup(DIR_MARKERS, DIR_SUFFIXES, name)
    } else {
        lookup(FILE_MARKERS, FILE_SUFFIXES, name)
    }
}

/// Labels for every bit set, in a fixed order.
pub fn kind_labels(bits: u16) -> Vec<&'static str> {
    KIND_LABELS
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .map(|(_, label)| *label)
        .collect()
}

/// The single most descriptive label: the build system when there is one,
/// `git` only when that is all there is.
pub fn primary_kind(bits: u16) -> Option<&'static str> {
    let labels = kind_labels(bits);
    labels
        .iter()
        .find(|label| **label != "git")
        .or(labels.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_for_binary_search() {
        for table in [DIR_MARKERS, FILE_MARKERS] {
            for pair in table.windows(2) {
                assert!(pair[0].0 < pair[1].0, "{} !< {}", pair[0].0, pair[1].0);
            }
        }
    }

    #[test]
    fn names_and_suffixes_match_by_kind_of_entry() {
        assert_eq!(marker_bits(".git", true), GIT);
        assert_eq!(marker_bits(".git", false), GIT);
        assert_eq!(marker_bits("ProjectSettings", true), UNITY);
        assert_eq!(marker_bits("ProjectSettings", false), 0);
        assert_eq!(marker_bits("App.xcodeproj", true), XCODE);
        assert_eq!(marker_bits("App.xcodeproj", false), 0);
        assert_eq!(marker_bits("Cargo.toml", false), RUST);
        assert_eq!(marker_bits("Cargo.toml", true), 0);
        assert_eq!(marker_bits("build.gradle.kts", false), GRADLE);
        assert_eq!(marker_bits("Game.uproject", false), UNREAL);
        assert_eq!(marker_bits("Tool.sln", false), DOTNET);
        assert_eq!(marker_bits("Gemfile", false), OTHER);
        // A bare suffix is not a project file.
        assert_eq!(marker_bits(".sln", false), 0);
        assert_eq!(marker_bits("main.rs", false), 0);
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert_eq!(marker_bits("cargo.toml", false), 0);
        assert_eq!(marker_bits("PACKAGE.JSON", false), 0);
        assert_eq!(marker_bits(".GIT", true), 0);
        assert_eq!(marker_bits("projectsettings", true), 0);
    }

    #[test]
    fn labels_follow_bit_order() {
        assert!(kind_labels(0).is_empty());
        assert_eq!(kind_labels(GIT | RUST | NODE), vec!["git", "rust", "node"]);
        assert_eq!(kind_labels(OTHER | XCODE), vec!["xcode", "other"]);
        assert_eq!(primary_kind(GIT | UNITY), Some("unity"));
        assert_eq!(primary_kind(GIT), Some("git"));
        assert_eq!(primary_kind(0), None);
    }
}
