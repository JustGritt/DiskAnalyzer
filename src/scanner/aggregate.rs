//! The aggregator thread: the one owner of the directory arena and of the
//! streaming `files.csv` writer. Workers send it events; it never blocks
//! them back.

use crossbeam_channel::Receiver;

use super::worker::Event;
use super::{DirNode, FileSink, NO_PARENT};

pub struct Aggregate {
    pub arena: Vec<Option<DirNode>>,
    pub total_files: u64,
    pub total_size: u64,
    pub skipped: Vec<(String, &'static str)>,
    pub links: Vec<String>,
    pub os_managed: Vec<(String, u64)>,
    pub errors: Vec<(String, String)>,
}

/// The arena as a dense vector. A slot is only empty when the scan was
/// cancelled before that directory was reached.
pub fn into_nodes(arena: Vec<Option<DirNode>>) -> Vec<DirNode> {
    arena
        .into_iter()
        .map(|slot| {
            slot.unwrap_or(DirNode {
                name: "".into(),
                parent: NO_PARENT,
                depth: 0,
                category: None,
                own_size: 0,
                own_files: 0,
                total_size: 0,
                total_files: 0,
                total_dirs: 0,
            })
        })
        .collect()
}

pub fn run(events: Receiver<Event>, mut sink: Option<&mut dyn FileSink>) -> Aggregate {
    let mut agg = Aggregate {
        arena: Vec::new(),
        total_files: 0,
        total_size: 0,
        skipped: Vec::new(),
        links: Vec::new(),
        os_managed: Vec::new(),
        errors: Vec::new(),
    };
    for event in events {
        match event {
            Event::Dir {
                id,
                parent,
                depth,
                name,
                category,
                own_size,
                own_files,
            } => {
                let index = id as usize;
                if agg.arena.len() <= index {
                    agg.arena.resize_with(index + 1, || None);
                }
                agg.arena[index] = Some(DirNode {
                    name,
                    parent,
                    depth,
                    category,
                    own_size,
                    own_files,
                    total_size: 0,
                    total_files: 0,
                    total_dirs: 0,
                });
                agg.total_files += own_files as u64;
                agg.total_size += own_size;
            }
            Event::FileRow {
                path,
                size,
                mtime,
                category,
            } => {
                if let Some(sink) = sink.as_deref_mut() {
                    sink.write_file(&path, size, mtime, category);
                }
            }
            Event::Skipped(path, reason) => agg.skipped.push((path, reason)),
            Event::Link(path) => agg.links.push(path),
            Event::OsManaged(path, size) => agg.os_managed.push((path, size)),
            Event::Error(path, reason) => agg.errors.push((path, reason)),
        }
    }
    agg
}
