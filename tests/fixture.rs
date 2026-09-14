//! End-to-end checks on a fixture tree built in a temp directory.
//!
//! The fixture builder is the one place tests write to disk, which is why
//! this file (and only this file) opts out of the read-only lint.
#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use diskanalyzer::categories::CatId;
use diskanalyzer::insights::build_report;
use diskanalyzer::reports::csv::{write_folder_csv, FileListWriter};
use diskanalyzer::reports::html;
use diskanalyzer::reports::sink::OutputDir;
use diskanalyzer::reports::terminal;
use diskanalyzer::scanner::{scan, FileSink, ScanConfig, ScanResult, SECONDS_PER_DAY};
use diskanalyzer::util::now_epoch;

const DUP_BYTES: usize = 30_000;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The `chmod 000` directory, restored on drop so the temp tree can be
    /// removed. Only Unix can make a directory unreadable this way.
    #[cfg(unix)]
    unreadable: Option<PathBuf>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Restore permissions so the temp dir can be removed.
        #[cfg(unix)]
        if let Some(path) = &self.unreadable {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        }
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
}

/// A temp directory under Cargo's target dir. The system temp folder would
/// classify the whole fixture on Windows, where `AppData\Local\Temp` matches
/// the Temp rule.
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

fn build_fixture() -> Fixture {
    let dir = temp_dir();
    let root = dir.path().join("root");
    fs::create_dir_all(root.join("a/node_modules")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::create_dir_all(root.join("skipme")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();

    // Same size, different contents: candidates for duplicate detection that
    // must not survive the head/tail comparison.
    for i in 0..10u8 {
        write(&root.join(format!("a/file{i}.txt")), &vec![b'a' + i; 1000]);
    }
    write(&root.join("a/node_modules/pkg.js"), &vec![b'j'; 5000]);
    write(&root.join("b/Archive.TAR.GZ"), &vec![b'z'; 20_000]);
    write(&root.join("b/README"), b"hello");
    write(&root.join("skipme/ignored.bin"), &vec![b'x'; 777]);
    write(&root.join("dup1.bin"), &vec![b'd'; DUP_BYTES]);
    write(&root.join("dup2.bin"), &vec![b'd'; DUP_BYTES]);
    write(
        &root.join("same-size-different.bin"),
        &vec![b'e'; DUP_BYTES],
    );

    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("a"), root.join("link-to-a")).unwrap();

    #[cfg(unix)]
    let mut unreadable = None;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let is_root = unsafe { libc_geteuid() } == 0;
        if !is_root {
            let path = root.join("unreadable");
            fs::create_dir_all(&path).unwrap();
            write(&path.join("secret.bin"), &[0u8; 100]);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            unreadable = Some(path);
        }
    }

    Fixture {
        _dir: dir,
        root,
        #[cfg(unix)]
        unreadable,
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

/// Counted bytes: a/ (10 KB) + node_modules (5 KB) + b/ (20 005) + 3 x 30 000.
const EXPECTED_SIZE: u64 = 10_000 + 5_000 + 20_005 + 3 * DUP_BYTES as u64;
const EXPECTED_FILES: u64 = 10 + 1 + 2 + 3;

fn config_for(fixture: &Fixture, threads: usize) -> ScanConfig {
    ScanConfig {
        roots: vec![fixture.root.clone()],
        exclude_names: vec!["skipme".to_string()],
        old_file_min_size: 1,
        find_duplicates: true,
        duplicate_min_size: 1000,
        threads,
        // Every file is "old": pretend the scan runs 400 days from now.
        now_override: Some(now_epoch() + 400.0 * SECONDS_PER_DAY),
        ..ScanConfig::default()
    }
}

fn node_table(result: &ScanResult) -> BTreeMap<String, (u64, u64, u32)> {
    let paths = result.all_paths();
    result
        .nodes
        .iter()
        .zip(paths)
        .map(|(node, path)| (path, (node.total_size, node.total_files, node.total_dirs)))
        .collect()
}

#[test]
fn totals_categories_links_skips_and_errors() {
    let fixture = build_fixture();
    let result = scan(&config_for(&fixture, 1), None, 0, None);

    assert!(!result.cancelled);
    assert_eq!(result.total_files, EXPECTED_FILES);
    assert_eq!(result.total_size, EXPECTED_SIZE);
    assert_eq!(result.logical_size, EXPECTED_SIZE);

    let root = &result.nodes[result.root_ids[0] as usize];
    assert_eq!(root.total_size, EXPECTED_SIZE);
    assert_eq!(root.total_files, EXPECTED_FILES);

    let table = node_table(&result);
    let a = &table[&fixture.root.join("a").display().to_string()];
    assert_eq!(*a, (15_000, 11, 1));
    let nm = &table[&fixture
        .root
        .join("a")
        .join("node_modules")
        .display()
        .to_string()];
    assert_eq!(*nm, (5_000, 1, 0));
    assert!(table.contains_key(&fixture.root.join("empty").display().to_string()));
    assert!(!table.contains_key(&fixture.root.join("skipme").display().to_string()));

    assert_eq!(result.cat_stats[CatId::DevDeps.index()], (1, 5_000));
    assert_eq!(result.cat_stats[CatId::Other.index()].0, EXPECTED_FILES - 1);
    assert_eq!(result.ext_stats["(no extension)"], (1, 5));
    assert_eq!(result.ext_stats[".gz"], (1, 20_000));
    assert_eq!(result.ext_stats[".txt"], (10, 10_000));

    assert_eq!(result.skipped.len(), 1);
    assert_eq!(result.skipped[0].1, "user-excluded name");
    assert!(result.skipped[0].0.ends_with("skipme"));

    // 400 days -> "1-2 years" bucket, and every file is old and large enough.
    assert_eq!(result.age_stats[3], (EXPECTED_FILES, EXPECTED_SIZE));
    assert_eq!(result.old_large_files.len(), EXPECTED_FILES as usize);
    assert_eq!(result.top_files.len(), EXPECTED_FILES as usize);
    assert_eq!(result.top_files[0].size, DUP_BYTES as u64);

    #[cfg(unix)]
    {
        assert_eq!(result.links.len(), 1);
        assert!(result.links[0].ends_with("link-to-a"));
        if fixture.unreadable.is_some() {
            assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
            assert!(result.errors[0].0.ends_with("unreadable"));
        }
    }
    assert_eq!(result.volumes.len(), 1);
}

#[test]
fn parallel_scan_matches_serial_scan() {
    let fixture = build_fixture();
    let serial = scan(&config_for(&fixture, 1), None, 0, None);
    for threads in [2, 4, 8] {
        let parallel = scan(&config_for(&fixture, threads), None, 0, None);
        assert_eq!(parallel.total_files, serial.total_files);
        assert_eq!(parallel.total_size, serial.total_size);
        assert_eq!(parallel.total_dirs, serial.total_dirs);
        assert_eq!(node_table(&parallel), node_table(&serial));
        assert_eq!(parallel.ext_stats, serial.ext_stats);
        assert_eq!(parallel.cat_stats, serial.cat_stats);
        assert_eq!(parallel.age_stats, serial.age_stats);
        let mut a: Vec<(u64, String)> = parallel
            .top_files
            .iter()
            .map(|r| (r.size, r.path.to_string()))
            .collect();
        let mut b: Vec<(u64, String)> = serial
            .top_files
            .iter()
            .map(|r| (r.size, r.path.to_string()))
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }
}

#[test]
fn duplicates_need_matching_content_not_just_size() {
    let fixture = build_fixture();
    let mut result = scan(&config_for(&fixture, 4), None, 0, None);
    let report = build_report(&mut result, 25, 30, true);
    assert_eq!(report.duplicates.len(), 1);
    let group = &report.duplicates[0];
    assert_eq!(group.size, DUP_BYTES as u64);
    assert_eq!(group.paths.len(), 2);
    assert!(group
        .paths
        .iter()
        .all(|p| p.ends_with("dup1.bin") || p.ends_with("dup2.bin")));
    assert_eq!(group.wasted(), DUP_BYTES as u64);
    assert!(!report.duplicates_truncated);
    assert_eq!(report.reclaimable.duplicates, DUP_BYTES as u64);
    assert_eq!(report.reclaimable.safe, 5_000);
    assert_eq!(report.reclaimable.info, EXPECTED_SIZE - 5_000);
}

#[test]
fn reports_render_and_csvs_stream() {
    let fixture = build_fixture();
    let out_dir = temp_dir();
    let output = OutputDir::create(&out_dir.path().join("nested/reports")).unwrap();

    let mut writer = FileListWriter::new(&output, 0).unwrap();
    let mut result = {
        let sink: &mut dyn FileSink = &mut writer;
        scan(&config_for(&fixture, 3), Some(sink), 0, None)
    };
    assert_eq!(writer.finish().unwrap(), EXPECTED_FILES);

    let files_csv = fs::read_to_string(output.join("files.csv")).unwrap();
    assert!(files_csv.starts_with(
        "\u{feff}size_bytes,size,modified,category,confidence,extension,folder,name,full_path\r\n"
    ));
    assert_eq!(files_csv.lines().count() as u64, EXPECTED_FILES + 1);
    assert!(files_csv.contains("5000,4.9 KB,"));
    assert!(files_csv.contains(",Project dependencies,safe,.js,"));
    assert!(files_csv.contains(",.gz,"));

    let folder_rows = write_folder_csv(&result, &output, 0).unwrap();
    assert_eq!(folder_rows, result.nodes.len() as u64);
    let folders_csv = fs::read_to_string(output.join("folders.csv")).unwrap();
    let first_row = folders_csv.lines().nth(1).unwrap();
    assert!(
        first_row.starts_with(&format!("{EXPECTED_SIZE},")),
        "{first_row}"
    );

    let report = build_report(&mut result, 25, 30, true);
    let text = terminal::render(&report, &result, false, 100);
    assert!(text.contains("DISK ANALYZER"));
    assert!(text.contains("-- WHERE THE SPACE IS"));
    assert!(text.contains("-- LIKELY DUPLICATES"));
    assert!(text.contains("-- BIG AND UNTOUCHED"));
    assert!(text.contains("[dev-deps]"));
    assert!(text.contains("Nothing was modified, moved or deleted."));
    // Only the user's own --exclude-name was skipped, and this is a folder
    // scan: neither the OS hint nor a volume coverage share applies.
    assert!(text.contains("Skipped  1 paths (user-excluded name)"));
    assert!(!text.contains("--include-os"));
    assert!(!text.contains("of used space"));
    assert!(text.contains("not modified for 1 year or more"));
    let colored = terminal::render(&report, &result, true, 60);
    assert!(colored.contains("\x1b[1m"));

    let page = html::render(&report, &result);
    assert!(page.starts_with("<!doctype html>"));
    assert!(
        page.contains(r#""tree":null"#),
        "fixture is far below the 8 MB treemap floor"
    );
    assert!(page.contains(r#""duplicates":[{"size":30000"#));
    assert!(page
        .contains(r#""skippedByReason":[{"reason":"user-excluded name","count":1,"os":false}]"#));
    assert!(page.contains(r#""oldMinDays":365"#));
    assert!(!page.contains("__PAYLOAD__"));
    html::write_html(&report, &result, &output).unwrap();
    assert!(output.join("report.html").is_file());
}

#[test]
fn binary_runs_end_to_end() {
    let fixture = build_fixture();
    let out_dir = temp_dir();
    let reports = out_dir.path().join("run");
    let exe = env!("CARGO_BIN_EXE_diskanalyzer");

    // Run from inside the fixture so `--exclude ./empty` is relative, and
    // scan `.` so the root is relative too.
    let output = Command::new(exe)
        .current_dir(&fixture.root)
        .args([
            "--no-color",
            "--quiet",
            "--threads",
            "2",
            "--csv-min-size",
            "0",
            "--folder-min-size",
            "0",
            "--duplicates",
        ])
        .args([
            "--dup-min-size",
            "1000",
            "--exclude-name",
            "skipme",
            "--exclude-name",
            "no-such-name",
            "--exclude",
            "./empty",
            "--out",
        ])
        .arg(&reports)
        .arg(".")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("DISK ANALYZER"));
    assert!(stdout.contains("Reports written to"));
    assert!(!stdout.contains("\x1b["));
    assert!(reports.join("files.csv").is_file());
    assert!(reports.join("folders.csv").is_file());
    assert!(reports.join("report.html").is_file());

    // The relative exclusion took effect, and only the unused name is noted.
    let folders_csv = fs::read_to_string(reports.join("folders.csv")).unwrap();
    assert!(folders_csv.contains(&*fixture.root.join("b").display().to_string()));
    assert!(!folders_csv.contains(&*fixture.root.join("empty").display().to_string()));
    assert!(stdout.contains("Skipped  2 paths (user-excluded, user-excluded name)"));
    assert!(
        stderr.contains("--exclude-name no-such-name matched no folder"),
        "{stderr}"
    );
    assert!(!stderr.contains("--exclude ./empty"), "{stderr}");

    let missing = Command::new(exe)
        .arg(fixture.root.join("does-not-exist"))
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Not a folder"));

    let verify = Command::new(exe)
        .arg("--verify-read-only")
        .output()
        .unwrap();
    assert_eq!(verify.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&verify.stdout).contains("Read-only guarantee"));
}
