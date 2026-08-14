"""Command-line entry point."""

from __future__ import annotations

import argparse
import contextlib
import os
import re
import sys
import time

from . import __version__, report_csv, report_html, report_terminal, safety
from .insights import build_report
from .scanner import ScanConfig, scan
from .util import human_count, human_size, supports_color

SIZE_PATTERN = re.compile(r"^\s*([\d.]+)\s*([kmgt]?b?)\s*$", re.IGNORECASE)
SIZE_UNITS = {"": 1, "b": 1, "k": 1024, "kb": 1024, "m": 1024**2, "mb": 1024**2,
              "g": 1024**3, "gb": 1024**3, "t": 1024**4, "tb": 1024**4}


def parse_size(text: str) -> int:
    """Accept 500, 500MB, 1.5g and friends."""
    match = SIZE_PATTERN.match(text)
    if not match:
        raise argparse.ArgumentTypeError(f"not a size: {text!r} (try 500MB or 2GB)")
    amount, unit = match.groups()
    factor = SIZE_UNITS.get(unit.lower())
    if factor is None:
        raise argparse.ArgumentTypeError(f"unknown size unit in {text!r}")
    return int(float(amount) * factor)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="diskanalyzer",
        description=(
            "Read-only inventory of what is using space on a drive. "
            "Skips the live Windows installation by default. "
            "This tool cannot delete, move or modify anything."
        ),
        epilog="Nothing is ever removed. Reports tell you where to look; you decide.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("roots", nargs="*", default=None,
                        help="paths to scan (default: C:\\)")
    parser.add_argument("--version", action="version", version=f"DiskAnalyzer {__version__}")

    scope = parser.add_argument_group("scope")
    scope.add_argument("--include-os", action="store_true",
                       help="also scan C:\\Windows and other live OS folders")
    scope.add_argument("--exclude", action="append", default=[], metavar="PATH",
                       help="skip this folder (repeatable)")
    scope.add_argument("--exclude-name", action="append", default=[], metavar="NAME",
                       help="skip any folder with this name (repeatable)")

    sizing = parser.add_argument_group("sizing")
    sizing.add_argument("--allocated", action="store_true",
                        help="estimate on-disk size by rounding up to whole clusters")
    sizing.add_argument("--cluster-size", type=int, default=4096, metavar="BYTES",
                        help="cluster size for --allocated (default: 4096)")

    detail = parser.add_argument_group("detail")
    detail.add_argument("--top", type=int, default=25, metavar="N",
                        help="folders per ranking (default: 25)")
    detail.add_argument("--files", type=int, default=30, metavar="N",
                        help="files per ranking (default: 30)")
    detail.add_argument("--old-min-size", type=parse_size, default="100MB", metavar="SIZE",
                        help="size floor for the untouched-files list (default: 100MB)")
    detail.add_argument("--old-min-days", type=int, default=365, metavar="DAYS",
                        help="age floor for the untouched-files list (default: 365)")
    detail.add_argument("--duplicates", action="store_true",
                        help="look for duplicate files (reads 128KB per candidate)")
    detail.add_argument("--dup-min-size", type=parse_size, default="25MB", metavar="SIZE",
                        help="ignore duplicates smaller than this (default: 25MB)")

    output = parser.add_argument_group("output")
    output.add_argument("--out", default=None, metavar="DIR",
                        help="report directory (default: ./reports/scan-<timestamp>)")
    output.add_argument("--csv-min-size", type=parse_size, default="10MB", metavar="SIZE",
                        help="size floor for the CSV file listing; 0 lists every file "
                             "(default: 10MB)")
    output.add_argument("--folder-min-size", type=parse_size, default="1MB", metavar="SIZE",
                        help="size floor for the CSV folder listing; 0 lists every "
                             "folder (default: 1MB)")
    output.add_argument("--no-html", action="store_true", help="skip the HTML report")
    output.add_argument("--no-csv", action="store_true", help="skip the CSV listings")
    output.add_argument("--no-color", action="store_true", help="disable ANSI color")
    output.add_argument("--quiet", action="store_true", help="suppress progress output")

    parser.add_argument("--verify-read-only", action="store_true",
                        help="audit this tool's own source for destructive calls and exit")
    return parser


class Progress:
    """A single rewritten status line, so a long scan does not scroll away."""

    def __init__(self, enabled: bool):
        self.enabled = enabled
        self.last = 0.0
        self.width = 0

    def __call__(self, result, current_path: str) -> None:
        if not self.enabled:
            return
        now = time.time()
        if current_path and now - self.last < 0.15:
            return
        self.last = now
        text = (
            f"  scanning  {human_count(result.total_files):>11} files"
            f"  {human_size(result.total_size):>10}  {current_path[:58]}"
        )
        pad = max(0, self.width - len(text))
        self.width = max(self.width, len(text))
        sys.stderr.write("\r" + text + " " * pad)
        sys.stderr.flush()

    def done(self) -> None:
        if self.enabled:
            sys.stderr.write("\r" + " " * self.width + "\r")
            sys.stderr.flush()


def default_output_dir() -> str:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    return os.path.join(os.getcwd(), "reports", f"scan-{stamp}")


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    # Before touching the disk, prove this build cannot damage it.
    violations = safety.audit_source()
    if args.verify_read_only:
        if violations:
            print("READ-ONLY AUDIT FAILED\n")
            for violation in violations:
                print("  " + violation.describe())
            return 1
        print("Read-only audit passed: no destructive filesystem calls in the source.")
        return 0
    if violations:
        print("Refusing to run: this build could modify files.", file=sys.stderr)
        for violation in violations:
            print("  " + violation.describe(), file=sys.stderr)
        return 1

    roots = tuple(args.roots) if args.roots else ("C:" + os.sep,)
    missing = [root for root in roots if not os.path.isdir(root)]
    if missing:
        print(f"Not a folder: {', '.join(missing)}", file=sys.stderr)
        return 2

    config = ScanConfig(
        roots=roots,
        include_os=args.include_os,
        exclude_dirs=tuple(args.exclude),
        exclude_names=tuple(args.exclude_name),
        use_allocated=args.allocated,
        cluster_size=args.cluster_size,
        old_file_min_size=args.old_min_size,
        old_file_min_days=args.old_min_days,
        find_duplicates=args.duplicates,
        duplicate_min_size=args.dup_min_size,
    )

    out_dir = args.out or default_output_dir()
    want_files = not args.no_csv
    if want_files or not args.no_html:
        os.makedirs(out_dir, exist_ok=True)

    progress = Progress(enabled=not args.quiet and sys.stderr.isatty())
    if not args.quiet:
        print(f"Scanning {', '.join(roots)} ... (read-only)", file=sys.stderr)

    with contextlib.ExitStack() as stack:
        sink = None
        if want_files:
            writer = stack.enter_context(
                report_csv.FileListWriter(
                    os.path.join(out_dir, "files.csv"), min_size=args.csv_min_size
                )
            )
            sink = writer.write

        try:
            result = scan(
                config,
                progress=progress,
                file_sink=sink,
                file_sink_min_size=args.csv_min_size,
            )
        except KeyboardInterrupt:
            progress.done()
            print("\nScan cancelled. Nothing was modified.", file=sys.stderr)
            return 130

    progress.done()

    report = build_report(
        result, top_n=args.top, file_n=args.files, with_duplicates=args.duplicates
    )

    color = None if not args.no_color else False
    report_terminal.render(report, color=color)

    written = []
    if want_files:
        written.append(("files.csv", os.path.join(out_dir, "files.csv")))
        folders_path = os.path.join(out_dir, "folders.csv")
        report_csv.write_folder_csv(result, folders_path, min_size=args.folder_min_size)
        written.append(("folders.csv", folders_path))
        if result.errors:
            errors_path = os.path.join(out_dir, "unreadable.csv")
            report_csv.write_error_csv(result, errors_path)
            written.append(("unreadable.csv", errors_path))

    if not args.no_html:
        html_path = os.path.join(out_dir, "report.html")
        report_html.write_html(report, html_path)
        written.append(("report.html", html_path))

    if written:
        tint = supports_color(sys.stdout) and not args.no_color
        bold = "\033[1m" if tint else ""
        reset = "\033[0m" if tint else ""
        print(f" Reports written to {bold}{out_dir}{reset}")
        for label, path in written:
            try:
                size = os.path.getsize(path)
            except OSError:
                size = 0
            print(f"   {label:<16} {human_size(size):>10}")
        print()
        print("   Open report.html in a browser for the interactive treemap.")
        if want_files and args.csv_min_size:
            print(
                f"   files.csv lists files over {human_size(args.csv_min_size)}"
                f" and folders.csv folders over {human_size(args.folder_min_size)};"
                " pass --csv-min-size 0 --folder-min-size 0 for everything."
            )
        print()

    return 0
