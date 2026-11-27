//! `--compare`: what changed since an earlier report.
//!
//! The baseline is a previous report directory. `folders.csv` holds the
//! data; `report.json`, when present, says what that scan covered and how it
//! sized things. A folder below the baseline's floor is not in its CSV, so a
//! folder missing from it may have been absent or merely small: it is shown
//! as "new" either way, and changes smaller than the floor are not listed.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::exclusions::{absolute_display, normalize_key};
use crate::insights::CHAIN_COLLAPSE_RATIO;
use crate::platform;
use crate::reports::csv::{split_path, timestamp, CsvReader};
use crate::scanner::{ScanResult, NO_PARENT};
use crate::util::Json;

/// The floor assumed for a baseline that does not record its own: the
/// `--folder-min-size` default.
pub const DEFAULT_FOLDER_FLOOR: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BaselineFolder {
    /// The display path as the baseline wrote it.
    pub path: String,
    pub total: u64,
}

#[derive(Debug, Default)]
pub struct Baseline {
    pub dir: PathBuf,
    /// When the baseline was taken, for headings.
    pub label: String,
    pub epoch: Option<f64>,
    pub roots: Vec<String>,
    /// Bytes the baseline scanned in all.
    pub total: u64,
    pub allocated: Option<bool>,
    pub folder_floor: Option<u64>,
    /// (volume label, used, free)
    pub volumes: Vec<(String, u64, u64)>,
    /// Keyed by `normalize_key` of the path.
    pub folders: HashMap<String, BaselineFolder>,
    /// Whether a usable `report.json` was found.
    pub has_json: bool,
    /// Why a `report.json` that exists could not be used.
    pub json_error: Option<String>,
}

/// `scan-20260914-163501` -> `2026-09-14 16:35`, the stamp the CLI gives
/// default report directories.
fn label_from_dir_name(name: &str) -> Option<String> {
    let stamp = name.strip_prefix("scan-")?;
    let bytes = stamp.as_bytes();
    let well_formed = bytes.len() == 15
        && bytes.iter().enumerate().all(|(i, b)| {
            if i == 8 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    well_formed.then(|| {
        format!(
            "{}-{}-{} {}:{}",
            &stamp[0..4],
            &stamp[4..6],
            &stamp[6..8],
            &stamp[9..11],
            &stamp[11..13]
        )
    })
}

impl Baseline {
    /// Read a report directory. Fails when `folders.csv` is missing or
    /// unreadable; a missing or broken `report.json` only loses metadata.
    pub fn load(dir: &Path) -> Result<Baseline, String> {
        let csv_path = dir.join("folders.csv");
        let text = std::fs::read_to_string(&csv_path)
            .map_err(|err| format!("cannot read {}: {err}", csv_path.display()))?;
        let mut base = Baseline::from_folders_csv(&text)
            .map_err(|err| format!("{}: {err}", csv_path.display()))?;
        base.dir = dir.to_path_buf();
        base.label = dir
            .file_name()
            .and_then(|name| label_from_dir_name(&name.to_string_lossy()))
            .or_else(|| {
                let modified = std::fs::metadata(&csv_path).ok()?.modified().ok()?;
                let epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
                Some(timestamp(epoch.as_secs_f64())).filter(|label| !label.is_empty())
            })
            .unwrap_or_else(|| dir.display().to_string());

        match std::fs::read_to_string(dir.join("report.json")) {
            Ok(text) => match Json::parse(&text) {
                Ok(doc) => base.apply_json(&doc),
                Err(err) => base.json_error = Some(err),
            },
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => base.json_error = Some(err.to_string()),
        }
        Ok(base)
    }

    /// The data half of a baseline: folders by path, and the roots (depth 0)
    /// with their total, for when no `report.json` says otherwise.
    pub fn from_folders_csv(text: &str) -> Result<Baseline, String> {
        let mut records = CsvReader::new(text);
        let header = match records.next() {
            Some(header) => header?,
            None => return Err("the file is empty".to_string()),
        };
        let column = |name: &str| header.iter().position(|h| h == name);
        let (Some(total_col), Some(path_col)) = (column("total_bytes"), column("full_path")) else {
            return Err("no total_bytes and full_path columns".to_string());
        };
        let depth_col = column("depth");

        let mut base = Baseline::default();
        for (index, record) in records.enumerate() {
            let mut record = record.map_err(|err| format!("record {}: {err}", index + 1))?;
            if record.len() == 1 && record[0].is_empty() {
                continue;
            }
            if record.len() <= total_col.max(path_col) {
                return Err(format!("record {} has too few fields", index + 1));
            }
            let total: u64 = record[total_col]
                .parse()
                .map_err(|_| format!("record {}: total_bytes is not a number", index + 1))?;
            let path = std::mem::take(&mut record[path_col]);
            if depth_col
                .and_then(|col| record.get(col))
                .is_some_and(|depth| depth == "0")
            {
                base.roots.push(path.clone());
                base.total += total;
            }
            base.folders
                .insert(normalize_key(&path), BaselineFolder { path, total });
        }
        Ok(base)
    }

    fn apply_json(&mut self, doc: &Json) {
        if doc.get("schema").and_then(Json::as_str) != Some(crate::reports::payload::SCHEMA) {
            self.json_error = Some("not a DiskAnalyzer report".to_string());
            return;
        }
        self.has_json = true;
        if let Some(label) = doc.get("generated").and_then(Json::as_str) {
            if !label.is_empty() {
                self.label = label.to_string();
            }
        }
        self.epoch = doc.get("generatedEpoch").and_then(Json::as_f64);
        if let Some(config) = doc.get("config") {
            if let Some(roots) = config.get("roots").and_then(Json::as_array) {
                self.roots = roots
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect();
            }
            self.allocated = config.get("allocated").and_then(Json::as_bool);
            self.folder_floor = config.get("folderMinSize").and_then(Json::as_u64);
        }
        if let Some(size) = doc
            .get("scan")
            .and_then(|scan| scan.get("size"))
            .and_then(Json::as_u64)
        {
            self.total = size;
        }
        if let Some(volumes) = doc.get("volumes").and_then(Json::as_array) {
            self.volumes = volumes
                .iter()
                .filter_map(|v| {
                    Some((
                        v.get("root")?.as_str()?.to_string(),
                        v.get("used")?.as_u64()?,
                        v.get("free")?.as_u64()?,
                    ))
                })
                .collect();
        }
    }
}

/// Whether two paths name the same directory once made absolute. Neither
/// has to exist yet.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let key = |p: &Path| normalize_key(&absolute_display(&p.to_string_lossy()));
    key(a) == key(b)
}

/// The directory `--compare` names. `last` means the newest
/// `reports/scan-*` under the current directory that has a `folders.csv`,
/// other than the one this run is about to write.
pub fn resolve_baseline_dir(arg: &str, out_dir: &Path) -> Result<PathBuf, String> {
    if arg != "last" {
        return Ok(PathBuf::from(arg));
    }
    let reports = std::env::current_dir()
        .map_err(|err| format!("--compare last: no current directory: {err}"))?
        .join("reports");
    let entries = std::fs::read_dir(&reports)
        .map_err(|err| format!("--compare last: cannot list {}: {err}", reports.display()))?;
    let mut best: Option<(String, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if !name.starts_with("scan-")
            || !path.join("folders.csv").is_file()
            || same_dir(&path, out_dir)
        {
            continue;
        }
        if best.as_ref().is_none_or(|(best_name, _)| name > *best_name) {
            best = Some((name, path));
        }
    }
    best.map(|(_, path)| path).ok_or_else(|| {
        format!(
            "--compare last: no earlier scan with a folders.csv in {}",
            reports.display()
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeStatus {
    Grew,
    Shrank,
    /// Not in the baseline: created since, or grew past its floor.
    New,
    Removed,
    /// Gone from this scan because this scan skipped it.
    NowExcluded,
    /// Present but unreadable this time, so its size reads as less.
    NowUnreadable,
}

impl ChangeStatus {
    pub fn key(self) -> &'static str {
        match self {
            ChangeStatus::Grew => "grew",
            ChangeStatus::Shrank => "shrank",
            ChangeStatus::New => "new",
            ChangeStatus::Removed => "removed",
            ChangeStatus::NowExcluded => "excluded",
            ChangeStatus::NowUnreadable => "unreadable",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub delta: i64,
    pub status: ChangeStatus,
}

#[derive(Debug, Default)]
pub struct Comparison {
    pub base_label: String,
    pub base_dir: String,
    pub base_epoch: Option<f64>,
    pub before_total: u64,
    pub after_total: u64,
    /// (label, used before, used after, free before, free after)
    pub volumes: Vec<(String, u64, u64, u64, u64)>,
    /// Changes smaller than this are not reported.
    pub min_change: u64,
    /// Chains collapsed, largest growth first, at most `top_n`.
    pub grew: Vec<Change>,
    /// Chains collapsed, largest loss first, at most `top_n`.
    pub shrank: Vec<Change>,
    /// Top-most folders gone since the baseline, largest first, at most `top_n`.
    pub removed: Vec<Change>,
    /// Every folder that changed by `min_change` or more, not collapsed,
    /// largest change first: the rows of `changes.csv`.
    pub all: Vec<Change>,
    pub warnings: Vec<String>,
}

/// Whether `key` or any ancestor of it is in `set`. Keys are
/// `normalize_key` forms, so a volume root is `c:` or the empty string.
fn at_or_under(key: &str, set: &HashSet<String>) -> bool {
    if set.is_empty() {
        return false;
    }
    let mut current = key;
    loop {
        if set.contains(current) {
            return true;
        }
        match current.rfind(['\\', '/']) {
            Some(idx) => current = &current[..idx],
            None => return false,
        }
    }
}

fn key_set<'a>(paths: impl Iterator<Item = &'a str>) -> HashSet<String> {
    paths.map(normalize_key).collect()
}

fn parent_key(path: &str) -> Option<String> {
    let (folder, _) = split_path(path);
    (!folder.is_empty()).then(|| normalize_key(folder))
}

/// The smallest change worth reporting: what this run was asked to list
/// (`--folder-min-size`), or what the baseline could resolve if coarser.
pub fn min_change(base: &Baseline, folder_min_size: u64) -> u64 {
    folder_min_size.max(base.folder_floor.unwrap_or(DEFAULT_FOLDER_FLOOR))
}

/// Diff a finished scan against a baseline. Linear in the arena plus the
/// baseline; parents precede children, as everywhere else.
pub fn compare(
    result: &ScanResult,
    base: &Baseline,
    top_n: usize,
    folder_min_size: u64,
) -> Comparison {
    let nodes = &result.nodes;
    let count = nodes.len();
    let min_change = min_change(base, folder_min_size);
    let significant = |delta: i64| delta != 0 && delta.unsigned_abs() >= min_change;

    let keys: Vec<String> = result
        .all_paths()
        .into_iter()
        .map(|path| normalize_key(&path))
        .collect();
    let id_of: HashMap<&str, usize> = keys
        .iter()
        .enumerate()
        .map(|(id, key)| (key.as_str(), id))
        .collect();
    let current_roots: Vec<String> = result
        .root_ids
        .iter()
        .map(|&id| nodes[id as usize].name.to_string())
        .collect();
    let current_root_keys = key_set(current_roots.iter().map(String::as_str));
    let base_root_keys = key_set(base.roots.iter().map(String::as_str));
    let skipped = key_set(result.skipped.iter().map(|(path, _)| path.as_str()));
    let unreadable = key_set(result.errors.iter().map(|(path, _)| path.as_str()));

    // Only what both scans covered can be compared; anything else would
    // show up as wholesale growth or loss. The roots warning says so.
    let before: Vec<Option<u64>> = keys
        .iter()
        .map(|key| base.folders.get(key).map(|folder| folder.total))
        .collect();
    let delta: Vec<i64> = (0..count)
        .map(|id| {
            let in_scope = base_root_keys.is_empty() || at_or_under(&keys[id], &base_root_keys);
            if in_scope {
                nodes[id].total_size as i64 - before[id].unwrap_or(0) as i64
            } else {
                0
            }
        })
        .collect();

    // Per folder, the largest growth and loss among its children, including
    // children that are gone, so a chain collapses to where it fans out.
    let mut child_growth = vec![0i64; count];
    let mut child_loss = vec![0i64; count];
    for (id, node) in nodes.iter().enumerate() {
        if node.parent != NO_PARENT {
            let parent = node.parent as usize;
            child_growth[parent] = child_growth[parent].max(delta[id]);
            child_loss[parent] = child_loss[parent].min(delta[id]);
        }
    }

    let mut all = Vec::new();
    let mut removed = Vec::new();
    for (key, folder) in &base.folders {
        if id_of.contains_key(key.as_str()) || !at_or_under(key, &current_root_keys) {
            continue;
        }
        let change = Change {
            path: folder.path.clone(),
            before: Some(folder.total),
            after: None,
            delta: -(folder.total as i64),
            status: if at_or_under(key, &skipped) {
                ChangeStatus::NowExcluded
            } else {
                ChangeStatus::Removed
            },
        };
        // Top-most only: the parent is still here. Deeper ones are inside it.
        let parent =
            parent_key(&folder.path).and_then(|parent| id_of.get(parent.as_str()).copied());
        if let Some(parent) = parent {
            child_loss[parent] = child_loss[parent].min(change.delta);
            if significant(change.delta) {
                removed.push(change.clone());
            }
        }
        if significant(change.delta) {
            all.push(change);
        }
    }

    let mut grew = Vec::new();
    let mut shrank = Vec::new();
    for id in 0..count {
        let d = delta[id];
        if !significant(d) {
            continue;
        }
        let status = match before[id] {
            None => ChangeStatus::New,
            Some(_) if d > 0 => ChangeStatus::Grew,
            Some(_) if unreadable.contains(&keys[id]) => ChangeStatus::NowUnreadable,
            Some(_) => ChangeStatus::Shrank,
        };
        let change = Change {
            path: result.node_path(id as u32),
            before: before[id],
            after: Some(nodes[id].total_size),
            delta: d,
            status,
        };
        let ratio = CHAIN_COLLAPSE_RATIO;
        if d > 0 && (child_growth[id] as f64) < ratio * d as f64 {
            grew.push(change.clone());
        } else if d < 0 && (child_loss[id].unsigned_abs() as f64) < ratio * d.unsigned_abs() as f64
        {
            shrank.push(change.clone());
        }
        all.push(change);
    }

    grew.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.path.cmp(&b.path)));
    shrank.sort_by(|a, b| a.delta.cmp(&b.delta).then_with(|| a.path.cmp(&b.path)));
    removed.sort_by(|a, b| a.delta.cmp(&b.delta).then_with(|| a.path.cmp(&b.path)));
    all.sort_by(|a, b| {
        b.delta
            .unsigned_abs()
            .cmp(&a.delta.unsigned_abs())
            .then_with(|| a.path.cmp(&b.path))
    });
    grew.truncate(top_n);
    shrank.truncate(top_n);
    removed.truncate(top_n);

    let mut warnings = Vec::new();
    match (&base.json_error, base.has_json) {
        (Some(err), _) => warnings.push(format!(
            "the baseline's report.json could not be used ({err}); cannot confirm it used the same roots and sizing"
        )),
        (None, false) => warnings.push(
            "baseline has no report.json; cannot confirm it used the same roots and sizing"
                .to_string(),
        ),
        _ => {}
    }
    if !base_root_keys.is_empty() && base_root_keys != current_root_keys {
        warnings.push(format!(
            "the baseline scanned {} and this scan {}; only what both cover is compared",
            base.roots.join(", "),
            current_roots.join(", ")
        ));
    }
    if let Some(allocated) = base.allocated {
        if allocated != result.config.use_allocated {
            let kind = |allocated: bool| if allocated { "on-disk" } else { "logical" };
            warnings.push(format!(
                "the baseline measured {} sizes and this scan {}; sizes are not comparable",
                kind(allocated),
                kind(result.config.use_allocated)
            ));
        }
    }

    let volumes =
        base.volumes
            .iter()
            .filter_map(|(label, used_before, free_before)| {
                let (_, (_, used, free)) = result.volumes.iter().find(|(current, _)| {
                    platform::fold_case(current) == platform::fold_case(label)
                })?;
                Some((label.clone(), *used_before, *used, *free_before, *free))
            })
            .collect();

    Comparison {
        base_label: base.label.clone(),
        base_dir: base.dir.display().to_string(),
        base_epoch: base.epoch,
        before_total: base.total,
        after_total: result.total_size,
        volumes,
        min_change,
        grew,
        shrank,
        removed,
        all,
        warnings,
    }
}

/// `changes.csv` rows; the writer lives with the other CSVs.
pub fn change_record(change: &Change) -> [String; 6] {
    [
        change.delta.to_string(),
        crate::util::human_delta(change.delta),
        change.before.map(|b| b.to_string()).unwrap_or_default(),
        change.after.map(|a| a.to_string()).unwrap_or_default(),
        change.status.key().to_string(),
        change.path.clone(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{roll_up, DirNode, ScanConfig};

    const MB: u64 = 1024 * 1024;

    fn dir(name: &str, parent: u32, own_size: u64) -> DirNode {
        DirNode {
            name: name.into(),
            parent,
            own_size,
            ..Default::default()
        }
    }

    fn current() -> ScanResult {
        let mut result = ScanResult::new(ScanConfig::default());
        result.nodes = vec![
            dir("ROOT", NO_PARENT, 0),
            dir("a", 0, 0),
            dir("b", 1, 0),
            dir("c", 2, 10 * MB),
            dir("fresh", 0, 3 * MB),
            dir("keep", 0, 5 * MB),
            dir("locked", 0, 0),
        ];
        result.root_ids = vec![0];
        roll_up(&mut result.nodes);
        result.total_size = result.nodes[0].total_size;
        let excluded = platform::join_display("ROOT", "excl");
        result.skipped = vec![(excluded, crate::exclusions::USER_EXCLUDED)];
        let locked = result.node_path(6);
        result.errors = vec![(locked, "access denied".to_string())];
        result
    }

    fn baseline(result: &ScanResult) -> Baseline {
        let mut base = Baseline {
            has_json: true,
            roots: vec!["ROOT".to_string()],
            folder_floor: Some(MB),
            ..Default::default()
        };
        let mut add = |path: String, total: u64| {
            base.folders
                .insert(normalize_key(&path), BaselineFolder { path, total });
        };
        add(result.node_path(0), 14 * MB + MB / 2);
        add(result.node_path(1), 2 * MB);
        add(result.node_path(2), 2 * MB);
        add(result.node_path(3), 2 * MB);
        add(result.node_path(5), 5 * MB);
        add(result.node_path(6), 2 * MB);
        let gone = platform::join_display("ROOT", "gone");
        add(platform::join_display(&gone, "inner"), 3 * MB);
        add(gone, 4 * MB);
        add(platform::join_display("ROOT", "excl"), MB + MB / 2);
        base
    }

    fn names(changes: &[Change]) -> Vec<(String, ChangeStatus)> {
        changes
            .iter()
            .map(|c| {
                let name = c.path.rsplit(['\\', '/']).next().unwrap().to_string();
                (name, c.status)
            })
            .collect()
    }

    #[test]
    fn changes_collapse_to_where_they_happened() {
        let result = current();
        let base = baseline(&result);
        let cmp = compare(&result, &base, 10, MB);

        // a, b and c all grew by 8 MB; only c explains it.
        assert_eq!(
            names(&cmp.grew),
            vec![
                ("c".to_string(), ChangeStatus::Grew),
                ("fresh".to_string(), ChangeStatus::New)
            ]
        );
        assert_eq!(cmp.grew[0].delta, 8 * MB as i64);
        assert_eq!(cmp.grew[1].before, None);
        assert_eq!(
            names(&cmp.shrank),
            vec![("locked".to_string(), ChangeStatus::NowUnreadable)]
        );
        // Only the top-most removal is listed; the excluded one says why.
        assert_eq!(
            names(&cmp.removed),
            vec![
                ("gone".to_string(), ChangeStatus::Removed),
                ("excl".to_string(), ChangeStatus::NowExcluded)
            ]
        );
        assert_eq!(cmp.removed[0].after, None);
        assert!(cmp.warnings.is_empty(), "{:?}", cmp.warnings);

        // changes.csv gets everything, uncollapsed, largest first.
        let all = names(&cmp.all);
        assert_eq!(all.len(), 9, "{all:?}");
        assert_eq!(cmp.all[0].delta.unsigned_abs(), 8 * MB);
        assert!(all.contains(&("inner".to_string(), ChangeStatus::Removed)));
        assert!(all.contains(&("ROOT".to_string(), ChangeStatus::Grew)));
        assert!(!all.iter().any(|(name, _)| name == "keep"));

        // A coarser floor hides the small ones.
        let coarse = compare(&result, &base, 10, 4 * MB);
        assert_eq!(coarse.min_change, 4 * MB);
        assert_eq!(
            names(&coarse.grew),
            vec![("c".to_string(), ChangeStatus::Grew)]
        );
    }

    #[test]
    fn mismatched_roots_and_missing_metadata_are_warned_about() {
        let result = current();
        let mut base = baseline(&result);
        base.roots = vec!["ELSEWHERE".to_string()];
        base.allocated = Some(true);
        let cmp = compare(&result, &base, 10, MB);
        assert!(
            cmp.warnings.iter().any(|w| w.contains("ELSEWHERE")),
            "{:?}",
            cmp.warnings
        );
        assert!(cmp.warnings.iter().any(|w| w.contains("not comparable")));
        assert!(
            cmp.grew.is_empty() && cmp.shrank.is_empty(),
            "nothing in common to compare"
        );

        let mut base = baseline(&result);
        base.has_json = false;
        base.folder_floor = None;
        let cmp = compare(&result, &base, 10, 0);
        assert_eq!(cmp.min_change, DEFAULT_FOLDER_FLOOR);
        assert!(cmp.warnings[0].contains("no report.json"));
    }

    #[test]
    fn folders_csv_is_read_by_header_name() {
        let text = "\u{feff}depth,full_path,total_bytes\r\n0,/r,30\r\n1,\"/r/with,comma\",20\r\n1,\"/r/line\nbreak\",10\r\n";
        let base = Baseline::from_folders_csv(text).unwrap();
        assert_eq!(base.roots, vec!["/r"]);
        assert_eq!(base.total, 30);
        assert_eq!(base.folders.len(), 3);
        assert_eq!(base.folders[&normalize_key("/r/line\nbreak")].total, 10);
        assert!(Baseline::from_folders_csv("a,b\r\n1,2\r\n").is_err());
        assert!(Baseline::from_folders_csv("total_bytes,full_path\r\nlots,/x\r\n").is_err());
        assert!(Baseline::from_folders_csv("").is_err());
    }

    #[test]
    fn default_report_directories_name_their_time() {
        assert_eq!(
            label_from_dir_name("scan-20260914-163501").as_deref(),
            Some("2026-09-14 16:35")
        );
        assert_eq!(label_from_dir_name("scan-2026-09-14"), None);
        assert_eq!(label_from_dir_name("mine"), None);
    }
}
