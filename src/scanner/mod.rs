//! The scan engine: one read-only walk of the configured roots.
//!
//! Design notes
//! ------------
//! *Single pass.* Every statistic in the report is accumulated during one
//! walk. Nothing is computed by re-reading the disk afterwards.
//!
//! *Bounded memory.* A full drive can hold millions of files; keeping a
//! record per file would cost gigabytes. The walk keeps one node per
//! *directory* and streams file-level data into fixed-size structures: a
//! top-N heap, an extension histogram, age buckets, and the CSV writer.
//!
//! *Directory-parallel.* A pool of workers pulls directories from one queue
//! and pushes the subdirectories it finds back onto it. Per-file statistics
//! are accumulated thread-locally and merged once at the end; the only
//! things that must be owned once - the directory arena and the streaming
//! CSV - live on a single aggregator thread fed by a channel. Workers never
//! take a lock on the hot path.
//!
//! *No link traversal.* Symlinks, junctions and mount points are recorded
//! but never descended into: following them would double count or wander
//! onto another disk.

mod aggregate;
mod worker;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

use crossbeam_channel::unbounded;

use crate::categories::{self, CatId, NUM_CATEGORIES};
use crate::exclusions::{self, ExclusionSet};
use crate::platform;
use crate::util::now_epoch;

pub use worker::split_ext as worker_split_ext;
pub use worker::FileRec;

/// Set by the Ctrl-C handler; workers stop at the next directory boundary.
pub static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Files kept in the largest-files heap. Not user-facing; the report shows
/// the top `--files` of these.
pub const TOP_FILES_LIMIT: usize = 2000;

pub const NO_PARENT: u32 = u32::MAX;

pub const SECONDS_PER_DAY: f64 = 86400.0;

/// Age buckets in days; the final entry is the open-ended tail.
pub const AGE_BUCKETS: [(Option<u64>, &str); 6] = [
    (Some(30), "under 1 month"),
    (Some(90), "1-3 months"),
    (Some(365), "3-12 months"),
    (Some(730), "1-2 years"),
    (Some(1825), "2-5 years"),
    (None, "over 5 years"),
];

pub fn age_bucket(mtime: f64, now: f64) -> usize {
    let days = (now - mtime) / SECONDS_PER_DAY;
    for (i, (limit, _)) in AGE_BUCKETS.iter().enumerate() {
        match limit {
            Some(limit) if days < *limit as f64 => return i,
            None => return i,
            _ => {}
        }
    }
    AGE_BUCKETS.len() - 1
}

#[derive(Clone, Debug)]
pub struct ScanConfig {
    pub roots: Vec<PathBuf>,
    pub include_os: bool,
    pub exclude_dirs: Vec<String>,
    pub exclude_names: Vec<String>,
    pub use_allocated: bool,
    pub cluster_size: u64,
    pub old_file_min_size: u64,
    pub old_file_min_days: u64,
    pub find_duplicates: bool,
    pub duplicate_min_size: u64,
    pub cross_filesystems: bool,
    pub dedup_hardlinks: bool,
    /// Worker threads; 0 picks a default from the CPU count.
    pub threads: usize,
    /// Fixed "now" for age calculations (tests); defaults to the wall clock.
    pub now_override: Option<f64>,
}

impl Default for ScanConfig {
    fn default() -> ScanConfig {
        ScanConfig {
            roots: vec![platform::default_root()],
            include_os: false,
            exclude_dirs: Vec::new(),
            exclude_names: Vec::new(),
            use_allocated: false,
            cluster_size: 4096,
            old_file_min_size: 100 * 1024 * 1024,
            old_file_min_days: 365,
            find_duplicates: false,
            duplicate_min_size: 25 * 1024 * 1024,
            cross_filesystems: false,
            dedup_hardlinks: false,
            threads: 0,
            now_override: None,
        }
    }
}

pub fn default_thread_count() -> usize {
    let cores = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cores * 2).clamp(1, 16)
}

/// One directory. `own_*` counts files directly inside; `total_*` and
/// `newest` include every descendant and are filled in by the rollup after
/// the walk.
#[derive(Clone, Debug, Default)]
pub struct DirNode {
    /// Basename, except for roots which carry their full display path.
    pub name: Box<str>,
    pub parent: u32,
    pub depth: u16,
    pub category: Option<CatId>,
    pub own_size: u64,
    pub own_files: u32,
    /// Newest file mtime directly inside; 0 when there are no files.
    pub own_newest: f64,
    /// `projects` marker bits found directly inside.
    pub markers: u16,
    pub total_size: u64,
    pub total_files: u64,
    pub total_dirs: u32,
    /// Newest file mtime anywhere in the subtree; 0 when it holds no files.
    pub newest: f64,
}

/// Live counters the CLI's progress line reads while the scan runs.
#[derive(Default)]
pub struct ScanProgress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub current: Mutex<String>,
}

/// Receives one row per file at or above the configured floor, as the walk
/// encounters it. This is how `files.csv` can be complete without the scan
/// ever holding millions of records in memory.
pub trait FileSink: Send {
    fn write_file(&mut self, path: &str, size: u64, mtime: f64, category: CatId);
}

#[derive(Debug)]
pub struct ScanResult {
    pub config: ScanConfig,
    pub started_at: f64,
    pub now: f64,
    pub elapsed: f64,
    pub cancelled: bool,
    /// Arena indexed by node id. Children always have larger ids than their
    /// parents, which is what makes the rollup a single reverse pass.
    pub nodes: Vec<DirNode>,
    pub root_ids: Vec<u32>,
    pub total_files: u64,
    pub total_dirs: u64,
    pub total_size: u64,
    pub logical_size: u64,
    /// extension -> (count, size)
    pub ext_stats: HashMap<Box<str>, (u64, u64)>,
    /// indexed by `CatId::index()` -> (count, size)
    pub cat_stats: [(u64, u64); NUM_CATEGORIES],
    /// indexed by age bucket -> (count, size)
    pub age_stats: [(u64, u64); AGE_BUCKETS.len()],
    /// Largest first.
    pub top_files: Vec<FileRec>,
    pub old_large_files: Vec<FileRec>,
    pub skipped: Vec<(String, &'static str)>,
    pub links: Vec<String>,
    pub os_managed: Vec<(String, u64)>,
    pub errors: Vec<(String, String)>,
    /// (label, (total, used, free))
    pub volumes: Vec<(String, (u64, u64, u64))>,
    pub dup_candidates: HashMap<u64, Vec<String>>,
}

impl ScanResult {
    /// An empty result; `scan` fills one in.
    pub fn new(config: ScanConfig) -> ScanResult {
        ScanResult {
            config,
            started_at: 0.0,
            now: 0.0,
            elapsed: 0.0,
            cancelled: false,
            nodes: Vec::new(),
            root_ids: Vec::new(),
            total_files: 0,
            total_dirs: 0,
            total_size: 0,
            logical_size: 0,
            ext_stats: HashMap::new(),
            cat_stats: [(0, 0); NUM_CATEGORIES],
            age_stats: [(0, 0); AGE_BUCKETS.len()],
            top_files: Vec::new(),
            old_large_files: Vec::new(),
            skipped: Vec::new(),
            links: Vec::new(),
            os_managed: Vec::new(),
            errors: Vec::new(),
            volumes: Vec::new(),
            dup_candidates: HashMap::new(),
        }
    }

    /// Full display path of a node, rebuilt from the parent chain.
    pub fn node_path(&self, id: u32) -> String {
        let mut chain = Vec::new();
        let mut cur = id;
        loop {
            chain.push(cur);
            let parent = self.nodes[cur as usize].parent;
            if parent == NO_PARENT {
                break;
            }
            cur = parent;
        }
        let mut out = String::from(&*self.nodes[*chain.last().unwrap() as usize].name);
        for &node in chain.iter().rev().skip(1) {
            platform::push_display(&mut out, &self.nodes[node as usize].name);
        }
        out
    }

    /// Display paths for every node, in id order. One forward pass, since a
    /// parent always precedes its children.
    pub fn all_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if node.parent == NO_PARENT {
                paths.push(node.name.to_string());
            } else {
                let mut path = paths[node.parent as usize].clone();
                platform::push_display(&mut path, &node.name);
                paths.push(path);
            }
        }
        paths
    }

    /// Largest first, as in the terminal report.
    pub fn sorted_old_files(&self) -> Vec<FileRec> {
        let mut files = self.old_large_files.clone();
        files.sort_by(|a, b| b.cmp(a));
        files
    }
}

/// State shared read-only (or through atomics) by every worker.
pub(crate) struct Shared<'a> {
    pub config: &'a ScanConfig,
    pub exclusions: ExclusionSet,
    pub allowed_devices: Vec<u64>,
    pub next_id: AtomicU32,
    pub pending: AtomicUsize,
    pub workers: usize,
    pub want_rows: bool,
    pub sink_min_size: u64,
    pub progress: Option<&'a ScanProgress>,
    /// `(device, file ID)` of every multiply-linked file counted so far.
    pub hardlinks: Mutex<HashSet<(u64, u128)>>,
    pub now: f64,
    pub old_min_age: f64,
}

/// Walk every configured root and return the accumulated statistics.
pub fn scan(
    config: &ScanConfig,
    sink: Option<&mut dyn FileSink>,
    sink_min_size: u64,
    progress: Option<&ScanProgress>,
) -> ScanResult {
    CANCELLED.store(false, Ordering::Relaxed);
    let started = now_epoch();
    let now = config.now_override.unwrap_or(started);
    let mut result = ScanResult::new(config.clone());
    result.started_at = started;
    result.now = now;

    // Roots are made absolute below; exclusions must be too, or a relative
    // `--exclude` would silently never match anything.
    let exclude_dirs: Vec<String> = config
        .exclude_dirs
        .iter()
        .map(|path| exclusions::absolute_display(path))
        .collect();
    let exclusions = ExclusionSet::new(config.include_os, &exclude_dirs, &config.exclude_names);
    let mut allowed_devices = platform::extra_allowed_devices();
    let mut seen_volumes = HashSet::new();
    let mut root_jobs = Vec::new();
    let mut scans_volume_root = false;

    for (index, root) in config.roots.iter().enumerate() {
        let scan_path = platform::to_scan_path(root);
        let display = platform::to_display_path(&scan_path);
        match std::fs::metadata(&scan_path) {
            Ok(md) => {
                allowed_devices.push(platform::device_of(&md));
                // Key by volume, not by scan root: two roots on the same
                // drive share one set of free-space numbers.
                let (key, label) = platform::volume_identity(root, &md);
                if seen_volumes.insert(key) {
                    match platform::disk_usage(&scan_path) {
                        Ok(usage) => result.volumes.push((label, usage)),
                        Err(err) => result
                            .errors
                            .push((display.clone(), format!("disk_usage failed: {err}"))),
                    }
                }
            }
            Err(err) => result.errors.push((display.clone(), err.to_string())),
        }
        if platform::is_volume_root(&display) {
            scans_volume_root = true;
        }
        let lower = platform::lower_slash(&display);
        let category = categories::resolve_category(None, &lower, "");
        let id = index as u32;
        result.root_ids.push(id);
        root_jobs.push(worker::DirJob {
            id,
            parent: NO_PARENT,
            depth: 0,
            path: scan_path,
            name: display.clone(),
            display,
            lower_slash: lower,
            category,
        });
    }
    if scans_volume_root {
        result.os_managed.extend(platform::collect_os_managed());
    }

    let root_count = root_jobs.len();
    let workers = if config.threads == 0 {
        default_thread_count()
    } else {
        config.threads.max(1)
    };
    let shared = Shared {
        config,
        exclusions,
        allowed_devices,
        next_id: AtomicU32::new(root_count as u32),
        pending: AtomicUsize::new(root_count),
        workers,
        want_rows: sink.is_some(),
        sink_min_size,
        progress,
        hardlinks: Mutex::new(HashSet::new()),
        now,
        old_min_age: config.old_file_min_days as f64 * SECONDS_PER_DAY,
    };

    let (job_tx, job_rx) = unbounded::<worker::Job>();
    let (event_tx, event_rx) = unbounded::<worker::Event>();
    for job in root_jobs {
        let _ = job_tx.send(worker::Job::Dir(job));
    }

    let mut states = Vec::with_capacity(workers);
    let aggregate = thread::scope(|scope| {
        let aggregator = scope.spawn(move || aggregate::run(event_rx, sink));
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let rx = job_rx.clone();
                let tx = job_tx.clone();
                let etx = event_tx.clone();
                let shared = &shared;
                scope.spawn(move || worker::run(shared, rx, tx, etx))
            })
            .collect();
        drop(job_tx);
        drop(job_rx);
        drop(event_tx);
        for handle in handles {
            states.push(handle.join().expect("scan worker panicked"));
        }
        aggregator.join().expect("scan aggregator panicked")
    });

    let aggregate::Aggregate {
        arena,
        total_files,
        total_size,
        skipped,
        links,
        os_managed,
        errors,
    } = aggregate;
    result.nodes = aggregate::into_nodes(arena);
    result.total_dirs = result.nodes.len() as u64;
    result.total_files = total_files;
    result.total_size = total_size;
    result.skipped = skipped;
    result.links = links;
    result.os_managed.extend(os_managed);
    result.errors.extend(errors);

    let mut top: Vec<FileRec> = Vec::new();
    for state in states {
        result.logical_size += state.logical_size;
        for (ext, (count, size)) in state.ext_stats {
            let slot = result.ext_stats.entry(ext).or_insert((0, 0));
            slot.0 += count;
            slot.1 += size;
        }
        for (i, (count, size)) in state.cat_stats.iter().enumerate() {
            result.cat_stats[i].0 += count;
            result.cat_stats[i].1 += size;
        }
        for (i, (count, size)) in state.age_stats.iter().enumerate() {
            result.age_stats[i].0 += count;
            result.age_stats[i].1 += size;
        }
        top.extend(state.top_heap.into_iter().map(|r| r.0));
        result.old_large_files.extend(state.old_large);
        for (size, paths) in state.dup_candidates {
            result.dup_candidates.entry(size).or_default().extend(paths);
        }
    }
    top.sort_by(|a, b| b.cmp(a));
    top.truncate(TOP_FILES_LIMIT);
    result.top_files = top;

    roll_up(&mut result.nodes);
    result.elapsed = now_epoch() - started;
    result.cancelled = CANCELLED.load(Ordering::Relaxed);
    if let Some(progress) = progress {
        if let Ok(mut current) = progress.current.lock() {
            current.clear();
        }
    }
    result
}

/// Propagate per-directory totals up to their parents. Children have larger
/// ids than parents, so walking the arena backwards finishes every child
/// before its parent is touched: one linear pass, no recursion.
pub fn roll_up(nodes: &mut [DirNode]) {
    for node in nodes.iter_mut() {
        node.total_size = node.own_size;
        node.total_files = node.own_files as u64;
        node.total_dirs = 0;
        node.newest = node.own_newest;
    }
    for id in (0..nodes.len()).rev() {
        let parent = nodes[id].parent;
        if parent == NO_PARENT {
            continue;
        }
        debug_assert!((parent as usize) < id, "child id must exceed parent id");
        let (size, files, dirs, newest) = (
            nodes[id].total_size,
            nodes[id].total_files,
            nodes[id].total_dirs,
            nodes[id].newest,
        );
        let parent = &mut nodes[parent as usize];
        parent.total_size += size;
        parent.total_files += files;
        parent.total_dirs += dirs + 1;
        parent.newest = parent.newest.max(newest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_buckets_match_python_boundaries() {
        let now = 1_000_000_000.0;
        let day = SECONDS_PER_DAY;
        assert_eq!(age_bucket(now, now), 0);
        assert_eq!(age_bucket(now - 29.9 * day, now), 0);
        assert_eq!(age_bucket(now - 30.0 * day, now), 1);
        assert_eq!(age_bucket(now - 364.0 * day, now), 2);
        assert_eq!(age_bucket(now - 365.0 * day, now), 3);
        assert_eq!(age_bucket(now - 1824.0 * day, now), 4);
        assert_eq!(age_bucket(now - 1825.0 * day, now), 5);
        assert_eq!(age_bucket(now - 99_999.0 * day, now), 5);
        assert_eq!(age_bucket(now + day, now), 0);
    }

    fn node(parent: u32, depth: u16, own_size: u64, own_files: u32, own_newest: f64) -> DirNode {
        DirNode {
            name: "n".into(),
            parent,
            depth,
            own_size,
            own_files,
            own_newest,
            ..Default::default()
        }
    }

    #[test]
    fn rollup_sums_children_into_parents() {
        let mut nodes = vec![
            node(NO_PARENT, 0, 10, 1, 5.0),
            node(0, 1, 20, 2, 50.0),
            node(0, 1, 30, 3, 0.0),
            node(2, 2, 40, 4, 20.0),
        ];
        roll_up(&mut nodes);
        assert_eq!(nodes[0].total_size, 100);
        assert_eq!(nodes[0].total_files, 10);
        assert_eq!(nodes[0].total_dirs, 3);
        assert_eq!(nodes[2].total_size, 70);
        assert_eq!(nodes[2].total_dirs, 1);
        assert_eq!(nodes[3].total_dirs, 0);
        assert_eq!(nodes[0].newest, 50.0);
        assert_eq!(
            nodes[2].newest, 20.0,
            "an empty folder takes its child's newest"
        );
        assert_eq!(nodes[3].newest, 20.0);
        // Running it twice gives the same answer.
        roll_up(&mut nodes);
        assert_eq!(nodes[0].newest, 50.0);
    }
}
