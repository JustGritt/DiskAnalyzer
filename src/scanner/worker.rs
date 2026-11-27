//! One worker: pull a directory, enumerate it, push its subdirectories back
//! onto the queue, and fold its files into thread-local statistics.

use std::cmp::{Ordering as CmpOrdering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::categories::{self, CatId, NUM_CATEGORIES};
use crate::exclusions;
use crate::platform;
use crate::projects;

use super::{age_bucket, Shared, AGE_BUCKETS, CANCELLED, TOP_FILES_LIMIT};

/// Files between progress-path updates.
const REPORT_EVERY: u32 = 4000;

/// A file that made it into one of the rankings.
#[derive(Clone, Debug)]
pub struct FileRec {
    pub size: u64,
    /// Monotonic per-worker tie-breaker so ordering never touches the path.
    pub seq: u64,
    pub path: Box<str>,
    pub mtime: f64,
}

impl PartialEq for FileRec {
    fn eq(&self, other: &FileRec) -> bool {
        self.size == other.size && self.seq == other.seq
    }
}
impl Eq for FileRec {}
impl PartialOrd for FileRec {
    fn partial_cmp(&self, other: &FileRec) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for FileRec {
    fn cmp(&self, other: &FileRec) -> CmpOrdering {
        (self.size, self.seq).cmp(&(other.size, other.seq))
    }
}

pub struct DirJob {
    pub id: u32,
    pub parent: u32,
    pub depth: u16,
    /// The path handed to the OS (extended-length on Windows).
    pub path: PathBuf,
    /// Basename, or the display path for a root.
    pub name: String,
    pub display: String,
    /// Lowercased, slash-separated form used by the category rules.
    pub lower_slash: String,
    pub category: Option<CatId>,
}

pub enum Job {
    Dir(DirJob),
    Shutdown,
}

pub enum Event {
    Dir {
        id: u32,
        parent: u32,
        depth: u16,
        name: Box<str>,
        category: Option<CatId>,
        own_size: u64,
        own_files: u32,
        /// Newest mtime among the files counted here; 0 when there are none.
        own_newest: f64,
        /// `projects` marker bits of the entries directly inside.
        markers: u16,
    },
    FileRow {
        path: String,
        size: u64,
        mtime: f64,
        category: CatId,
    },
    Skipped(String, &'static str),
    Link(String),
    OsManaged(String, u64),
    Error(String, String),
}

/// Everything a worker accumulates without synchronization.
pub struct WorkerState {
    pub ext_stats: HashMap<Box<str>, (u64, u64)>,
    pub cat_stats: [(u64, u64); NUM_CATEGORIES],
    pub age_stats: [(u64, u64); AGE_BUCKETS.len()],
    pub top_heap: BinaryHeap<Reverse<FileRec>>,
    pub old_large: Vec<FileRec>,
    pub dup_candidates: HashMap<u64, Vec<String>>,
    pub logical_size: u64,
    seq: u64,
    since_report: u32,
}

impl WorkerState {
    fn new() -> WorkerState {
        WorkerState {
            ext_stats: HashMap::new(),
            cat_stats: [(0, 0); NUM_CATEGORIES],
            age_stats: [(0, 0); AGE_BUCKETS.len()],
            top_heap: BinaryHeap::with_capacity(TOP_FILES_LIMIT + 1),
            old_large: Vec::new(),
            dup_candidates: HashMap::new(),
            logical_size: 0,
            seq: 0,
            since_report: 0,
        }
    }
}

pub fn run(
    shared: &Shared,
    jobs: Receiver<Job>,
    job_tx: Sender<Job>,
    events: Sender<Event>,
) -> WorkerState {
    let mut state = WorkerState::new();
    loop {
        if CANCELLED.load(Ordering::Relaxed) {
            break;
        }
        let job = match jobs.recv_timeout(Duration::from_millis(50)) {
            Ok(Job::Dir(job)) => job,
            Ok(Job::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => continue,
        };
        process_dir(shared, job, &mut state, &job_tx, &events);
        // The last directory to finish wakes every worker up to exit.
        if shared.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            for _ in 0..shared.workers {
                let _ = job_tx.send(Job::Shutdown);
            }
        }
    }
    state
}

/// `os.path.splitext` semantics: the extension is the last dotted suffix,
/// unless every character before the dot is itself a dot.
pub fn split_ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if name[..dot].bytes().any(|b| b != b'.') => &name[dot..],
        _ => "",
    }
}

/// Extra attempts after a call fails with `EINTR`.
const INTERRUPTED_RETRIES: u32 = 3;

/// Repeat a filesystem call that was interrupted by a signal. macOS has been
/// seen to report "Interrupted system call" for app-container folders; the
/// call itself is harmless to repeat, and a real refusal still surfaces.
fn retry_interrupted<T>(mut call: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut retries = 0;
    loop {
        match call() {
            Err(err)
                if err.kind() == io::ErrorKind::Interrupted && retries < INTERRUPTED_RETRIES =>
            {
                retries += 1;
            }
            outcome => return outcome,
        }
    }
}

fn mtime_of(md: &std::fs::Metadata) -> f64 {
    match md.modified() {
        Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        },
        Err(_) => 0.0,
    }
}

fn process_dir(
    shared: &Shared,
    job: DirJob,
    state: &mut WorkerState,
    job_tx: &Sender<Job>,
    events: &Sender<Event>,
) {
    let config = shared.config;
    let at_volume_root = platform::is_volume_root(&job.display);
    let mut own_size: u64 = 0;
    let mut own_files: u32 = 0;
    let mut own_newest: f64 = 0.0;
    let mut markers: u16 = 0;

    match retry_interrupted(|| std::fs::read_dir(&job.path)) {
        Err(err) => {
            let _ = events.send(Event::Error(job.display.clone(), err.to_string()));
        }
        Ok(entries) => {
            for entry in entries {
                if CANCELLED.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        // A directory can become unreadable mid-enumeration.
                        let _ = events.send(Event::Error(job.display.clone(), err.to_string()));
                        break;
                    }
                };
                let name_os = entry.file_name();
                let name = name_os.to_string_lossy();
                let mut display_cache: Option<String> = None;
                let md = match retry_interrupted(|| entry.metadata()) {
                    Ok(md) => md,
                    Err(err) => {
                        let display = display_of(&mut display_cache, &job.display, &name);
                        let _ = events.send(Event::Error(display.to_string(), err.to_string()));
                        continue;
                    }
                };

                if platform::is_link(&md) {
                    // Junction, symlink or mount point: record and move on.
                    let display = display_of(&mut display_cache, &job.display, &name);
                    let _ = events.send(Event::Link(display.to_string()));
                    continue;
                }

                // Before any exclusion: a skipped `.git` still marks a project.
                markers |= projects::marker_bits(&name, md.is_dir());

                if md.is_dir() {
                    let display = display_cache
                        .take()
                        .unwrap_or_else(|| platform::join_display(&job.display, &name));
                    if let Some(reason) = shared.exclusions.reason_for(&display, &name) {
                        let _ = events.send(Event::Skipped(display, reason));
                        continue;
                    }
                    if !config.cross_filesystems
                        && !shared.allowed_devices.contains(&platform::device_of(&md))
                    {
                        let _ = events.send(Event::Skipped(display, exclusions::OTHER_FILESYSTEM));
                        continue;
                    }
                    let name_lower = name.to_lowercase();
                    let lower_slash = platform::join_lower_slash(&job.lower_slash, &name_lower);
                    let category =
                        categories::resolve_category(job.category, &lower_slash, &name_lower);
                    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
                    shared.pending.fetch_add(1, Ordering::AcqRel);
                    let _ = job_tx.send(Job::Dir(DirJob {
                        id,
                        parent: job.id,
                        depth: job.depth.saturating_add(1),
                        path: entry.path(),
                        name: name.into_owned(),
                        display,
                        lower_slash,
                        category,
                    }));
                    continue;
                }

                // ---- regular file ------------------------------------
                let logical = md.len();
                if shared.exclusions.is_os_managed_file(&name, at_volume_root) {
                    let display = display_of(&mut display_cache, &job.display, &name);
                    let _ = events.send(Event::OsManaged(display.to_string(), logical));
                    continue;
                }
                if config.dedup_hardlinks {
                    if let Some(key) = platform::hardlink_key(&entry, &md) {
                        let first_seen = shared
                            .hardlinks
                            .lock()
                            .map(|mut set| set.insert(key))
                            .unwrap_or(true);
                        if !first_seen {
                            continue;
                        }
                    }
                }

                let size = if config.use_allocated {
                    platform::allocated_size(&md, config.cluster_size)
                } else {
                    logical
                };
                own_size += size;
                own_files += 1;
                state.logical_size += logical;

                let ext = split_ext(&name);
                let ext_lower: Box<str> = if ext.is_empty() {
                    "(no extension)".into()
                } else {
                    ext.to_lowercase().into()
                };
                let category = job
                    .category
                    .or_else(|| categories::match_extension(&ext_lower, size))
                    .unwrap_or(CatId::Other);
                let slot = state.ext_stats.entry(ext_lower).or_insert((0, 0));
                slot.0 += 1;
                slot.1 += size;
                let slot = &mut state.cat_stats[category.index()];
                slot.0 += 1;
                slot.1 += size;

                let mtime = mtime_of(&md);
                own_newest = own_newest.max(mtime);
                let slot = &mut state.age_stats[age_bucket(mtime, shared.now)];
                slot.0 += 1;
                slot.1 += size;

                state.seq += 1;
                let seq = state.seq;

                if shared.want_rows && size >= shared.sink_min_size {
                    let display = display_of(&mut display_cache, &job.display, &name);
                    let _ = events.send(Event::FileRow {
                        path: display.to_string(),
                        size,
                        mtime,
                        category,
                    });
                }

                let wants_top = state.top_heap.len() < TOP_FILES_LIMIT
                    || state.top_heap.peek().is_some_and(|min| size > min.0.size);
                if wants_top {
                    let display = display_of(&mut display_cache, &job.display, &name);
                    let rec = FileRec {
                        size,
                        seq,
                        path: display.into(),
                        mtime,
                    };
                    if state.top_heap.len() < TOP_FILES_LIMIT {
                        state.top_heap.push(Reverse(rec));
                    } else {
                        state.top_heap.pop();
                        state.top_heap.push(Reverse(rec));
                    }
                }

                if size >= config.old_file_min_size && (shared.now - mtime) >= shared.old_min_age {
                    let display = display_of(&mut display_cache, &job.display, &name);
                    state.old_large.push(FileRec {
                        size,
                        seq,
                        path: display.into(),
                        mtime,
                    });
                }

                if config.find_duplicates && size >= config.duplicate_min_size {
                    let display = display_of(&mut display_cache, &job.display, &name);
                    state
                        .dup_candidates
                        .entry(size)
                        .or_default()
                        .push(display.to_string());
                }

                state.since_report += 1;
            }
        }
    }

    if let Some(progress) = shared.progress {
        progress
            .files
            .fetch_add(own_files as u64, Ordering::Relaxed);
        progress.bytes.fetch_add(own_size, Ordering::Relaxed);
        if state.since_report >= REPORT_EVERY {
            state.since_report = 0;
            if let Ok(mut current) = progress.current.lock() {
                current.clear();
                current.push_str(&job.display);
            }
        }
    }

    let _ = events.send(Event::Dir {
        id: job.id,
        parent: job.parent,
        depth: job.depth,
        name: job.name.into_boxed_str(),
        category: job.category,
        own_size,
        own_files,
        own_newest,
        markers,
    });
}

/// Build the child's display path at most once per entry, and only for
/// entries that need it.
fn display_of<'a>(cache: &'a mut Option<String>, parent: &str, name: &str) -> &'a str {
    cache.get_or_insert_with(|| platform::join_display(parent, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_splitting_matches_python() {
        assert_eq!(split_ext("archive.tar.gz"), ".gz");
        assert_eq!(split_ext("README"), "");
        assert_eq!(split_ext(".bashrc"), "");
        assert_eq!(split_ext("..hidden"), "");
        assert_eq!(split_ext(".profile.bak"), ".bak");
        assert_eq!(split_ext("trailing."), ".");
        assert_eq!(split_ext("Movie.MKV"), ".MKV");
    }

    #[test]
    fn interrupted_calls_are_retried_a_bounded_number_of_times() {
        let mut calls = 0;
        let outcome = retry_interrupted(|| {
            calls += 1;
            if calls < 3 {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(outcome.unwrap(), 3);

        let mut calls = 0;
        let outcome: io::Result<()> = retry_interrupted(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::Interrupted))
        });
        assert!(outcome.is_err());
        assert_eq!(calls, 1 + INTERRUPTED_RETRIES);

        let mut calls = 0;
        let outcome: io::Result<()> = retry_interrupted(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert!(outcome.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn file_records_order_by_size_then_seq() {
        let a = FileRec {
            size: 5,
            seq: 1,
            path: "a".into(),
            mtime: 0.0,
        };
        let b = FileRec {
            size: 5,
            seq: 2,
            path: "b".into(),
            mtime: 0.0,
        };
        let c = FileRec {
            size: 9,
            seq: 0,
            path: "c".into(),
            mtime: 0.0,
        };
        let mut v = [a.clone(), b.clone(), c.clone()];
        v.sort_by(|x, y| y.cmp(x));
        assert_eq!(v[0].path, c.path);
        assert_eq!(v[1].path, b.path);
        assert_eq!(v[2].path, a.path);
    }
}
