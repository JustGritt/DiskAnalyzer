//! The one place in the crate that creates files.
//!
//! Every report writer gets its file handle from an `OutputDir`, which only
//! hands out files by bare name inside the directory chosen with `--out`.
//! `clippy.toml` forbids the file-creating APIs everywhere else, and
//! `tests/readonly_audit.rs` checks the source for the same names, so a
//! careless future edit fails loudly instead of quietly gaining the ability
//! to touch anything outside the report directory.

// The allow is the single sanctioned exception to the disallowed-methods
// lint configured in clippy.toml.
#![allow(clippy::disallowed_methods)]

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

pub struct OutputDir {
    dir: PathBuf,
}

impl OutputDir {
    /// Create the report directory (and parents) if it does not exist.
    pub fn create(dir: &Path) -> io::Result<OutputDir> {
        std::fs::create_dir_all(dir)?;
        Ok(OutputDir {
            dir: dir.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Create (truncating) a report file by bare name inside the directory.
    pub fn create_file(&self, name: &str) -> io::Result<BufWriter<File>> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("report file name must be a bare name: {name:?}"),
            ));
        }
        let file = File::create(self.dir.join(name))?;
        Ok(BufWriter::with_capacity(1 << 16, file))
    }
}
