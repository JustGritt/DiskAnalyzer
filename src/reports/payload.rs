//! The report as one JSON document. `report.html` embeds it for its script
//! and `report.json` is the same document on its own, so both always agree.
//!
//! Keys are only ever added: the page ignores what it does not read, and
//! anything reading `report.json` can rely on `schemaVersion` for the rest.

use super::csv::timestamp;
use crate::categories::{CatId, ALL};
use crate::compare::{Change, Comparison};
use crate::exclusions;
use crate::insights::{build_tree, Report, RECLAIM_MIN_SIZE};
use crate::platform;
use crate::projects;
use crate::scanner::{FileRec, ScanResult};
use crate::util::Json;

/// Identifies the document type for anything reading `report.json`.
pub const SCHEMA: &str = "diskanalyzer.report";
/// Bumped only when an existing key changes meaning or disappears.
pub const SCHEMA_VERSION: u64 = 1;

pub const TREE_MIN_SIZE: u64 = 8 * 1024 * 1024;
pub const TREE_MAX_NODES: usize = 3000;

fn file_rows(files: &[FileRec]) -> Vec<Json> {
    files
        .iter()
        .map(|rec| {
            Json::obj()
                .set("path", &*rec.path)
                .set("size", rec.size)
                .set("mtime", rec.mtime)
        })
        .collect()
}

fn folder_row(result: &ScanResult, id: u32, size: u64, files: u64) -> Json {
    let node = &result.nodes[id as usize];
    Json::obj()
        .set("path", result.node_path(id))
        .set("size", size)
        .set("files", files)
        .set("cat", node.category.unwrap_or(CatId::Other).key())
}

fn strings<S: AsRef<str>>(items: &[S]) -> Vec<Json> {
    items.iter().map(|s| Json::from(s.as_ref())).collect()
}

/// Skip counts per reason, most common first, so the page can say whether
/// `--include-os` is relevant at all.
fn skipped_by_reason(result: &ScanResult) -> Vec<Json> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for (_, reason) in &result.skipped {
        match counts.iter_mut().find(|(r, _)| *r == *reason) {
            Some((_, count)) => *count += 1,
            None => counts.push((reason, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    counts
        .into_iter()
        .map(|(reason, count)| {
            Json::obj()
                .set("reason", reason)
                .set("count", count)
                .set("os", exclusions::is_os_reason(reason))
        })
        .collect()
}

/// Everything that shaped the numbers, so a later reader (or `--compare`)
/// can tell whether two reports measured the same thing the same way.
fn config_block(report: &Report, result: &ScanResult) -> Json {
    let config = &result.config;
    let options = &report.options;
    let roots: Vec<String> = result
        .root_ids
        .iter()
        .map(|&id| result.nodes[id as usize].name.to_string())
        .collect();
    // Stored resolved: a relative path means nothing without the directory
    // the scan ran from.
    let exclude: Vec<String> = config
        .exclude_dirs
        .iter()
        .map(|path| exclusions::absolute_display(path))
        .collect();
    Json::obj()
        .set("roots", strings(&roots))
        .set("includeOs", config.include_os)
        .set("allocated", config.use_allocated)
        .set("clusterSize", config.cluster_size)
        .set("dedupHardlinks", config.dedup_hardlinks)
        .set("crossFilesystems", config.cross_filesystems)
        .set("exclude", strings(&exclude))
        .set("excludeName", strings(&config.exclude_names))
        .set("csvMinSize", options.csv_min_size)
        .set("folderMinSize", options.folder_min_size)
        .set("oldMinSize", config.old_file_min_size)
        .set("oldMinDays", config.old_file_min_days)
        .set("duplicates", config.find_duplicates)
        .set("dupMinSize", config.duplicate_min_size)
        .set("top", options.top_n)
        .set("files", options.file_n)
        .set("staleDays", options.stale_days)
}

fn change_rows(changes: &[Change]) -> Vec<Json> {
    changes
        .iter()
        .map(|change| {
            Json::obj()
                .set("path", change.path.as_str())
                .set("before", change.before)
                .set("after", change.after)
                .set("delta", change.delta)
                .set("status", change.status.key())
        })
        .collect()
}

fn comparison_block(comparison: &Comparison) -> Json {
    let volumes: Vec<Json> = comparison
        .volumes
        .iter()
        .map(|(root, used_before, used_after, free_before, free_after)| {
            Json::obj()
                .set("root", root.as_str())
                .set("usedBefore", *used_before)
                .set("usedAfter", *used_after)
                .set("freeBefore", *free_before)
                .set("freeAfter", *free_after)
        })
        .collect();
    Json::obj()
        .set("base", comparison.base_label.as_str())
        .set("baseDir", comparison.base_dir.as_str())
        .set("baseEpoch", comparison.base_epoch)
        .set("beforeTotal", comparison.before_total)
        .set("afterTotal", comparison.after_total)
        .set("minChange", comparison.min_change)
        .set("volumes", volumes)
        .set("grew", change_rows(&comparison.grew))
        .set("shrank", change_rows(&comparison.shrank))
        .set("removed", change_rows(&comparison.removed))
        .set("changedFolders", comparison.all.len())
        .set("warnings", strings(&comparison.warnings))
}

fn reclaim_folder_rows(report: &Report, result: &ScanResult) -> Vec<Json> {
    report
        .reclaim_folders
        .folders
        .iter()
        .map(|folder| {
            let project = folder.project.map(|id| &result.nodes[id as usize]);
            Json::obj()
                .set("path", result.node_path(folder.id))
                .set("size", folder.size)
                .set("files", folder.files)
                .set("cat", folder.category.key())
                .set("project", project.map(|node| node.name.to_string()))
                .set("projectPath", folder.project.map(|id| result.node_path(id)))
                .set(
                    "projectKinds",
                    strings(
                        &project.map_or(Vec::new(), |node| projects::kind_labels(node.markers)),
                    ),
                )
                .set("activity", folder.activity)
                .set("newest", folder.contents_newest)
                .set("stale", folder.stale)
                .set("hint", folder.hint)
        })
        .collect()
}

pub fn build_payload(report: &Report, result: &ScanResult) -> Json {
    let total = result.total_size.max(1) as f64;

    let scan = Json::obj()
        .set("files", result.total_files)
        .set("dirs", result.total_dirs)
        .set("size", result.total_size)
        .set("logical", result.logical_size)
        .set("elapsed", result.elapsed)
        .set("now", result.now)
        .set(
            "roots",
            result
                .root_ids
                .iter()
                .map(|&id| Json::from(result.node_path(id)))
                .collect::<Vec<_>>(),
        )
        .set("skipped", result.skipped.len())
        .set("skippedByReason", skipped_by_reason(result))
        .set("oldMinDays", result.config.old_file_min_days)
        .set("oldMinSize", result.config.old_file_min_size)
        .set("errors", result.errors.len())
        .set("links", result.links.len())
        .set("cloudOnlyFiles", result.cloud_only.0)
        .set("cloudOnlyBytes", result.cloud_only.1)
        .set("allocated", result.config.use_allocated)
        .set("includeOs", result.config.include_os)
        .set("osName", platform::OS_NAME)
        .set("errorsHint", platform::ERRORS_HINT);

    let volumes: Vec<Json> = result
        .volumes
        .iter()
        .map(|(root, (total, used, free))| {
            Json::obj()
                .set("root", root.as_str())
                .set("total", *total)
                .set("used", *used)
                .set("free", *free)
        })
        .collect();

    let reclaim = Json::obj()
        .set("safe", report.reclaimable.safe)
        .set("review", report.reclaimable.review)
        .set("info", report.reclaimable.info)
        .set("duplicates", report.reclaimable.duplicates);

    let categories: Vec<Json> = report
        .category_rows
        .iter()
        .map(|(cat, count, size, share)| {
            Json::obj()
                .set("key", cat.key())
                .set("label", cat.label())
                .set("confidence", cat.confidence().key())
                .set("hint", cat.hint())
                .set("count", *count)
                .set("size", *size)
                .set("share", *share)
        })
        .collect();

    let folders: Vec<Json> = report
        .top_folders
        .iter()
        .map(|&id| {
            let node = &result.nodes[id as usize];
            folder_row(result, id, node.total_size, node.total_files)
        })
        .collect();

    let hotspots: Vec<Json> = report
        .hotspots
        .iter()
        .map(|&id| {
            let node = &result.nodes[id as usize];
            folder_row(result, id, node.own_size, node.own_files as u64)
        })
        .collect();

    let extensions: Vec<Json> = report
        .extension_rows
        .iter()
        .take(40)
        .map(|(ext, count, size, share)| {
            Json::obj()
                .set("ext", &**ext)
                .set("count", *count)
                .set("size", *size)
                .set("share", *share)
        })
        .collect();

    let ages: Vec<Json> = report
        .age_rows
        .iter()
        .map(|(label, count, size)| {
            Json::obj()
                .set("label", *label)
                .set("count", *count)
                .set("size", *size)
                .set("share", *size as f64 / total)
        })
        .collect();

    let duplicates: Vec<Json> = report
        .duplicates
        .iter()
        .take(150)
        .map(|group| {
            Json::obj()
                .set("size", group.size)
                .set("wasted", group.wasted())
                .set("paths", strings(&group.paths))
        })
        .collect();

    let mut managed: Vec<&(String, u64)> = result.os_managed.iter().collect();
    managed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let os_managed: Vec<Json> = managed
        .into_iter()
        .map(|(path, size)| Json::obj().set("path", path.as_str()).set("size", *size))
        .collect();

    let skipped_paths: Vec<Json> = result
        .skipped
        .iter()
        .take(200)
        .map(|(path, reason)| {
            Json::obj()
                .set("path", path.as_str())
                .set("reason", *reason)
        })
        .collect();

    let mut category_meta = Json::obj();
    for cat in ALL {
        category_meta = category_meta.set(
            cat.key(),
            Json::obj()
                .set("label", cat.label())
                .set("confidence", cat.confidence().key())
                .set("hint", cat.hint()),
        );
    }

    Json::obj()
        .set("schema", SCHEMA)
        .set("schemaVersion", SCHEMA_VERSION)
        .set("toolVersion", crate::VERSION)
        .set("generated", timestamp(result.started_at))
        .set("generatedEpoch", result.started_at)
        .set("config", config_block(report, result))
        .set("scan", scan)
        .set("compare", report.comparison.as_ref().map(comparison_block))
        .set("volumes", volumes)
        .set("tree", build_tree(result, TREE_MIN_SIZE, TREE_MAX_NODES))
        .set("reclaim", reclaim)
        .set("categories", categories)
        .set("reclaimFolders", reclaim_folder_rows(report, result))
        .set("reclaimFoldersCount", report.reclaim_folders.count)
        .set("reclaimFoldersTotal", report.reclaim_folders.total)
        .set("reclaimFoldersStale", report.reclaim_folders.stale)
        .set("reclaimFoldersMinSize", RECLAIM_MIN_SIZE)
        .set("folders", folders)
        .set("hotspots", hotspots)
        .set("files", file_rows(&report.top_files))
        .set("oldFiles", file_rows(&report.old_files))
        .set("extensions", extensions)
        .set("ages", ages)
        .set("duplicates", duplicates)
        .set("duplicatesTruncated", report.duplicates_truncated)
        .set("osManaged", os_managed)
        .set("skippedPaths", skipped_paths)
        .set("categoryMeta", category_meta)
}
