//! CSV output: the full inventory, for sorting and filtering in a
//! spreadsheet. Rows of `files.csv` are written as the walk encounters
//! them, so the listing can be complete without the scan ever holding every
//! file in memory.
//!
//! Files are UTF-8 with a BOM and CRLF line ends, which is what Excel opens
//! cleanly by double-click.

use std::fs::File;
use std::io::{self, BufWriter, Write};

use super::sink::OutputDir;
use crate::categories::CatId;
use crate::compare::{change_record, Comparison};
use crate::platform;
use crate::scanner::{worker_split_ext, FileSink, ScanResult};
use crate::util::human_size;

const FILE_COLUMNS: [&str; 9] = [
    "size_bytes",
    "size",
    "modified",
    "category",
    "confidence",
    "extension",
    "folder",
    "name",
    "full_path",
];

const FOLDER_COLUMNS: [&str; 10] = [
    "total_bytes",
    "total_size",
    "files_here_bytes",
    "files_here",
    "files_total",
    "subfolders",
    "depth",
    "category",
    "newest_modified",
    "full_path",
];

const CHANGE_COLUMNS: [&str; 6] = [
    "delta_bytes",
    "delta",
    "before_bytes",
    "after_bytes",
    "status",
    "full_path",
];

/// Minimal RFC 4180 writer: quote only when a field needs it.
pub struct CsvWriter<W: Write> {
    inner: W,
}

impl<W: Write> CsvWriter<W> {
    pub fn new(mut inner: W) -> io::Result<CsvWriter<W>> {
        inner.write_all(b"\xEF\xBB\xBF")?;
        Ok(CsvWriter { inner })
    }

    pub fn write_record<I, S>(&mut self, fields: I) -> io::Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for (i, field) in fields.into_iter().enumerate() {
            if i > 0 {
                self.inner.write_all(b",")?;
            }
            let field = field.as_ref();
            if field.contains([',', '"', '\r', '\n']) {
                self.inner.write_all(b"\"")?;
                self.inner
                    .write_all(field.replace('"', "\"\"").as_bytes())?;
                self.inner.write_all(b"\"")?;
            } else {
                self.inner.write_all(field.as_bytes())?;
            }
        }
        self.inner.write_all(b"\r\n")
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub fn timestamp(epoch: f64) -> String {
    match platform::local_datetime(epoch) {
        Some((y, m, d, h, min, _s)) => format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}"),
        None => String::new(),
    }
}

/// `os.path.split`: the folder keeps its trailing separator only when it is
/// a bare root (`/`, `C:\`).
pub fn split_path(path: &str) -> (&str, &str) {
    let Some(idx) = path.rfind([platform::PATH_SEP, '/']) else {
        return ("", path);
    };
    let head = &path[..=idx];
    let name = &path[idx + 1..];
    let trimmed = head.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || (cfg!(windows) && trimmed.len() == 2 && trimmed.ends_with(':')) {
        (head, name)
    } else {
        (trimmed, name)
    }
}

/// Streams one CSV row per file as the scan produces it.
pub struct FileListWriter {
    csv: Option<CsvWriter<BufWriter<File>>>,
    min_size: u64,
    pub rows_written: u64,
    error: Option<io::Error>,
}

impl FileListWriter {
    pub fn new(out: &OutputDir, min_size: u64) -> io::Result<FileListWriter> {
        let mut csv = CsvWriter::new(out.create_file("files.csv")?)?;
        csv.write_record(FILE_COLUMNS)?;
        Ok(FileListWriter {
            csv: Some(csv),
            min_size,
            rows_written: 0,
            error: None,
        })
    }

    /// Flush and surface the first write error, if any.
    pub fn finish(mut self) -> io::Result<u64> {
        if let Some(err) = self.error.take() {
            return Err(err);
        }
        if let Some(csv) = self.csv.take() {
            csv.finish()?;
        }
        Ok(self.rows_written)
    }
}

impl FileSink for FileListWriter {
    fn write_file(&mut self, full_path: &str, size: u64, mtime: f64, category: CatId) {
        if size < self.min_size || self.error.is_some() {
            return;
        }
        let Some(csv) = self.csv.as_mut() else { return };
        let (folder, name) = split_path(full_path);
        let outcome = csv.write_record([
            size.to_string(),
            human_size(size),
            timestamp(mtime),
            category.label().to_string(),
            category.confidence().key().to_string(),
            worker_split_ext(name).to_lowercase(),
            folder.to_string(),
            name.to_string(),
            full_path.to_string(),
        ]);
        match outcome {
            Ok(()) => self.rows_written += 1,
            Err(err) => self.error = Some(err),
        }
    }
}

/// One row per folder, with both its own bytes and its subtree total,
/// largest first.
pub fn write_folder_csv(result: &ScanResult, out: &OutputDir, min_size: u64) -> io::Result<u64> {
    let mut csv = CsvWriter::new(out.create_file("folders.csv")?)?;
    csv.write_record(FOLDER_COLUMNS)?;
    let paths = result.all_paths();
    let mut ids: Vec<u32> = (0..result.nodes.len() as u32)
        .filter(|&id| result.nodes[id as usize].total_size >= min_size)
        .collect();
    ids.sort_by(|&a, &b| {
        result.nodes[b as usize]
            .total_size
            .cmp(&result.nodes[a as usize].total_size)
            .then(a.cmp(&b))
    });
    let mut written = 0;
    for id in ids {
        let node = &result.nodes[id as usize];
        csv.write_record([
            node.total_size.to_string(),
            human_size(node.total_size),
            node.own_size.to_string(),
            node.own_files.to_string(),
            node.total_files.to_string(),
            node.total_dirs.to_string(),
            node.depth.to_string(),
            node.category.unwrap_or(CatId::Other).label().to_string(),
            if node.newest > 0.0 {
                timestamp(node.newest)
            } else {
                String::new()
            },
            paths[id as usize].clone(),
        ])?;
        written += 1;
    }
    csv.finish()?;
    Ok(written)
}

/// Every folder that changed since the `--compare` baseline by at least the
/// comparison floor, not collapsed, largest change first.
pub fn write_changes_csv(comparison: &Comparison, out: &OutputDir) -> io::Result<u64> {
    let mut csv = CsvWriter::new(out.create_file("changes.csv")?)?;
    csv.write_record(CHANGE_COLUMNS)?;
    for change in &comparison.all {
        csv.write_record(change_record(change))?;
    }
    csv.finish()?;
    Ok(comparison.all.len() as u64)
}

/// Reads CSV text back: what `CsvWriter` produces, and what a spreadsheet
/// saves. Handles a UTF-8 BOM, CRLF or LF line ends, quoted fields with `""`
/// escapes, and quoted fields spanning lines (Unix paths may hold newlines).
pub struct CsvReader<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> CsvReader<'a> {
    pub fn new(text: &'a str) -> CsvReader<'a> {
        CsvReader {
            text: text.strip_prefix('\u{feff}').unwrap_or(text),
            pos: 0,
        }
    }

    /// One record, or an error for a quote that never closes. Delimiters
    /// are ASCII, so every slice boundary is a valid UTF-8 boundary.
    fn record(&mut self) -> Result<Vec<String>, String> {
        let bytes = self.text.as_bytes();
        let mut fields = Vec::new();
        loop {
            let mut field = String::new();
            if bytes.get(self.pos) == Some(&b'"') {
                self.pos += 1;
                let mut run = self.pos;
                loop {
                    match bytes.get(self.pos) {
                        None => return Err("a quoted field never closes".to_string()),
                        Some(b'"') if bytes.get(self.pos + 1) == Some(&b'"') => {
                            field.push_str(&self.text[run..=self.pos]);
                            self.pos += 2;
                            run = self.pos;
                        }
                        Some(b'"') => {
                            field.push_str(&self.text[run..self.pos]);
                            self.pos += 1;
                            break;
                        }
                        Some(_) => self.pos += 1,
                    }
                }
            }
            // Unquoted text, or anything trailing a closing quote.
            let start = self.pos;
            while let Some(&b) = bytes.get(self.pos) {
                if matches!(b, b',' | b'\n' | b'\r') {
                    break;
                }
                self.pos += 1;
            }
            field.push_str(&self.text[start..self.pos]);
            fields.push(field);
            match bytes.get(self.pos) {
                Some(b',') => self.pos += 1,
                Some(b'\r') => {
                    self.pos += 1;
                    if bytes.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    return Ok(fields);
                }
                Some(b'\n') => {
                    self.pos += 1;
                    return Ok(fields);
                }
                _ => return Ok(fields),
            }
        }
    }
}

impl Iterator for CsvReader<'_> {
    type Item = Result<Vec<String>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.text.len() {
            return None;
        }
        Some(self.record())
    }
}

/// Paths the scan could not read, so the totals can be read honestly.
pub fn write_error_csv(result: &ScanResult, out: &OutputDir) -> io::Result<u64> {
    let mut csv = CsvWriter::new(out.create_file("unreadable.csv")?)?;
    csv.write_record(["path", "reason"])?;
    for (path, reason) in &result.errors {
        csv.write_record([path.as_str(), reason.as_str()])?;
    }
    csv.finish()?;
    Ok(result.errors.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_rfc4180() {
        let mut buf = Vec::new();
        {
            let mut csv = CsvWriter::new(&mut buf).unwrap();
            csv.write_record(["plain", "with,comma", "with \"quote\"", "line\nbreak", ""])
                .unwrap();
            csv.finish().unwrap();
        }
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(
            text,
            "\u{feff}plain,\"with,comma\",\"with \"\"quote\"\"\",\"line\nbreak\",\r\n"
        );
    }

    fn read_all(text: &str) -> Vec<Vec<String>> {
        CsvReader::new(text).collect::<Result<_, _>>().unwrap()
    }

    #[test]
    fn reader_handles_bom_line_ends_quotes_and_newlines() {
        let text =
            "\u{feff}a,b,c\r\n1,\"x, y\",\"say \"\"hi\"\"\"\r\n2,\"line\nbreak\",\n3,plain,last\n";
        assert_eq!(
            read_all(text),
            vec![
                vec!["a", "b", "c"],
                vec!["1", "x, y", "say \"hi\""],
                vec!["2", "line\nbreak", ""],
                vec!["3", "plain", "last"],
            ]
        );
        // No final line end, and a lone CR as a line end.
        assert_eq!(read_all("a,b\rc,"), vec![vec!["a", "b"], vec!["c", ""]]);
        assert!(CsvReader::new("a,\"open").next().unwrap().is_err());
    }

    #[test]
    fn reader_reads_back_what_the_writer_wrote() {
        let rows = [
            vec!["plain", "with,comma", "with \"quote\"", "line\r\nbreak", ""],
            vec!["é", "", "", "", "end"],
        ];
        let mut buf = Vec::new();
        {
            let mut csv = CsvWriter::new(&mut buf).unwrap();
            for row in &rows {
                csv.write_record(row).unwrap();
            }
            csv.finish().unwrap();
        }
        assert_eq!(read_all(&String::from_utf8(buf).unwrap()), rows);
    }

    #[test]
    fn path_splitting() {
        if cfg!(windows) {
            assert_eq!(split_path(r"C:\a\b.txt"), (r"C:\a", "b.txt"));
            assert_eq!(split_path(r"C:\b.txt"), (r"C:\", "b.txt"));
        } else {
            assert_eq!(split_path("/a/b.txt"), ("/a", "b.txt"));
            assert_eq!(split_path("/b.txt"), ("/", "b.txt"));
        }
        assert_eq!(split_path("b.txt"), ("", "b.txt"));
    }
}
