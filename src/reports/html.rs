//! A single self-contained HTML report.
//!
//! Everything is inlined into one file: no CDN, no network, no companion
//! assets. The treemap is the point of this report: a table tells you the
//! ten biggest folders; a treemap shows the shape of the whole drive at
//! once. The page itself is `template.html`; this module only builds the
//! JSON payload it reads.

use std::io::{self, Write};

use super::csv::timestamp;
use super::sink::OutputDir;
use crate::categories::{CatId, ALL};
use crate::exclusions;
use crate::insights::{build_tree, Report};
use crate::platform;
use crate::scanner::{FileRec, ScanResult};
use crate::util::{now_epoch, Json};

const TEMPLATE: &str = include_str!("template.html");

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

pub fn build_payload(report: &Report, result: &ScanResult) -> Json {
    let total = result.total_size.max(1) as f64;

    let scan = Json::obj()
        .set("files", result.total_files)
        .set("dirs", result.total_dirs)
        .set("size", result.total_size)
        .set("logical", result.logical_size)
        .set("elapsed", result.elapsed)
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
                .set(
                    "paths",
                    group
                        .paths
                        .iter()
                        .map(|p| Json::from(p.as_str()))
                        .collect::<Vec<_>>(),
                )
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
        .set("generated", timestamp(now_epoch()))
        .set("scan", scan)
        .set("volumes", volumes)
        .set("tree", build_tree(result, TREE_MIN_SIZE, TREE_MAX_NODES))
        .set("reclaim", reclaim)
        .set("categories", categories)
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

/// The complete document as a string.
pub fn render(report: &Report, result: &ScanResult) -> String {
    // `</script>` inside the JSON would end the block early; escaping the
    // slash keeps the document valid without changing what JSON.parse sees.
    let blob = build_payload(report, result)
        .to_json_string()
        .replace("</", "<\\/");
    TEMPLATE.replace("__PAYLOAD__", &blob)
}

pub fn write_html(report: &Report, result: &ScanResult, out: &OutputDir) -> io::Result<()> {
    let mut file = out.create_file("report.html")?;
    file.write_all(render(report, result).as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_has_payload_slot_and_no_external_assets() {
        assert_eq!(TEMPLATE.matches("__PAYLOAD__").count(), 1);
        assert!(!TEMPLATE.contains("http://"));
        assert!(!TEMPLATE.contains("https://"));
    }
}
