//! The read-only guarantee, checked against the source: no module other
//! than `reports/sink.rs` may name a filesystem API that creates, writes,
//! removes, renames or otherwise alters anything. This is the successor of
//! the Python tool's `safety.py` self-audit; it runs where the source exists.

use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    "remove_file",
    "remove_dir",
    "fs::rename",
    "fs::copy",
    "hard_link",
    "soft_link",
    // `fs::symlink`, `symlink_file`, `symlink_dir` - but not the read-only
    // `is_symlink()` query the scanner uses on every entry.
    "fs::symlink",
    "set_permissions",
    "fs::write(",
    "create_dir",
    "File::create",
    "OpenOptions",
    "set_modified",
    "set_times",
    "set_len",
    // Win32, for the calls that bypass std (the platform layer opens files
    // for FILE_READ_ATTRIBUTES with OPEN_EXISTING): write access, any
    // disposition that can create or truncate, and the mutating APIs.
    "GENERIC_WRITE",
    "GENERIC_ALL",
    "FILE_WRITE_",
    "FILE_APPEND_DATA",
    "CREATE_ALWAYS",
    "CREATE_NEW",
    "OPEN_ALWAYS",
    "TRUNCATE_EXISTING",
    "WriteFile",
    "DeleteFile",
    "RemoveDirectory",
    "MoveFile",
    "CopyFile",
    "ReplaceFile",
    "SetFileInformationByHandle",
    "SetFileAttributes",
    "SetFileTime",
    "SetEndOfFile",
    "CreateDirectory",
    "CreateHardLink",
    "CreateSymbolicLink",
];

const WRITER_MODULE: &str = "sink.rs";

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_destructive_filesystem_calls_outside_the_report_sink() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        files.len() > 10,
        "expected to find the crate sources under {}",
        src.display()
    );

    let mut violations = Vec::new();
    for path in &files {
        if path.file_name().is_some_and(|name| name == WRITER_MODULE) {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    violations.push(format!(
                        "{}:{}: {needle}\n    {}",
                        path.strip_prefix(&src).unwrap().display(),
                        number + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "destructive filesystem calls found outside {WRITER_MODULE}:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_sink_is_the_only_file_creator_and_is_lint_exempted() {
    let sink = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/reports")
        .join(WRITER_MODULE);
    let text = std::fs::read_to_string(&sink).unwrap();
    assert!(text.contains("#![allow(clippy::disallowed_methods)]"));
    let clippy =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("clippy.toml")).unwrap();
    for api in [
        "remove_file",
        "remove_dir_all",
        "rename",
        "File::create",
        "OpenOptions::write",
    ] {
        assert!(clippy.contains(api), "clippy.toml should forbid {api}");
    }
}
