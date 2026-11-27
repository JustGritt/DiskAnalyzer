//! The terminal report.
//!
//! Ordering is deliberate: the volume gauge first (why you are here), then
//! where the space physically is, then what kind of thing it is, and only
//! afterwards the long tail of per-file detail. Someone who reads the first
//! screen and stops should still know what to do next.

use std::fmt::Write as _;
use std::io::IsTerminal;

use crate::categories::Confidence;
use crate::compare::{Change, ChangeStatus};
use crate::exclusions;
use crate::insights::{Report, RECLAIM_MIN_SIZE};
use crate::platform;
use crate::projects;
use crate::scanner::ScanResult;
use crate::util::{
    human_age, human_count, human_days, human_delta, human_duration, human_size, truncate_middle,
    truncate_start,
};

/// ANSI colors, or transparent no-ops when the terminal cannot show them.
pub struct Palette {
    enabled: bool,
}

impl Palette {
    pub fn new(enabled: bool) -> Palette {
        Palette { enabled }
    }

    fn code(style: &str) -> &'static str {
        match style {
            "dim" => "\x1b[2m",
            "bold" => "\x1b[1m",
            "red" => "\x1b[31m",
            "yellow" => "\x1b[33m",
            "green" => "\x1b[32m",
            "cyan" => "\x1b[36m",
            "blue" => "\x1b[34m",
            _ => "",
        }
    }

    pub fn paint(&self, text: &str, styles: &[&str]) -> String {
        if !self.enabled || styles.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len() + 12);
        for style in styles {
            out.push_str(Palette::code(style));
        }
        out.push_str(text);
        out.push_str("\x1b[0m");
        out
    }
}

fn confidence_style(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Safe => "green",
        Confidence::Review => "yellow",
        Confidence::Info => "dim",
    }
}

fn bar(fraction: f64, width: usize) -> String {
    let fraction = fraction.clamp(0.0, 1.0);
    let filled = ((fraction * width as f64).round() as usize).min(width);
    format!("{}{}", "#".repeat(filled), ".".repeat(width - filled))
}

/// Whether stdout can show ANSI color: honours `NO_COLOR`, requires a
/// terminal, and on Windows asks the console to enable VT processing.
pub fn supports_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if !std::io::stdout().is_terminal() {
        return false;
    }
    platform::enable_vt()
}

pub fn terminal_width() -> usize {
    match platform::term_width() {
        Some(width) => width.clamp(60, 140),
        None => 100,
    }
}

struct TerminalReport<'a> {
    report: &'a Report,
    result: &'a ScanResult,
    c: Palette,
    width: usize,
    out: String,
}

impl TerminalReport<'_> {
    // -- plumbing ----------------------------------------------------------
    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn blank(&mut self) {
        self.out.push('\n');
    }

    fn heading(&mut self, title: &str) {
        self.blank();
        let rule = "-".repeat(self.width.saturating_sub(title.len() + 3));
        let text = format!(
            "{}{}",
            self.c.paint(&format!("-- {title} "), &["bold"]),
            self.c.paint(&rule, &["dim"])
        );
        self.line(&text);
    }

    fn note(&mut self, text: &str) {
        let text = self.c.paint(&format!("   {text}"), &["dim"]);
        self.line(&text);
    }

    // -- sections ----------------------------------------------------------
    fn render(&mut self) {
        self.render_header();
        self.render_volumes();
        self.render_scan_summary();
        self.render_comparison();
        self.render_roots();
        self.render_top_folders();
        self.render_categories();
        self.render_reclaim_folders();
        self.render_top_files();
        self.render_old_files();
        self.render_extensions();
        self.render_ages();
        self.render_duplicates();
        self.render_footnotes();
    }

    fn render_header(&mut self) {
        self.blank();
        let rule = self.c.paint(&"=".repeat(self.width), &["dim"]);
        self.line(&rule);
        let title = self.c.paint(" DISK ANALYZER  ", &["bold"]);
        let subtitle = self.c.paint(
            "read-only inventory - nothing on disk is modified",
            &["dim"],
        );
        self.line(&format!("{title}{subtitle}"));
        self.line(&rule);
    }

    fn render_volumes(&mut self) {
        if self.result.volumes.is_empty() {
            return;
        }
        self.blank();
        for (root, (total, used, free)) in &self.result.volumes {
            let share = if *total > 0 {
                *used as f64 / *total as f64
            } else {
                0.0
            };
            let gauge = bar(share, 34);
            let style = if *free < 20 * 1024 * 1024 * 1024 {
                "red"
            } else if *free < 60 * 1024 * 1024 * 1024 {
                "yellow"
            } else {
                "green"
            };
            let text = format!(
                " {:<4} {} used of {}   [{}] {:.0}%",
                root,
                human_size(*used),
                human_size(*total),
                self.c.paint(&gauge, &[style]),
                share * 100.0
            );
            self.line(&text);
            let free_text = self
                .c
                .paint(&format!("{} free", human_size(*free)), &[style, "bold"]);
            self.line(&format!("      {free_text}"));
        }
    }

    fn render_scan_summary(&mut self) {
        let result = self.result;
        // Coverage only means something when a whole volume was asked for;
        // a folder scan is a sliver of the volume by design.
        let scans_volume = result
            .root_ids
            .iter()
            .any(|&id| platform::is_volume_root(&result.nodes[id as usize].name));
        let coverage = if scans_volume {
            let used_total: u64 = result.volumes.iter().map(|(_, (_t, used, _f))| *used).sum();
            let scanned_share = result.total_size as f64 / used_total.max(1) as f64;
            format!("  ({:.0}% of used space)", scanned_share * 100.0)
        } else {
            String::new()
        };
        self.blank();
        let text = format!(
            " Scanned  {} files in {} folders  =  {}{}",
            human_count(result.total_files),
            human_count(result.total_dirs),
            self.c.paint(&human_size(result.total_size), &["bold"]),
            coverage
        );
        self.line(&text);
        self.line(&format!(" Time     {}", human_duration(result.elapsed)));

        if !result.skipped.is_empty() {
            let mut reasons: Vec<&str> = result.skipped.iter().map(|(_, reason)| *reason).collect();
            reasons.sort_unstable();
            reasons.dedup();
            // Only the OS tables are lifted by --include-os; suggesting it for
            // the user's own exclusions would be wrong.
            let hint = if reasons
                .iter()
                .any(|reason| exclusions::is_os_reason(reason))
            {
                self.c.paint("  --include-os to scan them", &["dim"])
            } else {
                String::new()
            };
            let text = format!(
                " Skipped  {} paths ({}){}",
                result.skipped.len(),
                reasons.join(", "),
                hint
            );
            self.line(&text);
        }
        if !result.links.is_empty() {
            self.line(&format!(
                " Links    {} junctions/symlinks not followed, so nothing is counted twice",
                result.links.len()
            ));
        }
        if result.cloud_only.0 > 0 {
            self.line(&format!(
                " Cloud    {} cloud-only files ({}) not stored on this disk; --duplicates does not read them",
                human_count(result.cloud_only.0),
                human_size(result.cloud_only.1)
            ));
        }
        if !result.errors.is_empty() {
            let text = self.c.paint(
                &format!(
                    " Errors   {} paths unreadable ({} to see more)",
                    result.errors.len(),
                    platform::ERRORS_HINT
                ),
                &["dim"],
            );
            self.line(&text);
        }
    }

    fn render_comparison(&mut self) {
        let Some(comparison) = &self.report.comparison else {
            return;
        };
        self.heading(&format!("CHANGED SINCE {}", comparison.base_label));
        for warning in &comparison.warnings {
            self.note(warning);
        }
        let mut totals = format!(
            " Scanned  {} -> {}  ({})",
            human_size(comparison.before_total),
            human_size(comparison.after_total),
            human_delta(comparison.after_total as i64 - comparison.before_total as i64)
        );
        let free =
            |before: u64, after: u64| format!("{} -> {}", human_size(before), human_size(after));
        match comparison.volumes.as_slice() {
            [] => self.line(&totals),
            [(_, _, _, free_before, free_after)] => {
                let _ = write!(totals, "      Free  {}", free(*free_before, *free_after));
                self.line(&totals);
            }
            volumes => {
                self.line(&totals);
                for (label, _, _, free_before, free_after) in volumes {
                    self.line(&format!(
                        " Free     {label}  {}",
                        free(*free_before, *free_after)
                    ));
                }
            }
        }

        let mut shrank: Vec<&Change> = comparison
            .shrank
            .iter()
            .chain(&comparison.removed)
            .collect();
        shrank.sort_by(|a, b| a.delta.cmp(&b.delta).then_with(|| a.path.cmp(&b.path)));
        shrank.truncate(self.report.options.top_n);
        if comparison.grew.is_empty() && shrank.is_empty() {
            self.note(&format!(
                "no folder changed by {} or more",
                human_size(comparison.min_change)
            ));
            return;
        }
        // delta, before, arrow, after: 3 + 11 + 2 + 10 + 4 + 10 + 2. A signed
        // "+1,011.6 MB" is 11 wide.
        let path_width = self.width.saturating_sub(42).max(30);
        if !comparison.grew.is_empty() {
            self.line(" Grew");
            for change in &comparison.grew {
                self.change_line(change, path_width);
            }
        }
        if !shrank.is_empty() {
            self.line(" Shrank");
            for change in shrank {
                self.change_line(change, path_width);
            }
        }
        self.note(&format!(
            "folders collapsed to where the change happened; changes under {} not listed",
            human_size(comparison.min_change)
        ));
    }

    fn change_line(&mut self, change: &Change, path_width: usize) {
        let style = if change.delta > 0 { "yellow" } else { "green" };
        let delta = self
            .c
            .paint(&format!("{:>11}", human_delta(change.delta)), &[style]);
        let before = change.before.map_or_else(|| "new".to_string(), human_size);
        let after = match (change.after, change.status) {
            (Some(_), ChangeStatus::NowUnreadable) => "unreadable".to_string(),
            (Some(after), _) => human_size(after),
            (None, ChangeStatus::NowExcluded) => "excluded".to_string(),
            (None, _) => "removed".to_string(),
        };
        let text = format!(
            "   {delta}  {before:>10} -> {after:<10}  {}",
            truncate_start(&change.path, path_width)
        );
        self.line(&text);
    }

    fn render_roots(&mut self) {
        self.heading("WHERE THE SPACE IS");
        for (root, children) in &self.report.root_breakdown {
            let root_node = &self.result.nodes[*root as usize];
            let total = root_node.total_size.max(1) as f64;
            let text = format!(
                " {}  {}",
                self.result.node_path(*root),
                self.c.paint(&human_size(root_node.total_size), &["bold"])
            );
            self.line(&text);
            for &child in children.iter().take(12) {
                let node = &self.result.nodes[child as usize];
                let share = node.total_size as f64 / total;
                let text = format!(
                    "   {:>11}  {:5.1}%  {}  {}",
                    human_size(node.total_size),
                    share * 100.0,
                    self.c.paint(&bar(share, 18), &["cyan"]),
                    node.name
                );
                self.line(&text);
            }
            if children.len() > 12 {
                let rest: u64 = children[12..]
                    .iter()
                    .map(|&id| self.result.nodes[id as usize].total_size)
                    .sum();
                self.note(&format!(
                    "{:>11}         + {} more folders",
                    human_size(rest),
                    children.len() - 12
                ));
            }
        }
    }

    fn render_top_folders(&mut self) {
        self.heading("LARGEST FOLDERS");
        self.note(
            "chains collapsed: each row is the deepest folder that still holds the whole total",
        );
        self.blank();
        let path_width = self.width.saturating_sub(34).max(40);
        for &id in &self.report.top_folders {
            let node = &self.result.nodes[id as usize];
            let mut tag = String::new();
            if let Some(category) = node.category {
                if category.confidence() != Confidence::Info {
                    tag = self.c.paint(
                        &format!(" [{}]", category.key()),
                        &[confidence_style(category.confidence())],
                    );
                }
            }
            let text = format!(
                " {:>11}  {:>9} files  {}{}",
                human_size(node.total_size),
                human_count(node.total_files),
                truncate_middle(&self.result.node_path(id), path_width),
                tag
            );
            self.line(&text);
        }
    }

    fn render_categories(&mut self) {
        self.heading("WHAT IT IS");
        let rows = &self.report.category_rows;
        if rows.is_empty() {
            return;
        }
        let label_width = rows
            .iter()
            .map(|(cat, ..)| cat.label().len())
            .max()
            .unwrap_or(0);
        for (category, _count, size, share) in rows {
            let style = confidence_style(category.confidence());
            let text = format!(
                " {:>11}  {:5.1}%  {}  {}  {}",
                human_size(*size),
                share * 100.0,
                self.c
                    .paint(&format!("{:<label_width$}", category.label()), &[style]),
                self.c.paint(&bar(*share, 14), &[style]),
                self.c.paint(category.hint(), &["dim"])
            );
            self.line(&text);
        }

        let reclaimable = &self.report.reclaimable;
        self.blank();
        let text = format!(
            " {}  {}{}",
            self.c.paint("Regenerable", &["green", "bold"]),
            human_size(reclaimable.safe),
            self.c.paint(
                "   caches and build output that rebuild themselves",
                &["dim"]
            )
        );
        self.line(&text);
        let text = format!(
            " {} {}{}",
            self.c.paint("Worth review", &["yellow", "bold"]),
            human_size(reclaimable.review),
            self.c
                .paint("   often unwanted, but only you can say", &["dim"])
        );
        self.line(&text);
        if reclaimable.duplicates > 0 {
            let text = format!(
                " {}  {}{}",
                self.c.paint("Duplicated", &["cyan", "bold"]),
                human_size(reclaimable.duplicates),
                self.c.paint("   extra copies beyond the first", &["dim"])
            );
            self.line(&text);
        }
        self.blank();
        self.note("DiskAnalyzer never deletes. These are places to look, not instructions.");
    }

    fn render_reclaim_folders(&mut self) {
        let found = &self.report.reclaim_folders;
        if found.folders.is_empty() {
            return;
        }
        let result = self.result;
        self.heading("RECLAIMABLE FOLDERS");
        self.note("top-most regenerable or review folders. idle = time since the project around");
        self.note("the folder last changed, not counting the folder itself; with no project, its own files");
        self.blank();

        let key_width = found
            .folders
            .iter()
            .map(|folder| folder.category.key().len())
            .max()
            .unwrap_or(0);
        let project_names: Vec<String> = found
            .folders
            .iter()
            .map(|folder| match folder.project {
                Some(id) => {
                    let node = &result.nodes[id as usize];
                    let name = match projects::primary_kind(node.markers) {
                        Some(kind) => format!("{} ({kind})", node.name),
                        None => node.name.to_string(),
                    };
                    truncate_middle(&name, 30)
                }
                None => "-".to_string(),
            })
            .collect();
        let project_width = project_names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(1);
        let idle_texts: Vec<String> = found
            .folders
            .iter()
            .map(|folder| format!("idle {}", human_age(folder.activity, result.now)))
            .collect();
        let idle_width = idle_texts.iter().map(String::len).max().unwrap_or(0);
        let path_width = self
            .width
            .saturating_sub(13 + key_width + 2 + idle_width + 2 + project_width + 2)
            .max(30);
        for ((folder, project), idle) in found.folders.iter().zip(&project_names).zip(&idle_texts) {
            // Pad before coloring, as elsewhere: escape codes count toward widths.
            let category = self.c.paint(
                &format!("{:<key_width$}", folder.category.key()),
                &[confidence_style(folder.category.confidence())],
            );
            let idle = format!("{idle:<idle_width$}");
            let idle = if folder.stale {
                self.c.paint(&idle, &["yellow"])
            } else {
                idle
            };
            // The end of the path names the project and the folder; keep it.
            let text = format!(
                " {:>11}  {}  {}  {:<project_width$}  {}",
                human_size(folder.size),
                category,
                idle,
                project,
                truncate_start(&result.node_path(folder.id), path_width)
            );
            self.line(&text);
        }

        self.blank();
        let mut summary = format!(
            "{} folder{} over {}, {} in all",
            human_count(found.count as u64),
            if found.count == 1 { "" } else { "s" },
            human_size(RECLAIM_MIN_SIZE),
            human_size(found.total)
        );
        if found.count > found.folders.len() {
            let _ = write!(summary, " (largest {} shown)", found.folders.len());
        }
        if found.stale > 0 {
            let _ = write!(
                summary,
                "; {} idle for {} or more",
                human_size(found.stale),
                human_days(self.report.options.stale_days)
            );
        }
        self.note(&summary);
        self.note(
            "Single files such as disc images are not folders; see LARGEST FILES and files.csv.",
        );

        // Once per distinct hint, not per row: a list of twenty `target`
        // folders needs `cargo clean` said once.
        let mut hints: Vec<(&str, &str)> = Vec::new();
        for folder in &found.folders {
            if let Some(hint) = folder.hint {
                if !hints.iter().any(|(_, seen)| *seen == hint) {
                    hints.push((&result.nodes[folder.id as usize].name, hint));
                }
            }
        }
        if hints.is_empty() {
            return;
        }
        self.blank();
        self.note(
            "How each tool clears its own folder. DiskAnalyzer prints these and never runs them:",
        );
        let name_width = hints
            .iter()
            .map(|(name, _)| name.chars().count())
            .max()
            .unwrap_or(0);
        for (name, hint) in hints {
            self.line(&format!("     {name:<name_width$}  {hint}"));
        }
    }

    fn render_top_files(&mut self) {
        self.heading("LARGEST FILES");
        let path_width = self.width.saturating_sub(30).max(40);
        let now = self.result.now;
        for rec in &self.report.top_files {
            let text = format!(
                " {:>11}  {:>6}  {}",
                human_size(rec.size),
                human_age(rec.mtime, now),
                truncate_middle(&rec.path, path_width)
            );
            self.line(&text);
        }
    }

    fn render_old_files(&mut self) {
        if self.report.old_files.is_empty() {
            return;
        }
        let config = &self.result.config;
        self.heading(&format!(
            "BIG AND UNTOUCHED  (over {}, not modified for {} or more)",
            human_size(config.old_file_min_size),
            human_days(config.old_file_min_days)
        ));
        self.note("large files you have not written to in a long time: the best place to start");
        self.blank();
        let path_width = self.width.saturating_sub(30).max(40);
        let now = self.result.now;
        for rec in &self.report.old_files {
            // Pad before coloring: escape codes are invisible but would still
            // count toward a format width.
            let age = self
                .c
                .paint(&format!("{:>6}", human_age(rec.mtime, now)), &["yellow"]);
            let text = format!(
                " {:>11}  {}  {}",
                human_size(rec.size),
                age,
                truncate_middle(&rec.path, path_width)
            );
            self.line(&text);
        }
    }

    fn render_extensions(&mut self) {
        self.heading("BY FILE TYPE");
        for (ext, count, size, share) in self.report.extension_rows.iter().take(15) {
            let text = format!(
                " {:>11}  {:5.1}%  {:<16} {:>10} files",
                human_size(*size),
                share * 100.0,
                ext,
                human_count(*count)
            );
            self.line(&text);
        }
    }

    fn render_ages(&mut self) {
        self.heading("BY AGE (last modified)");
        let total = self.result.total_size.max(1) as f64;
        for (label, count, size) in &self.report.age_rows {
            let share = *size as f64 / total;
            let text = format!(
                " {:>11}  {:5.1}%  {:<16} {}  {:>10} files",
                human_size(*size),
                share * 100.0,
                label,
                self.c.paint(&bar(share, 20), &["blue"]),
                human_count(*count)
            );
            self.line(&text);
        }
    }

    fn render_duplicates(&mut self) {
        let groups = &self.report.duplicates;
        if groups.is_empty() {
            return;
        }
        let wasted: u64 = groups.iter().map(|g| g.wasted()).sum();
        self.heading(&format!(
            "LIKELY DUPLICATES  ({} in extra copies)",
            human_size(wasted)
        ));
        self.note("matched on exact size plus first and last 64 KB - strong evidence, not proof");
        self.blank();
        let path_width = self.width.saturating_sub(20).max(40);
        for group in groups.iter().take(12) {
            let text = format!(
                " {:>11} x {}  {}",
                human_size(group.size),
                group.paths.len(),
                self.c.paint(
                    &format!("({} recoverable)", human_size(group.wasted())),
                    &["cyan"]
                )
            );
            self.line(&text);
            for path in group.paths.iter().take(4) {
                let text = format!("             {}", truncate_middle(path, path_width));
                self.line(&text);
            }
            if group.paths.len() > 4 {
                self.note(&format!(
                    "          + {} more copies",
                    group.paths.len() - 4
                ));
            }
        }
        if groups.len() > 12 {
            self.note(&format!(
                "+ {} more duplicate groups (see the HTML report)",
                groups.len() - 12
            ));
        }
        if self.report.duplicates_truncated {
            self.note("duplicate search stopped early at its read budget; more may exist");
        }
    }

    fn render_footnotes(&mut self) {
        if !self.result.os_managed.is_empty() {
            self.heading(&format!(
                "OS-MANAGED FILES  (sized by {}, not by you)",
                platform::OS_NAME
            ));
            let mut managed: Vec<&(String, u64)> = self.result.os_managed.iter().collect();
            managed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (path, size) in managed {
                let text = format!(" {:>11}  {}", human_size(*size), path);
                self.line(&text);
            }
            self.note("adjust these through System settings, never by deleting them");
        }

        self.blank();
        let rule = self.c.paint(&"-".repeat(self.width), &["dim"]);
        self.line(&rule);
        let text = format!(
            "{}{}",
            self.c
                .paint(" Nothing was modified, moved or deleted. ", &["green"]),
            self.c
                .paint("Sizes are logical unless --allocated was used.", &["dim"])
        );
        self.line(&text);
        self.blank();
    }
}

/// Render the whole report to a string ready to print.
pub fn render(report: &Report, result: &ScanResult, color: bool, width: usize) -> String {
    let mut tr = TerminalReport {
        report,
        result,
        c: Palette::new(color),
        width,
        out: String::new(),
    };
    tr.render();
    let _ = write!(tr.out, "");
    tr.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_round_like_python() {
        assert_eq!(bar(0.0, 4), "....");
        assert_eq!(bar(0.5, 4), "##..");
        assert_eq!(bar(0.26, 4), "#...");
        assert_eq!(bar(1.5, 4), "####");
    }

    #[test]
    fn palette_wraps_and_resets() {
        assert_eq!(Palette::new(false).paint("x", &["bold"]), "x");
        assert_eq!(Palette::new(true).paint("x", &[]), "x");
        assert_eq!(
            Palette::new(true).paint("x", &["green", "bold"]),
            "\x1b[32m\x1b[1mx\x1b[0m"
        );
    }
}
