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
use diskanalyzer::insights::{build_report, reclaim_folders, ReportOptions};
use diskanalyzer::platform;
use diskanalyzer::projects;
use diskanalyzer::reports::csv::{write_folder_csv, FileListWriter};
use diskanalyzer::reports::html;
use diskanalyzer::reports::sink::OutputDir;
use diskanalyzer::reports::terminal;
use diskanalyzer::scanner::{scan, FileSink, ScanConfig, ScanResult, SECONDS_PER_DAY};
use diskanalyzer::util::{now_epoch, Json};

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

fn with_duplicates() -> ReportOptions {
    ReportOptions {
        with_duplicates: true,
        ..ReportOptions::default()
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
    let report = build_report(&mut result, &with_duplicates());
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

    let report = build_report(&mut result, &with_duplicates());
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

/// Set a file's modification time. Test-only: the crate itself can never
/// do this.
fn backdate(path: &Path, days: f64) {
    let when =
        std::time::SystemTime::now() - std::time::Duration::from_secs_f64(days * SECONDS_PER_DAY);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn reclaimable_folders_are_judged_by_their_project() {
    let dir = temp_dir();
    let root = dir.path().join("work");
    let proj = root.join("proj");
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::create_dir_all(proj.join("target").join("debug")).unwrap();
    write(&proj.join("Cargo.toml"), b"[package]\n");
    write(&proj.join("src").join("main.rs"), b"fn main() {}\n");
    write(
        &proj.join("target").join("debug").join("big.bin"),
        &[7u8; 4096],
    );
    backdate(&proj.join("Cargo.toml"), 200.0);
    backdate(&proj.join("src").join("main.rs"), 200.0);

    let config = ScanConfig {
        roots: vec![root.clone()],
        threads: 2,
        ..ScanConfig::default()
    };
    let result = scan(&config, None, 0, None);
    let found = reclaim_folders(&result, 10, 90, 0, None);
    assert_eq!(found.folders.len(), 1, "{found:?}");
    let folder = &found.folders[0];
    assert_eq!(folder.category, CatId::DevBuild);
    assert!(result.node_path(folder.id).ends_with("target"));
    let project = folder.project.expect("proj is recognized by Cargo.toml");
    assert_eq!(&*result.nodes[project as usize].name, "proj");
    assert!(result.nodes[project as usize].markers & projects::RUST != 0);
    assert!(folder.stale, "source untouched for 200 days");
    assert!(
        result.now - folder.contents_newest < SECONDS_PER_DAY,
        "build output is fresh"
    );

    // A 200-day-old project is not idle under a 365-day threshold.
    assert!(!reclaim_folders(&result, 10, 365, 0, None).folders[0].stale);

    // Both reports show it. The real floor is 64 MB, so substitute the list
    // found without one.
    let mut result = result;
    let mut report = build_report(&mut result, &ReportOptions::default());
    report.reclaim_folders = found;
    let text = terminal::render(&report, &result, false, 120);
    assert!(text.contains("-- RECLAIMABLE FOLDERS"), "{text}");
    assert!(text.contains("proj (rust)"), "{text}");
    assert!(text.contains("idle for 90 days or more"), "{text}");
    let page = html::render(&report, &result);
    assert!(page.contains(r#""project":"proj","#));
    assert!(page.contains(r#""projectKinds":["rust"]"#));
    assert!(page.contains(r#""stale":true"#));

    // folders.csv carries the newest mtime per folder.
    let out_dir = temp_dir();
    let output = OutputDir::create(out_dir.path()).unwrap();
    write_folder_csv(&result, &output, 0).unwrap();
    let csv = fs::read_to_string(output.join("folders.csv")).unwrap();
    assert!(csv
        .lines()
        .next()
        .unwrap()
        .ends_with(",newest_modified,full_path"));
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

    let json = fs::read_to_string(reports.join("report.json")).unwrap();
    let doc = Json::parse(&json).unwrap();
    assert_eq!(doc.get("schemaVersion").and_then(Json::as_u64), Some(1));
    assert_eq!(
        doc.get("schema").and_then(Json::as_str),
        Some("diskanalyzer.report")
    );
    let config = doc.get("config").unwrap();
    let roots: Vec<String> = config
        .get("roots")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .filter_map(Json::as_str)
        .map(platform::fold_case)
        .collect();
    assert_eq!(
        roots,
        vec![platform::fold_case(&fixture.root.display().to_string())]
    );
    assert_eq!(config.get("folderMinSize").and_then(Json::as_u64), Some(0));
    assert!(doc.get("generatedEpoch").and_then(Json::as_f64).unwrap() > 0.0);

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

#[test]
fn compare_reports_what_changed_since_an_earlier_run() {
    let fixture = build_fixture();
    let out = temp_dir();
    let exe = env!("CARGO_BIN_EXE_diskanalyzer");
    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(exe)
            .args(["--no-color", "--quiet", "--threads", "2"])
            .args(["--folder-min-size", "0", "--exclude-name", "skipme"])
            .args(args)
            .arg(&fixture.root)
            .output()
            .unwrap()
    };
    let (a, b, c) = (
        out.path().join("A"),
        out.path().join("B"),
        out.path().join("C"),
    );
    let sep = std::path::MAIN_SEPARATOR;
    // Only the CHANGED SINCE section counts: LARGEST FOLDERS lists b and a too.
    let lists_path_ending = |stdout: &str, tail: &str| {
        stdout
            .split("\n-- ")
            .filter(|section| section.starts_with("CHANGED SINCE"))
            .flat_map(str::lines)
            .any(|line| line.trim_end().ends_with(&format!("root{sep}{tail}")))
    };

    let first = run(&["--out".as_ref(), a.as_os_str()]);
    assert_eq!(first.status.code(), Some(0));

    // 50 KB more in b/: visible because the baseline listed every folder.
    write(&fixture.root.join("b").join("new.bin"), &[1u8; 50_000]);
    let second = run(&[
        "--compare".as_ref(),
        a.as_os_str(),
        "--out".as_ref(),
        b.as_os_str(),
    ]);
    let stdout = String::from_utf8_lossy(&second.stdout);
    assert_eq!(
        second.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(stdout.contains("-- CHANGED SINCE"), "{stdout}");
    assert!(lists_path_ending(&stdout, "b"), "{stdout}");
    assert!(!stdout.contains("no report.json"), "{stdout}");
    let changes = fs::read_to_string(b.join("changes.csv")).unwrap();
    assert!(changes
        .starts_with("\u{feff}delta_bytes,delta,before_bytes,after_bytes,status,full_path\r\n"));
    let b_path = fixture.root.join("b").display().to_string();
    assert!(
        changes.contains(&format!("50000,+48.8 KB,20005,70005,grew,{b_path}\r\n")),
        "{changes}"
    );
    let json = Json::parse(&fs::read_to_string(b.join("report.json")).unwrap()).unwrap();
    let grew = json
        .get("compare")
        .and_then(|c| c.get("grew"))
        .and_then(Json::as_array)
        .unwrap();
    assert_eq!(grew.len(), 1, "the chain up to the root collapses into b");

    // Writing the new reports over the baseline is refused before scanning.
    let clash = run(&[
        "--compare".as_ref(),
        a.as_os_str(),
        "--out".as_ref(),
        a.as_os_str(),
    ]);
    assert_eq!(clash.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&clash.stderr).contains("same directory"));

    // A baseline that does not exist fails fast and creates nothing.
    let nowhere = out.path().join("nowhere");
    let d = out.path().join("D");
    let missing = run(&[
        "--compare".as_ref(),
        nowhere.as_os_str(),
        "--out".as_ref(),
        d.as_os_str(),
    ]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(!d.exists());

    // Without report.json the floor is assumed to be the 1 MB default: the
    // 50 KB is no longer listed, a 1.2 MB change is, and the gap is noted.
    fs::remove_file(a.join("report.json")).unwrap();
    write(
        &fixture.root.join("a").join("big.bin"),
        &vec![2u8; 1_200_000],
    );
    let third = run(&[
        "--compare".as_ref(),
        a.as_os_str(),
        "--out".as_ref(),
        c.as_os_str(),
    ]);
    let stdout = String::from_utf8_lossy(&third.stdout);
    assert_eq!(third.status.code(), Some(0));
    assert!(stdout.contains("baseline has no report.json"), "{stdout}");
    assert!(lists_path_ending(&stdout, "a"), "{stdout}");
    assert!(!lists_path_ending(&stdout, "b"), "{stdout}");
}
