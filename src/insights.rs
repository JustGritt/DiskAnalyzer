//! Turning raw scan totals into the handful of facts worth acting on.

use std::io::{Read, Seek, SeekFrom};

use crate::categories::{CatId, Confidence, ALL};
use crate::exclusions;
use crate::platform;
use crate::projects;
use crate::scanner::{FileRec, ScanResult, AGE_BUCKETS, NO_PARENT, SECONDS_PER_DAY};
use crate::util::Json;

/// When a folder holds this much of its parent, the parent tells you nothing
/// the child did not already say, so only the child is worth listing.
pub const CHAIN_COLLAPSE_RATIO: f64 = 0.90;

pub const DUPLICATE_MAX_GROUPS: usize = 400;
pub const DUPLICATE_READ_BUDGET: u64 = 8 * 1024 * 1024 * 1024;
const DUPLICATE_CHUNK: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct DuplicateGroup {
    pub size: u64,
    pub paths: Vec<String>,
}

impl DuplicateGroup {
    /// Bytes recoverable if all but one copy were removed.
    pub fn wasted(&self) -> u64 {
        self.size * (self.paths.len() as u64 - 1)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Reclaimable {
    pub safe: u64,
    pub review: u64,
    pub info: u64,
    pub duplicates: u64,
}

/// Report settings that are not part of the scan itself. Kept on the
/// `Report` so every writer (and `report.json`) can state what it used.
#[derive(Clone, Debug)]
pub struct ReportOptions {
    /// Folders per ranking (`--top`).
    pub top_n: usize,
    /// Files per ranking (`--files`).
    pub file_n: usize,
    pub with_duplicates: bool,
    /// Floor for `files.csv` (`--csv-min-size`).
    pub csv_min_size: u64,
    /// Floor for `folders.csv` (`--folder-min-size`).
    pub folder_min_size: u64,
    /// A project untouched this long counts as idle (`--stale-days`).
    pub stale_days: u64,
}

impl Default for ReportOptions {
    fn default() -> ReportOptions {
        ReportOptions {
            top_n: 25,
            file_n: 30,
            with_duplicates: false,
            csv_min_size: 10 * 1024 * 1024,
            folder_min_size: 1024 * 1024,
            stale_days: 90,
        }
    }
}

/// Reclaimable folders smaller than this are not listed: below it, the
/// time spent looking costs more than the space returned.
pub const RECLAIM_MIN_SIZE: u64 = 64 * 1024 * 1024;

/// How far above a reclaimable folder to look for the project that owns it.
/// Four levels reach `Game/Library/Bee` and `repo/packages/app/node_modules`.
const PROJECT_SEARCH_DEPTH: usize = 4;

/// A top-most regenerable or review folder, with the project it belongs to.
#[derive(Clone, Debug)]
pub struct ReclaimFolder {
    pub id: u32,
    pub category: CatId,
    pub size: u64,
    pub files: u64,
    /// Node id of the owning project, when one was recognized.
    pub project: Option<u32>,
    /// Last activity of the project around the folder, not counting any
    /// reclaimable folder's contents; the folder's own newest mtime when no
    /// project was found. 0 when unknown.
    pub activity: f64,
    /// Newest mtime inside the folder itself.
    pub contents_newest: f64,
    pub stale: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ReclaimFolders {
    /// Largest first, at most `top_n`.
    pub folders: Vec<ReclaimFolder>,
    /// How many folders cleared the size floor, before truncation.
    pub count: usize,
    /// Bytes in all of them. Top-most folders never nest, so this does not
    /// double count.
    pub total: u64,
    /// Bytes in the ones marked stale.
    pub stale: u64,
}

#[derive(Debug, Default)]
pub struct Report {
    pub options: ReportOptions,
    /// Node ids, largest first.
    pub top_folders: Vec<u32>,
    pub hotspots: Vec<u32>,
    /// (root id, child ids largest first)
    pub root_breakdown: Vec<(u32, Vec<u32>)>,
    /// (category, count, size, share)
    pub category_rows: Vec<(CatId, u64, u64, f64)>,
    /// (extension, count, size, share)
    pub extension_rows: Vec<(Box<str>, u64, u64, f64)>,
    /// (label, count, size) in bucket order
    pub age_rows: Vec<(&'static str, u64, u64)>,
    pub top_files: Vec<FileRec>,
    pub old_files: Vec<FileRec>,
    pub duplicates: Vec<DuplicateGroup>,
    pub duplicates_truncated: bool,
    pub reclaimable: Reclaimable,
    pub reclaim_folders: ReclaimFolders,
}

/// Largest `total_size` among each node's direct children.
fn max_child_size(result: &ScanResult) -> Vec<u64> {
    let mut biggest = vec![0u64; result.nodes.len()];
    for node in &result.nodes {
        if node.parent != NO_PARENT {
            let slot = &mut biggest[node.parent as usize];
            if node.total_size > *slot {
                *slot = node.total_size;
            }
        }
    }
    biggest
}

/// Rank folders by total size, keeping only the point where size fans out:
/// a node is kept only when no single child accounts for nearly all of it,
/// so the listed folder is the deepest one that still explains the total.
pub fn collapse_chains(result: &ScanResult, limit: usize) -> Vec<u32> {
    let biggest = max_child_size(result);
    let mut interesting: Vec<u32> = (0..result.nodes.len() as u32)
        .filter(|&id| {
            let total = result.nodes[id as usize].total_size;
            total > 0 && (biggest[id as usize] as f64) < total as f64 * CHAIN_COLLAPSE_RATIO
        })
        .collect();
    interesting.sort_by(|&a, &b| {
        result.nodes[b as usize]
            .total_size
            .cmp(&result.nodes[a as usize].total_size)
            .then(a.cmp(&b))
    });
    interesting.truncate(limit);
    interesting
}

/// The user's home folder, which is never taken as a project: a dotfiles
/// repository in `~` must not claim `~/Library/Caches`.
fn home_dir() -> Option<String> {
    let vars: [&str; 2] = if cfg!(windows) {
        ["USERPROFILE", "HOME"]
    } else {
        ["HOME", "USERPROFILE"]
    };
    vars.iter()
        .filter_map(std::env::var_os)
        .map(|v| v.to_string_lossy().into_owned())
        .find(|v| !v.is_empty())
}

/// Find the top-most regenerable or review folders and the projects they
/// belong to. Linear passes over the arena; parents precede children.
///
/// Why activity skips the folders themselves: build output is rewritten by
/// every build and package stores may keep archive mtimes, so a folder's own
/// timestamps say little about whether anyone still works on the project.
/// The source tree around it does.
pub fn reclaim_folders(
    result: &ScanResult,
    top_n: usize,
    stale_days: u64,
    min_size: u64,
    home: Option<&str>,
) -> ReclaimFolders {
    let nodes = &result.nodes;
    let count = nodes.len();

    // Forward pass: which folders are top-most candidates. A candidate inside
    // another folds into it, and anything inside an installed app or game is
    // that app's business, not a project's leftovers.
    let mut in_app = vec![false; count];
    let mut is_root = vec![false; count];
    let mut covered = vec![false; count];
    for id in 0..count {
        let node = &nodes[id];
        let (parent_in_app, parent_covered) = match node.parent {
            NO_PARENT => (false, false),
            parent => (in_app[parent as usize], covered[parent as usize]),
        };
        in_app[id] = parent_in_app || matches!(node.category, Some(CatId::Apps | CatId::Games));
        let reclaimable = node
            .category
            .is_some_and(|cat| matches!(cat.confidence(), Confidence::Safe | Confidence::Review));
        is_root[id] = reclaimable && !parent_covered && !parent_in_app;
        covered[id] = parent_covered || is_root[id];
    }

    // Backward pass: activity rolls up like `newest`, except that a
    // candidate's subtree never feeds its parent.
    let mut quiet_newest: Vec<f64> = nodes.iter().map(|node| node.own_newest).collect();
    for id in (0..count).rev() {
        let parent = nodes[id].parent;
        if parent == NO_PARENT || is_root[id] {
            continue;
        }
        let parent = parent as usize;
        quiet_newest[parent] = quiet_newest[parent].max(quiet_newest[id]);
    }

    let mut kept: Vec<u32> = (0..count as u32)
        .filter(|&id| is_root[id as usize] && nodes[id as usize].total_size >= min_size)
        .collect();
    kept.sort_by(|&a, &b| {
        nodes[b as usize]
            .total_size
            .cmp(&nodes[a as usize].total_size)
            .then(a.cmp(&b))
    });

    let home_key = home.map(exclusions::normalize_key);
    let not_a_project = |id: u32| {
        let node = &nodes[id as usize];
        (node.parent == NO_PARENT && platform::is_volume_root(&node.name))
            || home_key
                .as_ref()
                .is_some_and(|home| exclusions::normalize_key(&result.node_path(id)) == *home)
    };
    let find_project = |id: u32| {
        let mut nearest_any = None;
        let mut cur = nodes[id as usize].parent;
        for _ in 0..PROJECT_SEARCH_DEPTH {
            if cur == NO_PARENT {
                break;
            }
            let markers = nodes[cur as usize].markers;
            if markers != 0 && !not_a_project(cur) {
                if markers & projects::GIT != 0 {
                    return Some(cur);
                }
                nearest_any.get_or_insert(cur);
            }
            cur = nodes[cur as usize].parent;
        }
        nearest_any
    };

    let stale_after = stale_days as f64 * SECONDS_PER_DAY;
    let mut out = ReclaimFolders {
        count: kept.len(),
        ..Default::default()
    };
    for id in kept {
        let node = &nodes[id as usize];
        let project = find_project(id);
        let own = quiet_newest[id as usize];
        let activity = match project {
            Some(project) if quiet_newest[project as usize] > 0.0 => quiet_newest[project as usize],
            _ => own,
        };
        let stale = activity > 0.0 && result.now - activity >= stale_after;
        out.total += node.total_size;
        if stale {
            out.stale += node.total_size;
        }
        if out.folders.len() < top_n {
            out.folders.push(ReclaimFolder {
                id,
                category: node.category.unwrap_or(CatId::Other),
                size: node.total_size,
                files: node.total_files,
                project,
                activity,
                contents_newest: node.newest,
                stale,
            });
        }
    }
    out
}

fn read_signature(path: &str, size: u64, spent: &mut u64) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut head = vec![0u8; DUPLICATE_CHUNK as usize];
    let head_len = read_fully(&mut file, &mut head)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&head[..head_len]);
    *spent += head_len as u64;
    if size > DUPLICATE_CHUNK * 2 {
        file.seek(SeekFrom::End(-(DUPLICATE_CHUNK as i64)))?;
        let mut tail = vec![0u8; DUPLICATE_CHUNK as usize];
        let tail_len = read_fully(&mut file, &mut tail)?;
        hasher.update(&tail[..tail_len]);
        *spent += tail_len as u64;
    }
    Ok(*hasher.finalize().as_bytes())
}

fn read_fully(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Identify same-size files whose head and tail bytes also match.
///
/// Files are only ever opened for reading, and only the first and last
/// 64 KB of each are read, so a group of 40 GB virtual disks costs a few
/// megabytes of I/O. Matching size, head and tail is strong evidence of a
/// duplicate but not proof; the report says "likely" for that reason.
pub fn find_duplicates(
    result: &mut ScanResult,
    max_groups: usize,
    read_budget: u64,
) -> (Vec<DuplicateGroup>, bool) {
    let mut candidates: Vec<(u64, Vec<String>)> = result
        .dup_candidates
        .drain()
        .filter(|(_, paths)| paths.len() > 1)
        .collect();
    candidates.sort_by(|a, b| {
        let waste_a = a.0 * (a.1.len() as u64 - 1);
        let waste_b = b.0 * (b.1.len() as u64 - 1);
        waste_b.cmp(&waste_a).then(b.0.cmp(&a.0))
    });

    let mut groups = Vec::new();
    let mut truncated = false;
    let mut spent: u64 = 0;

    for (size, paths) in candidates {
        if groups.len() >= max_groups || spent >= read_budget {
            truncated = true;
            break;
        }
        let mut by_signature: Vec<([u8; 32], Vec<String>)> = Vec::new();
        for path in paths {
            match read_signature(&path, size, &mut spent) {
                Ok(sig) => match by_signature.iter_mut().find(|(s, _)| *s == sig) {
                    Some((_, matched)) => matched.push(path),
                    None => by_signature.push((sig, vec![path])),
                },
                Err(err) => result
                    .errors
                    .push((path, format!("duplicate check: {err}"))),
            }
        }
        for (_, mut matched) in by_signature {
            if matched.len() > 1 {
                matched.sort();
                groups.push(DuplicateGroup {
                    size,
                    paths: matched,
                });
            }
        }
    }

    groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then(b.size.cmp(&a.size)));
    (groups, truncated)
}

pub fn build_report(result: &mut ScanResult, options: &ReportOptions) -> Report {
    let top_n = options.top_n;
    let file_n = options.file_n;
    let mut report = Report {
        options: options.clone(),
        top_folders: collapse_chains(result, top_n),
        ..Default::default()
    };

    let mut hotspots: Vec<u32> = (0..result.nodes.len() as u32)
        .filter(|&id| result.nodes[id as usize].own_size > 0)
        .collect();
    hotspots.sort_by(|&a, &b| {
        result.nodes[b as usize]
            .own_size
            .cmp(&result.nodes[a as usize].own_size)
            .then(a.cmp(&b))
    });
    hotspots.truncate(top_n);
    report.hotspots = hotspots;

    for &root in &result.root_ids {
        let mut children: Vec<u32> = (0..result.nodes.len() as u32)
            .filter(|&id| {
                let node = &result.nodes[id as usize];
                node.parent == root && node.total_size > 0
            })
            .collect();
        children.sort_by(|&a, &b| {
            result.nodes[b as usize]
                .total_size
                .cmp(&result.nodes[a as usize].total_size)
                .then(a.cmp(&b))
        });
        report.root_breakdown.push((root, children));
    }

    let total = result.total_size.max(1) as f64;

    let mut rows: Vec<(CatId, u64, u64, f64)> = ALL
        .iter()
        .map(|&cat| {
            let (count, size) = result.cat_stats[cat.index()];
            (cat, count, size, size as f64 / total)
        })
        .filter(|(_, count, _, _)| *count > 0)
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    report.category_rows = rows;

    let mut ext_rows: Vec<(Box<str>, u64, u64, f64)> = result
        .ext_stats
        .iter()
        .map(|(ext, (count, size))| (ext.clone(), *count, *size, *size as f64 / total))
        .collect();
    ext_rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    report.extension_rows = ext_rows;

    report.age_rows = AGE_BUCKETS
        .iter()
        .enumerate()
        .map(|(i, (_, label))| (*label, result.age_stats[i].0, result.age_stats[i].1))
        .collect();

    report.top_files = result.top_files.iter().take(file_n).cloned().collect();
    report.old_files = result.sorted_old_files().into_iter().take(file_n).collect();

    if options.with_duplicates {
        let (groups, truncated) =
            find_duplicates(result, DUPLICATE_MAX_GROUPS, DUPLICATE_READ_BUDGET);
        report.duplicates = groups;
        report.duplicates_truncated = truncated;
    }

    let mut reclaimable = Reclaimable::default();
    for (cat, _count, size, _share) in &report.category_rows {
        match cat.confidence() {
            Confidence::Safe => reclaimable.safe += size,
            Confidence::Review => reclaimable.review += size,
            Confidence::Info => reclaimable.info += size,
        }
    }
    reclaimable.duplicates = report.duplicates.iter().map(DuplicateGroup::wasted).sum();
    report.reclaimable = reclaimable;

    report.reclaim_folders = reclaim_folders(
        result,
        top_n,
        options.stale_days,
        RECLAIM_MIN_SIZE,
        home_dir().as_deref(),
    );

    report
}

/// A nested structure for the HTML treemap, pruned to what is visible.
/// Anything below `min_size` cannot render as more than a sliver, so it is
/// folded into its parent's own bytes rather than shipped to the browser.
pub fn build_tree(result: &ScanResult, min_size: u64, max_nodes: usize) -> Option<Json> {
    let mut kept: Vec<u32> = (0..result.nodes.len() as u32)
        .filter(|&id| result.nodes[id as usize].total_size >= min_size)
        .collect();
    if kept.is_empty() {
        return None;
    }
    if kept.len() > max_nodes {
        kept.sort_by(|&a, &b| {
            result.nodes[b as usize]
                .total_size
                .cmp(&result.nodes[a as usize].total_size)
                .then(a.cmp(&b))
        });
        kept.truncate(max_nodes);
    }

    let mut is_kept = vec![false; result.nodes.len()];
    for &id in &kept {
        is_kept[id as usize] = true;
    }
    let mut children: Vec<Vec<u32>> = vec![Vec::new(); result.nodes.len()];
    for &id in &kept {
        let parent = result.nodes[id as usize].parent;
        if parent != NO_PARENT && is_kept[parent as usize] {
            children[parent as usize].push(id);
        }
    }

    fn emit(result: &ScanResult, children: &[Vec<u32>], id: u32, path: &str) -> (Json, u64) {
        let node = &result.nodes[id as usize];
        let mut kids: Vec<(Json, u64)> = children[id as usize]
            .iter()
            .map(|&child| {
                let mut child_path = path.to_string();
                crate::platform::push_display(&mut child_path, &result.nodes[child as usize].name);
                emit(result, children, child, &child_path)
            })
            .collect();
        kids.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
        let shown: u64 = kids.iter().map(|(_, size)| size).sum();
        let mut payload = Json::obj()
            .set("name", &*node.name)
            .set("path", path)
            .set("size", node.total_size)
            .set("files", node.total_files)
            .set("cat", node.category.unwrap_or(CatId::Other).key())
            .set("newest", node.newest);
        // Bytes not covered by a listed child: files directly here plus every
        // subfolder too small to draw. Without this the treemap would not add up.
        if !kids.is_empty() {
            let remainder = node.total_size.saturating_sub(shown);
            payload = payload.set(
                "children",
                kids.into_iter().map(|(j, _)| j).collect::<Vec<_>>(),
            );
            if remainder > 0 {
                payload = payload.set("rest", remainder);
            }
        }
        (payload, node.total_size)
    }

    let roots: Vec<(Json, u64)> = result
        .root_ids
        .iter()
        .filter(|&&id| is_kept[id as usize] && result.nodes[id as usize].total_size > 0)
        .map(|&id| emit(result, &children, id, &result.nodes[id as usize].name))
        .collect();
    if roots.is_empty() {
        return None;
    }
    if roots.len() == 1 {
        return Some(roots.into_iter().next().unwrap().0);
    }
    let size: u64 = roots.iter().map(|(_, s)| s).sum();
    let newest = result
        .root_ids
        .iter()
        .map(|&id| result.nodes[id as usize].newest)
        .fold(0.0, f64::max);
    Some(
        Json::obj()
            .set("name", "Scanned roots")
            .set("path", "")
            .set("size", size)
            .set("files", result.total_files)
            .set("cat", "other")
            .set("newest", newest)
            .set(
                "children",
                roots.into_iter().map(|(j, _)| j).collect::<Vec<_>>(),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{roll_up, DirNode, ScanConfig};

    fn node(name: &str, parent: u32, depth: u16, own_size: u64) -> DirNode {
        DirNode {
            name: name.into(),
            parent,
            depth,
            own_size,
            own_files: 1,
            ..Default::default()
        }
    }

    const DAY: f64 = SECONDS_PER_DAY;
    const NOW: f64 = 2_000_000_000.0;

    /// A node for the reclaim tests: category, markers and newest file age.
    fn dir(name: &str, parent: u32, cat: Option<CatId>, markers: u16, age_days: f64) -> DirNode {
        DirNode {
            name: name.into(),
            parent,
            category: cat,
            markers,
            own_size: 100,
            own_files: 1,
            own_newest: NOW - age_days * DAY,
            ..Default::default()
        }
    }

    fn reclaim_result(nodes: Vec<DirNode>) -> ScanResult {
        let mut result = fake_result(nodes);
        result.now = NOW;
        result
    }

    fn listed(result: &ScanResult, found: &ReclaimFolders) -> Vec<String> {
        found
            .folders
            .iter()
            .map(|f| result.nodes[f.id as usize].name.to_string())
            .collect()
    }

    #[test]
    fn idle_project_is_judged_by_its_source_not_its_build_output() {
        let result = reclaim_result(vec![
            dir("/", NO_PARENT, None, 0, 1.0),
            dir("proj", 0, None, projects::RUST, 200.0),
            dir("src", 1, None, 0, 200.0),
            dir("target", 1, Some(CatId::DevBuild), 0, 1.0),
            dir("debug", 3, Some(CatId::DevBuild), 0, 1.0),
        ]);
        let found = reclaim_folders(&result, 10, 90, 0, None);
        assert_eq!(listed(&result, &found), vec!["target"]);
        let folder = &found.folders[0];
        assert_eq!(folder.project, Some(1));
        assert_eq!(folder.activity, NOW - 200.0 * DAY);
        assert_eq!(folder.contents_newest, NOW - DAY);
        assert!(folder.stale);
        assert_eq!(found.count, 1);
        assert_eq!(found.total, 200);
        assert_eq!(found.stale, 200);

        // The same project, touched yesterday, is not idle.
        let fresh = reclaim_folders(&result, 10, 250, 0, None);
        assert!(!fresh.folders[0].stale);
        assert_eq!(fresh.stale, 0);
    }

    #[test]
    fn only_the_top_most_reclaimable_folder_is_listed() {
        let result = reclaim_result(vec![
            dir("/", NO_PARENT, None, 0, 1.0),
            dir("node_modules", 0, Some(CatId::DevDeps), 0, 5.0),
            dir("x", 1, Some(CatId::DevDeps), 0, 5.0),
            dir("android", 2, Some(CatId::DevDeps), 0, 5.0),
            dir("build", 3, Some(CatId::DevBuild), 0, 5.0),
        ]);
        let found = reclaim_folders(&result, 10, 90, 0, None);
        assert_eq!(listed(&result, &found), vec!["node_modules"]);
        assert_eq!(found.total, 400);
        // No project around it: activity is the folder's own contents.
        assert_eq!(found.folders[0].project, None);
        assert_eq!(found.folders[0].activity, NOW - 5.0 * DAY);
    }

    #[test]
    fn caches_inside_installed_apps_are_left_out() {
        let result = reclaim_result(vec![
            dir("/", NO_PARENT, None, 0, 1.0),
            dir("Unity", 0, Some(CatId::Apps), 0, 1.0),
            dir("emscripten", 1, Some(CatId::Apps), 0, 1.0),
            dir("cache", 2, Some(CatId::AppCache), 0, 1.0),
        ]);
        let found = reclaim_folders(&result, 10, 90, 0, None);
        assert!(found.folders.is_empty());
        assert_eq!(found.count, 0);
    }

    #[test]
    fn a_repository_in_the_home_folder_is_not_a_project() {
        let home = if cfg!(windows) {
            r"C:\Users\me"
        } else {
            "/home/me"
        };
        let result = reclaim_result(vec![
            dir(home, NO_PARENT, None, projects::GIT, 1.0),
            dir("Library", 0, None, 0, 1.0),
            dir("Caches", 1, Some(CatId::AppCache), 0, 400.0),
        ]);
        let found = reclaim_folders(&result, 10, 90, 0, Some(home));
        assert_eq!(found.folders[0].project, None);
        assert!(found.folders[0].stale, "judged by its own contents instead");

        // Without the home rule the dotfiles repository would claim it.
        let claimed = reclaim_folders(&result, 10, 90, 0, None);
        assert_eq!(claimed.folders[0].project, Some(0));
    }

    #[test]
    fn git_beats_a_nearer_marker_and_the_floor_and_limit_apply() {
        let result = reclaim_result(vec![
            dir("/", NO_PARENT, None, 0, 1.0),
            dir("repo", 0, None, projects::GIT, 10.0),
            dir("packages", 1, None, 0, 10.0),
            dir("app", 2, None, projects::NODE, 10.0),
            dir("node_modules", 3, Some(CatId::DevDeps), 0, 10.0),
            dir("tmp", 1, Some(CatId::Temp), 0, 10.0),
        ]);
        let found = reclaim_folders(&result, 10, 90, 0, None);
        let nm = found
            .folders
            .iter()
            .find(|f| &*result.nodes[f.id as usize].name == "node_modules")
            .unwrap();
        assert_eq!(nm.project, Some(1));

        let limited = reclaim_folders(&result, 1, 90, 0, None);
        assert_eq!(limited.folders.len(), 1);
        assert_eq!(limited.count, 2);
        assert_eq!(limited.total, 200);

        assert_eq!(reclaim_folders(&result, 10, 90, 101, None).count, 0);
    }

    fn fake_result(nodes: Vec<DirNode>) -> ScanResult {
        let mut result = ScanResult::new(ScanConfig::default());
        result.nodes = nodes;
        result.root_ids = vec![0];
        roll_up(&mut result.nodes);
        result.total_size = result.nodes[0].total_size;
        result
    }

    #[test]
    fn chains_collapse_to_the_deepest_explaining_folder() {
        // root(0) -> a(1) -> b(2) holds 100 with b owning 95; c(3) owns 5.
        let result = fake_result(vec![
            node("/", NO_PARENT, 0, 0),
            node("a", 0, 1, 0),
            node("b", 1, 2, 95),
            node("c", 0, 1, 5),
        ]);
        // root: biggest child 95 < 100*0.9=90? no -> collapsed
        // a: biggest child 95 < 95*0.9? no -> collapsed
        // b: no children -> kept; c: kept
        assert_eq!(collapse_chains(&result, 10), vec![2, 3]);
    }

    #[test]
    fn tree_prunes_and_adds_rest() {
        let result = fake_result(vec![
            node("/", NO_PARENT, 0, 10),
            node("big", 0, 1, 100),
            node("tiny", 0, 1, 1),
        ]);
        let tree = build_tree(&result, 50, 3000).unwrap().to_json_string();
        assert!(tree.contains(r#""name":"big""#));
        assert!(!tree.contains("tiny"));
        assert!(tree.contains(r#""rest":11"#));
        assert!(build_tree(&result, 1_000, 3000).is_none());
    }
}
