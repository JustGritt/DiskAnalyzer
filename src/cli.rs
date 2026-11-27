//! Command-line entry point: argument parsing and wiring.

use std::collections::HashSet;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;

use crate::compare::{compare, resolve_baseline_dir, same_dir, Baseline};
use crate::exclusions;
use crate::insights::{build_report, ReportOptions};
use crate::platform;
use crate::reports::csv::{write_changes_csv, write_error_csv, write_folder_csv, FileListWriter};
use crate::reports::html::write_html_payload;
use crate::reports::json::write_json;
use crate::reports::payload::build_payload;
use crate::reports::sink::OutputDir;
use crate::reports::terminal;
use crate::scanner::{scan, FileSink, ScanConfig, ScanProgress};
use crate::util::{human_count, human_size, now_epoch, parse_size};

fn size_arg(text: &str) -> Result<u64, String> {
    parse_size(text)
}

#[derive(Parser, Debug)]
#[command(
    name = "diskanalyzer",
    version = crate::VERSION,
    about = "Read-only inventory of what is using space on a drive. Skips the live \
             operating system by default. This tool cannot delete, move or modify anything.",
    after_help = "Nothing is ever removed. Reports tell you where to look; you decide.\n\
                  Sizes accept 500MB, 2GB, 1.5g or a plain byte count."
)]
pub struct Args {
    /// paths to scan (default: the system volume)
    pub roots: Vec<PathBuf>,

    /// also scan the live OS folders that are skipped by default
    #[arg(long, help_heading = "Scope")]
    pub include_os: bool,
    /// skip this folder (repeatable)
    #[arg(long, value_name = "PATH", help_heading = "Scope")]
    pub exclude: Vec<String>,
    /// skip any folder with this name (repeatable)
    #[arg(long, value_name = "NAME", help_heading = "Scope")]
    pub exclude_name: Vec<String>,
    /// descend into other mounted filesystems (Unix; off by default)
    #[arg(long, help_heading = "Scope")]
    pub cross_filesystems: bool,

    /// report on-disk size instead of logical size
    #[arg(long, help_heading = "Sizing")]
    pub allocated: bool,
    /// cluster size for --allocated on Windows (default: 4096)
    #[arg(long, value_name = "BYTES", help_heading = "Sizing")]
    pub cluster_size: Option<u64>,
    /// count a file with several hard links once
    #[arg(long, help_heading = "Sizing")]
    pub dedup_hardlinks: bool,

    /// folders per ranking
    #[arg(long, default_value_t = 25, value_name = "N", help_heading = "Detail")]
    pub top: usize,
    /// files per ranking
    #[arg(long, default_value_t = 30, value_name = "N", help_heading = "Detail")]
    pub files: usize,
    /// size floor for the untouched-files list
    #[arg(long, default_value = "100MB", value_name = "SIZE", value_parser = size_arg, help_heading = "Detail")]
    pub old_min_size: u64,
    /// age floor for the untouched-files list
    #[arg(
        long,
        default_value_t = 365,
        value_name = "DAYS",
        help_heading = "Detail"
    )]
    pub old_min_days: u64,
    /// a project unchanged this long counts as idle in the reclaimable-folders list
    #[arg(
        long,
        default_value_t = 90,
        value_name = "DAYS",
        help_heading = "Detail"
    )]
    pub stale_days: u64,
    /// look for duplicate files (reads 128KB per candidate)
    #[arg(long, help_heading = "Detail")]
    pub duplicates: bool,
    /// ignore duplicates smaller than this
    #[arg(long, default_value = "25MB", value_name = "SIZE", value_parser = size_arg, help_heading = "Detail")]
    pub dup_min_size: u64,

    /// report directory (default: ./reports/scan-<timestamp>)
    #[arg(long, value_name = "DIR", help_heading = "Output")]
    pub out: Option<PathBuf>,
    /// size floor for the CSV file listing; 0 lists every file
    #[arg(long, default_value = "10MB", value_name = "SIZE", value_parser = size_arg, help_heading = "Output")]
    pub csv_min_size: u64,
    /// size floor for the CSV folder listing and --compare; 0 lists every folder
    #[arg(long, default_value = "1MB", value_name = "SIZE", value_parser = size_arg, help_heading = "Output")]
    pub folder_min_size: u64,
    /// skip the HTML report
    #[arg(long, help_heading = "Output")]
    pub no_html: bool,
    /// skip the CSV listings
    #[arg(long, help_heading = "Output")]
    pub no_csv: bool,
    /// skip report.json, the machine-readable copy of the report
    #[arg(long, help_heading = "Output")]
    pub no_json: bool,
    /// disable ANSI color
    #[arg(long, help_heading = "Output")]
    pub no_color: bool,
    /// suppress progress output
    #[arg(long, help_heading = "Output")]
    pub quiet: bool,

    /// report directory of an earlier scan to compare against, or `last` for
    /// the newest one in ./reports
    #[arg(long, value_name = "DIR", help_heading = "Compare")]
    pub compare: Option<String>,

    /// worker threads (default: twice the CPU count, at most 16)
    #[arg(long, value_name = "N", help_heading = "Performance")]
    pub threads: Option<usize>,

    /// describe how this build guarantees it is read-only, and exit
    #[arg(long)]
    pub verify_read_only: bool,
}

const READ_ONLY_GUARANTEE: &str = "\
Read-only guarantee for this build

  - The scanner only ever opens directories and file metadata for reading,
    and opens file contents read-only when --duplicates compares them.
  - Exactly one module, reports::sink, may create files, and it only creates
    the report files by name inside the directory chosen with --out.
  - The build forbids every std::fs write, remove and rename API outside that
    module (clippy.toml, disallowed-methods) and a test audits the source for
    the same names and their Win32 equivalents. Both run in CI.

Enforcement is at build time: a compiled binary cannot audit its own source.
Nothing this program does can delete, move, rename or modify your files.";

/// A single rewritten status line, so a long scan does not scroll away.
struct ProgressLine {
    enabled: bool,
    width: usize,
}

impl ProgressLine {
    fn render(&mut self, files: u64, bytes: u64, path: &str) {
        if !self.enabled {
            return;
        }
        let shown: String = path.chars().take(58).collect();
        let text = format!(
            "  scanning  {:>11} files  {:>10}  {}",
            human_count(files),
            human_size(bytes),
            shown
        );
        let len = text.chars().count();
        let pad = self.width.saturating_sub(len);
        self.width = self.width.max(len);
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r{text}{}", " ".repeat(pad));
        let _ = err.flush();
    }

    fn done(&self) {
        if self.enabled {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{}\r", " ".repeat(self.width));
            let _ = err.flush();
        }
    }
}

/// `--exclude` paths and `--exclude-name` names that skipped nothing, so a
/// typo is reported instead of quietly scanning what the user meant to skip.
fn unmatched_exclusions<'a>(
    exclude: &'a [String],
    exclude_name: &'a [String],
    skipped: &[(String, &'static str)],
) -> (Vec<&'a str>, Vec<&'a str>) {
    let skipped_dirs: HashSet<String> = skipped
        .iter()
        .filter(|(_, reason)| *reason == exclusions::USER_EXCLUDED)
        .map(|(path, _)| exclusions::normalize_key(path))
        .collect();
    let skipped_names: HashSet<String> = skipped
        .iter()
        .filter(|(_, reason)| *reason == exclusions::USER_EXCLUDED_NAME)
        .filter_map(|(path, _)| {
            path.trim_end_matches(['\\', '/'])
                .rsplit(['\\', '/'])
                .next()
        })
        .map(str::to_lowercase)
        .collect();
    let dirs = exclude
        .iter()
        .filter(|path| {
            let key = exclusions::normalize_key(&exclusions::absolute_display(path));
            !skipped_dirs.contains(&key)
        })
        .map(String::as_str)
        .collect();
    let names = exclude_name
        .iter()
        .filter(|name| {
            let name = name.trim().to_lowercase();
            !name.is_empty() && !skipped_names.contains(&name)
        })
        .map(String::as_str)
        .collect();
    (dirs, names)
}

fn default_output_dir() -> PathBuf {
    let stamp = match platform::local_datetime(now_epoch()) {
        Some((y, mo, d, h, mi, s)) => format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}"),
        None => "unknown-time".to_string(),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join("reports").join(format!("scan-{stamp}"))
}

pub fn run() -> i32 {
    run_with(Args::parse())
}

pub fn run_with(args: Args) -> i32 {
    if args.verify_read_only {
        println!("{READ_ONLY_GUARANTEE}");
        return 0;
    }

    let roots: Vec<PathBuf> = if args.roots.is_empty() {
        vec![platform::default_root()]
    } else {
        args.roots.clone()
    };
    let missing: Vec<String> = roots
        .iter()
        .filter(|root| !root.is_dir())
        .map(|root| root.display().to_string())
        .collect();
    if !missing.is_empty() {
        eprintln!("Not a folder: {}", missing.join(", "));
        return 2;
    }

    if args.cluster_size.is_some() && !cfg!(windows) {
        eprintln!(
            "--cluster-size only applies on Windows; --allocated uses real block counts here."
        );
    }
    for path in &args.exclude {
        if !Path::new(path).is_dir() {
            eprintln!(
                "Note: --exclude {path} is not an existing folder, so it will not skip anything."
            );
        }
    }

    let config = ScanConfig {
        roots: roots.clone(),
        include_os: args.include_os,
        exclude_dirs: args.exclude.clone(),
        exclude_names: args.exclude_name.clone(),
        use_allocated: args.allocated,
        cluster_size: args.cluster_size.unwrap_or(4096),
        old_file_min_size: args.old_min_size,
        old_file_min_days: args.old_min_days,
        find_duplicates: args.duplicates,
        duplicate_min_size: args.dup_min_size,
        cross_filesystems: args.cross_filesystems,
        dedup_hardlinks: args.dedup_hardlinks,
        threads: args.threads.unwrap_or(0),
        now_override: None,
    };

    let want_files = !args.no_csv;
    let want_html = !args.no_html;
    let want_json = !args.no_json;
    let out_dir = args.out.clone().unwrap_or_else(default_output_dir);

    // The baseline is read before anything is created, so a bad --compare
    // fails fast and never leaves an empty report directory behind.
    let baseline = match &args.compare {
        None => None,
        Some(arg) => {
            let dir = match resolve_baseline_dir(arg, &out_dir) {
                Ok(dir) => dir,
                Err(err) => {
                    eprintln!("Cannot compare: {err}");
                    return 2;
                }
            };
            if same_dir(&dir, &out_dir) {
                eprintln!(
                    "Cannot compare: --compare and --out are the same directory ({}); \
                     the new reports would overwrite the baseline. Choose another --out.",
                    dir.display()
                );
                return 2;
            }
            match Baseline::load(&dir) {
                Ok(base) => {
                    if !args.quiet {
                        eprintln!("Comparing with {} ({})", dir.display(), base.label);
                    }
                    Some(base)
                }
                Err(err) => {
                    eprintln!("Cannot compare: {err}");
                    return 2;
                }
            }
        }
    };

    let output = if want_files || want_html || want_json {
        match OutputDir::create(&out_dir) {
            Ok(output) => Some(output),
            Err(err) => {
                eprintln!(
                    "Cannot create report directory {}: {err}",
                    out_dir.display()
                );
                return 1;
            }
        }
    } else {
        None
    };

    let mut writer = match (&output, want_files) {
        (Some(output), true) => match FileListWriter::new(output, args.csv_min_size) {
            Ok(writer) => Some(writer),
            Err(err) => {
                eprintln!("Cannot create files.csv: {err}");
                return 1;
            }
        },
        _ => None,
    };

    platform::install_ctrl_c();
    if !args.quiet {
        let names: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
        eprintln!("Scanning {} ... (read-only)", names.join(", "));
    }

    let progress = ScanProgress::default();
    let stop = AtomicBool::new(false);
    let mut line = ProgressLine {
        enabled: !args.quiet && std::io::stderr().is_terminal(),
        width: 0,
    };
    let mut result = thread::scope(|scope| {
        let ticker = scope.spawn(|| {
            let mut last = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(30));
                if last.elapsed() >= Duration::from_millis(150) {
                    last = Instant::now();
                    let files = progress.files.load(Ordering::Relaxed);
                    if files == 0 {
                        continue;
                    }
                    let bytes = progress.bytes.load(Ordering::Relaxed);
                    let path = progress
                        .current
                        .lock()
                        .map(|p| p.clone())
                        .unwrap_or_default();
                    line.render(files, bytes, &path);
                }
            }
        });
        let sink = writer.as_mut().map(|w| w as &mut dyn FileSink);
        let result = scan(&config, sink, args.csv_min_size, Some(&progress));
        stop.store(true, Ordering::Relaxed);
        let _ = ticker.join();
        result
    });
    line.done();

    if result.cancelled {
        drop(writer);
        eprintln!("\nScan cancelled. Nothing was modified.");
        return 130;
    }
    if let Some(writer) = writer.take() {
        if let Err(err) = writer.finish() {
            eprintln!("Warning: files.csv was not fully written: {err}");
        }
    }
    let (unused_dirs, unused_names) =
        unmatched_exclusions(&args.exclude, &args.exclude_name, &result.skipped);
    for path in unused_dirs {
        if Path::new(path).is_dir() {
            eprintln!("Note: --exclude {path} matched no folder inside the scanned roots.");
        }
    }
    for name in unused_names {
        eprintln!("Note: --exclude-name {name} matched no folder in this scan.");
    }

    let options = ReportOptions {
        top_n: args.top,
        file_n: args.files,
        with_duplicates: args.duplicates,
        csv_min_size: args.csv_min_size,
        folder_min_size: args.folder_min_size,
        stale_days: args.stale_days,
    };
    let mut report = build_report(&mut result, &options);
    report.comparison = baseline
        .as_ref()
        .map(|base| compare(&result, base, args.top, args.folder_min_size));
    drop(baseline);

    let color = !args.no_color && terminal::supports_color();
    let text = terminal::render(&report, &result, color, terminal::terminal_width());
    {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    let mut written: Vec<(&str, PathBuf)> = Vec::new();
    if let Some(output) = &output {
        if want_files {
            written.push(("files.csv", output.join("files.csv")));
            match write_folder_csv(&result, output, args.folder_min_size) {
                Ok(_) => written.push(("folders.csv", output.join("folders.csv"))),
                Err(err) => eprintln!("Warning: folders.csv was not written: {err}"),
            }
            if !result.errors.is_empty() {
                match write_error_csv(&result, output) {
                    Ok(_) => written.push(("unreadable.csv", output.join("unreadable.csv"))),
                    Err(err) => eprintln!("Warning: unreadable.csv was not written: {err}"),
                }
            }
            if let Some(comparison) = &report.comparison {
                match write_changes_csv(comparison, output) {
                    Ok(_) => written.push(("changes.csv", output.join("changes.csv"))),
                    Err(err) => eprintln!("Warning: changes.csv was not written: {err}"),
                }
            }
        }
        // One payload for both documents, so they cannot disagree.
        let payload = (want_html || want_json).then(|| build_payload(&report, &result));
        if let (true, Some(payload)) = (want_html, &payload) {
            match write_html_payload(payload, output) {
                Ok(()) => written.push(("report.html", output.join("report.html"))),
                Err(err) => eprintln!("Warning: report.html was not written: {err}"),
            }
        }
        if let (true, Some(payload)) = (want_json, &payload) {
            match write_json(payload, output) {
                Ok(()) => written.push(("report.json", output.join("report.json"))),
                Err(err) => eprintln!("Warning: report.json was not written: {err}"),
            }
        }
    }

    if !written.is_empty() {
        let bold = if color { "\x1b[1m" } else { "" };
        let reset = if color { "\x1b[0m" } else { "" };
        let mut out = std::io::stdout().lock();
        let _ = writeln!(
            out,
            " Reports written to {bold}{}{reset}",
            out_dir.display()
        );
        for (label, path) in &written {
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let _ = writeln!(out, "   {label:<16} {:>10}", human_size(size));
        }
        let _ = writeln!(out);
        if want_html {
            let _ = writeln!(
                out,
                "   Open report.html in a browser for the interactive treemap."
            );
        }
        if want_files && args.csv_min_size > 0 {
            let _ = writeln!(
                out,
                "   files.csv lists files over {} and folders.csv folders over {}; \
                 pass --csv-min-size 0 --folder-min-size 0 for everything.",
                human_size(args.csv_min_size),
                human_size(args.folder_min_size)
            );
        }
        let _ = writeln!(out);
        let _ = out.flush();
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_python_tool() {
        let args = Args::parse_from(["diskanalyzer"]);
        assert_eq!(args.top, 25);
        assert_eq!(args.files, 30);
        assert_eq!(args.old_min_size, 100 * 1024 * 1024);
        assert_eq!(args.old_min_days, 365);
        assert_eq!(args.dup_min_size, 25 * 1024 * 1024);
        assert_eq!(args.csv_min_size, 10 * 1024 * 1024);
        assert_eq!(args.folder_min_size, 1024 * 1024);
        assert_eq!(args.stale_days, 90);
        assert_eq!(args.stale_days, ReportOptions::default().stale_days);
        assert!(!args.duplicates);
        assert!(args.roots.is_empty());
    }

    #[test]
    fn repeatable_and_size_flags_parse() {
        let args = Args::parse_from([
            "diskanalyzer",
            "--exclude",
            "/a",
            "--exclude",
            "/b",
            "--exclude-name",
            "node_modules",
            "--csv-min-size",
            "0",
            "--old-min-size",
            "1.5g",
            "/tmp",
        ]);
        assert_eq!(args.exclude, vec!["/a", "/b"]);
        assert_eq!(args.exclude_name, vec!["node_modules"]);
        assert_eq!(args.csv_min_size, 0);
        assert_eq!(args.old_min_size, 1_610_612_736);
        assert_eq!(args.roots, vec![PathBuf::from("/tmp")]);
        assert!(Args::try_parse_from(["diskanalyzer", "--csv-min-size", "lots"]).is_err());
    }

    #[test]
    fn exclusions_that_skipped_nothing_are_reported() {
        let hit = exclusions::absolute_display("hit");
        let hit_name = platform::join_display(&exclusions::absolute_display("x"), "Build");
        let skipped = vec![
            (hit.clone(), exclusions::USER_EXCLUDED),
            (hit_name, exclusions::USER_EXCLUDED_NAME),
            (exclusions::absolute_display("os"), "windows os"),
        ];
        let exclude = vec!["./hit".to_string(), "miss".to_string(), "os".to_string()];
        let names = vec![
            "build".to_string(),
            "nothing-here".to_string(),
            " ".to_string(),
        ];
        let (dirs, unused_names) = unmatched_exclusions(&exclude, &names, &skipped);
        assert_eq!(dirs, vec!["miss", "os"]);
        assert_eq!(unused_names, vec!["nothing-here"]);
    }
}
