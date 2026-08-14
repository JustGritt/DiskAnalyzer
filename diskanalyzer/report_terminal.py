"""The terminal report.

Ordering is deliberate: the volume gauge first (why you are here), then where
the space physically is, then what kind of thing it is, and only afterwards the
long tail of per-file detail. Someone who reads the first screen and stops
should still know what to do next.
"""

from __future__ import annotations

import sys

from . import categories
from .insights import Report
from .util import (
    human_age,
    human_count,
    human_duration,
    human_size,
    supports_color,
    terminal_width,
    truncate_middle,
)


class Palette:
    """ANSI colors, or transparent no-ops when the terminal cannot show them."""

    CODES = {
        "dim": "\033[2m",
        "bold": "\033[1m",
        "red": "\033[31m",
        "yellow": "\033[33m",
        "green": "\033[32m",
        "cyan": "\033[36m",
        "blue": "\033[34m",
        "reset": "\033[0m",
    }

    def __init__(self, enabled: bool):
        self.enabled = enabled

    def __call__(self, text: str, *styles: str) -> str:
        if not self.enabled or not styles:
            return text
        prefix = "".join(self.CODES.get(style, "") for style in styles)
        return f"{prefix}{text}{self.CODES['reset']}"


CONFIDENCE_STYLE = {"safe": "green", "review": "yellow", "info": "dim"}
CONFIDENCE_TAG = {"safe": "regenerable", "review": "check first", "info": "in use"}


def _bar(fraction: float, width: int, fill: str = "#", empty: str = ".") -> str:
    fraction = max(0.0, min(1.0, fraction))
    filled = int(round(fraction * width))
    return fill * filled + empty * (width - filled)


class TerminalReport:
    def __init__(self, report: Report, stream=None, color: bool | None = None):
        self.report = report
        self.result = report.result
        self.stream = stream or sys.stdout
        if color is None:
            color = supports_color(self.stream)
        self.c = Palette(color)
        self.width = terminal_width()

    # -- plumbing ----------------------------------------------------------
    def line(self, text: str = "") -> None:
        print(text, file=self.stream)

    def heading(self, title: str) -> None:
        self.line()
        rule = "-" * max(0, self.width - len(title) - 3)
        self.line(self.c(f"-- {title} ", "bold") + self.c(rule, "dim"))

    def note(self, text: str) -> None:
        self.line(self.c("   " + text, "dim"))

    # -- sections ----------------------------------------------------------
    def render(self) -> None:
        self.render_header()
        self.render_volumes()
        self.render_scan_summary()
        self.render_roots()
        self.render_top_folders()
        self.render_categories()
        self.render_top_files()
        self.render_old_files()
        self.render_extensions()
        self.render_ages()
        self.render_duplicates()
        self.render_footnotes()

    def render_header(self) -> None:
        self.line()
        self.line(self.c("=" * self.width, "dim"))
        title = "DISK ANALYZER"
        subtitle = "read-only inventory - nothing on disk is modified"
        self.line(self.c(f" {title}  ", "bold") + self.c(subtitle, "dim"))
        self.line(self.c("=" * self.width, "dim"))

    def render_volumes(self) -> None:
        if not self.result.volumes:
            return
        self.line()
        for root, (total, used, free) in self.result.volumes.items():
            share = used / total if total else 0.0
            gauge = _bar(share, 34)
            style = "red" if free < 20 * 1024**3 else "yellow" if free < 60 * 1024**3 else "green"
            self.line(
                f" {root:<4} {human_size(used)} used of {human_size(total)}"
                f"   [{self.c(gauge, style)}] {share * 100:.0f}%"
            )
            self.line(
                "      " + self.c(f"{human_size(free)} free", style, "bold")
            )

    def render_scan_summary(self) -> None:
        result = self.result
        scanned_share = result.total_size / max(1, sum(u for _t, u, _f in result.volumes.values()))
        self.line()
        self.line(
            f" Scanned  {human_count(result.total_files)} files in "
            f"{human_count(result.total_dirs)} folders"
            f"  =  {self.c(human_size(result.total_size), 'bold')}"
            f"  ({scanned_share * 100:.0f}% of used space)"
        )
        self.line(f" Time     {human_duration(result.elapsed)}")

        if result.skipped:
            skipped_note = ", ".join(
                sorted({reason for _path, reason in result.skipped})
            )
            self.line(
                f" Skipped  {len(result.skipped)} paths ({skipped_note})"
                + self.c("  --include-os to scan them", "dim")
            )
        if result.reparse_points:
            self.line(
                f" Links    {len(result.reparse_points)} junctions/symlinks not"
                " followed, so nothing is counted twice"
            )
        if result.errors:
            self.line(
                self.c(
                    f" Errors   {len(result.errors)} paths unreadable"
                    " (run as Administrator to see more)",
                    "dim",
                )
            )

    def render_roots(self) -> None:
        self.heading("WHERE THE SPACE IS")
        for root, children in self.report.root_breakdown:
            total = root.total_size or 1
            self.line(f" {root.display}  {self.c(human_size(root.total_size), 'bold')}")
            for node in children[:12]:
                share = node.total_size / total
                self.line(
                    f"   {human_size(node.total_size):>11}  {share * 100:5.1f}%  "
                    f"{self.c(_bar(share, 18), 'cyan')}  {node.name}"
                )
            if len(children) > 12:
                rest = sum(n.total_size for n in children[12:])
                self.note(f"{human_size(rest):>11}         + {len(children) - 12} more folders")

    def render_top_folders(self) -> None:
        self.heading("LARGEST FOLDERS")
        self.note("chains collapsed: each row is the deepest folder that still holds the whole total")
        self.line()
        path_width = max(40, self.width - 34)
        for node in self.report.top_folders:
            label = categories.CATEGORY_BY_KEY.get(node.category or "other")
            tag = ""
            if label is not None and label.confidence != "info":
                tag = self.c(f" [{label.key}]", CONFIDENCE_STYLE[label.confidence])
            self.line(
                f" {human_size(node.total_size):>11}  {human_count(node.total_files):>9} files  "
                f"{truncate_middle(node.display, path_width)}{tag}"
            )

    def render_categories(self) -> None:
        self.heading("WHAT IT IS")
        rows = self.report.category_rows
        if not rows:
            return
        label_width = max(len(row[0].label) for row in rows)
        for category, count, size, share in rows:
            style = CONFIDENCE_STYLE[category.confidence]
            self.line(
                f" {human_size(size):>11}  {share * 100:5.1f}%  "
                f"{self.c(category.label.ljust(label_width), style)}  "
                f"{self.c(_bar(share, 14), style)}  "
                f"{self.c(category.hint, 'dim')}"
            )

        reclaimable = self.report.reclaimable
        self.line()
        self.line(
            " " + self.c("Regenerable", "green", "bold")
            + f"  {human_size(reclaimable['safe'])}"
            + self.c("   caches and build output that rebuild themselves", "dim")
        )
        self.line(
            " " + self.c("Worth review", "yellow", "bold")
            + f" {human_size(reclaimable['review'])}"
            + self.c("   often unwanted, but only you can say", "dim")
        )
        if reclaimable.get("duplicates"):
            self.line(
                " " + self.c("Duplicated", "cyan", "bold")
                + f"  {human_size(reclaimable['duplicates'])}"
                + self.c("   extra copies beyond the first", "dim")
            )
        self.line()
        self.note("DiskAnalyzer never deletes. These are places to look, not instructions.")

    def render_top_files(self) -> None:
        self.heading("LARGEST FILES")
        path_width = max(40, self.width - 30)
        for size, path, mtime in self.report.top_files:
            self.line(
                f" {human_size(size):>11}  {human_age(mtime):>6}  "
                f"{truncate_middle(path, path_width)}"
            )

    def render_old_files(self) -> None:
        if not self.report.old_files:
            return
        config = self.result.config
        self.heading(
            f"BIG AND UNTOUCHED  (over {human_size(config.old_file_min_size)}, "
            f"not modified in {config.old_file_min_days // 365 or 1}+ year)"
        )
        self.note("large files you have not written to in a long time: the best place to start")
        self.line()
        path_width = max(40, self.width - 30)
        for size, path, mtime in self.report.old_files:
            # Pad before coloring: escape codes are invisible but still count
            # toward a format spec's width.
            age = self.c(human_age(mtime).rjust(6), "yellow")
            self.line(
                f" {human_size(size):>11}  {age}  {truncate_middle(path, path_width)}"
            )

    def render_extensions(self) -> None:
        self.heading("BY FILE TYPE")
        for ext, count, size, share in self.report.extension_rows[:15]:
            self.line(
                f" {human_size(size):>11}  {share * 100:5.1f}%  "
                f"{ext:<16} {human_count(count):>10} files"
            )

    def render_ages(self) -> None:
        self.heading("BY AGE (last modified)")
        total = self.result.total_size or 1
        for label, count, size in self.report.age_rows:
            share = size / total
            self.line(
                f" {human_size(size):>11}  {share * 100:5.1f}%  {label:<16} "
                f"{self.c(_bar(share, 20), 'blue')}  {human_count(count):>10} files"
            )

    def render_duplicates(self) -> None:
        groups = self.report.duplicates
        if not groups:
            return
        wasted = sum(g.wasted for g in groups)
        self.heading(f"LIKELY DUPLICATES  ({human_size(wasted)} in extra copies)")
        self.note("matched on exact size plus first and last 64 KB - strong evidence, not proof")
        self.line()
        path_width = max(40, self.width - 20)
        for group in groups[:12]:
            self.line(
                f" {human_size(group.size):>11} x {len(group.paths)}  "
                + self.c(f"({human_size(group.wasted)} recoverable)", "cyan")
            )
            for path in group.paths[:4]:
                self.line(f"             {truncate_middle(path, path_width)}")
            if len(group.paths) > 4:
                self.note(f"          + {len(group.paths) - 4} more copies")
        if len(groups) > 12:
            self.note(f"+ {len(groups) - 12} more duplicate groups (see the HTML report)")
        if self.report.duplicates_truncated:
            self.note("duplicate search stopped early at its read budget; more may exist")

    def render_footnotes(self) -> None:
        result = self.result
        if result.os_managed:
            self.heading("OS-MANAGED FILES  (sized by Windows, not by you)")
            for path, size in sorted(result.os_managed, key=lambda item: -item[1]):
                self.line(f" {human_size(size):>11}  {path}")
            self.note("adjust these through System settings, never by deleting them")

        self.line()
        self.line(self.c("-" * self.width, "dim"))
        self.line(
            self.c(" Nothing was modified, moved or deleted. ", "green")
            + self.c("Sizes are logical unless --allocated was used.", "dim")
        )
        self.line()


def render(report: Report, stream=None, color: bool | None = None) -> None:
    TerminalReport(report, stream=stream, color=color).render()
