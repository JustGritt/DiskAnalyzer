"""CSV output: the full inventory, for sorting and filtering in a spreadsheet.

The terminal and HTML reports answer the common questions. This file exists for
the uncommon one, the moment you want to sort every file you own by size, or
filter to one extension, or hand the list to another tool. Rows are written as
the walk encounters them, so the listing can be complete without the scan ever
holding every file in memory.

These writers are the only place in the package that opens a file for writing,
and they only ever write to the output directory you chose.
"""

from __future__ import annotations

import csv
import datetime as _datetime
import os

from . import categories
from .scanner import ScanResult
from .util import human_size

FILE_COLUMNS = (
    "size_bytes",
    "size",
    "modified",
    "category",
    "confidence",
    "extension",
    "folder",
    "name",
    "full_path",
)

FOLDER_COLUMNS = (
    "total_bytes",
    "total_size",
    "files_here_bytes",
    "files_here",
    "files_total",
    "subfolders",
    "depth",
    "category",
    "full_path",
)


def _timestamp(epoch: float) -> str:
    try:
        return _datetime.datetime.fromtimestamp(epoch).strftime("%Y-%m-%d %H:%M")
    except (OSError, OverflowError, ValueError):
        return ""


class FileListWriter:
    """Streams one CSV row per file as the scan produces it."""

    def __init__(self, path: str, min_size: int = 0):
        self.path = path
        self.min_size = min_size
        self.rows_written = 0
        self._handle = None
        self._writer = None

    def __enter__(self) -> "FileListWriter":
        # newline="" is required or csv emits blank lines between rows on Windows.
        self._handle = open(self.path, "w", encoding="utf-8-sig", newline="")
        self._writer = csv.writer(self._handle)
        self._writer.writerow(FILE_COLUMNS)
        return self

    def __exit__(self, *_exc) -> None:
        if self._handle is not None:
            self._handle.close()
            self._handle = None

    def write(self, full_path: str, size: int, mtime: float, category: str) -> None:
        if size < self.min_size or self._writer is None:
            return
        folder, name = os.path.split(full_path)
        meta = categories.CATEGORY_BY_KEY.get(category)
        self._writer.writerow(
            (
                size,
                human_size(size),
                _timestamp(mtime),
                meta.label if meta else category,
                meta.confidence if meta else "",
                os.path.splitext(name)[1].lower(),
                folder,
                name,
                full_path,
            )
        )
        self.rows_written += 1


def write_folder_csv(result: ScanResult, path: str, min_size: int = 0) -> int:
    """One row per folder, with both its own bytes and its subtree total."""
    written = 0
    with open(path, "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(FOLDER_COLUMNS)
        nodes = sorted(result.nodes, key=lambda n: n.total_size, reverse=True)
        for node in nodes:
            if node.total_size < min_size:
                continue
            meta = categories.CATEGORY_BY_KEY.get(node.category or "other")
            writer.writerow(
                (
                    node.total_size,
                    human_size(node.total_size),
                    node.own_size,
                    node.own_files,
                    node.total_files,
                    node.total_dirs,
                    node.depth,
                    meta.label if meta else "",
                    node.display,
                )
            )
            written += 1
    return written


def write_error_csv(result: ScanResult, path: str) -> int:
    """Paths the scan could not read, so the totals can be read honestly."""
    with open(path, "w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(("path", "reason"))
        for entry_path, reason in result.errors:
            writer.writerow((entry_path, reason))
    return len(result.errors)
