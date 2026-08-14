"""The scan engine: one read-only walk of the volume.

Design notes
------------
*Single pass.* Every statistic in the report is accumulated during one walk.
Nothing is computed by re-reading the disk afterwards.

*Bounded memory.* A full C: drive can hold millions of files; keeping a record
per file would cost gigabytes. Instead the walk keeps one node per *directory*
(a few hundred thousand at most) and streams file-level data into fixed-size
structures: a top-N heap, an extension histogram, and age buckets.

*Cheap stat.* On Windows the directory enumeration already returns size and
timestamps, so ``DirEntry.stat(follow_symlinks=False)`` costs no extra syscall.
Following a symlink would cost one, which is another reason not to.

*No reparse traversal.* Junctions, symlinks and mounted volumes are recorded
but never descended into. Following them would double-count (``AppData\\Local\\
Application Data`` loops back on itself) or silently wander onto another disk.
"""

from __future__ import annotations

import heapq
import os
import shutil
import time
from dataclasses import dataclass, field
from typing import Callable

from . import categories
from .exclusions import ExclusionSet
from .util import round_to_cluster, to_display_path, to_extended_path

FILE_ATTRIBUTE_REPARSE_POINT = 0x400

# Age buckets, in days. The final entry is the open-ended tail.
AGE_BUCKETS: tuple[tuple[int | None, str], ...] = (
    (30, "under 1 month"),
    (90, "1-3 months"),
    (365, "3-12 months"),
    (730, "1-2 years"),
    (1825, "2-5 years"),
    (None, "over 5 years"),
)

SECONDS_PER_DAY = 86400.0


@dataclass
class ScanConfig:
    roots: tuple[str, ...] = ("C:" + os.sep,)
    include_os: bool = False
    exclude_dirs: tuple[str, ...] = ()
    exclude_names: tuple[str, ...] = ()
    use_allocated: bool = False
    cluster_size: int = 4096
    top_files: int = 2000
    old_file_min_size: int = 100 * 1024 * 1024
    old_file_min_days: int = 365
    find_duplicates: bool = False
    duplicate_min_size: int = 25 * 1024 * 1024


class DirNode:
    """One directory. ``own_*`` counts files directly inside; ``total_*``
    includes every descendant and is filled in after the walk."""

    __slots__ = (
        "path", "name", "parent", "depth", "category",
        "own_size", "own_files", "total_size", "total_files", "total_dirs",
    )

    def __init__(self, path: str, name: str, parent, depth: int, category):
        self.path = path
        self.name = name
        self.parent = parent
        self.depth = depth
        self.category = category
        self.own_size = 0
        self.own_files = 0
        self.total_size = 0
        self.total_files = 0
        self.total_dirs = 0

    @property
    def display(self) -> str:
        return to_display_path(self.path)


@dataclass
class ScanResult:
    config: ScanConfig
    started_at: float = 0.0
    elapsed: float = 0.0
    nodes: list = field(default_factory=list)
    root_nodes: list = field(default_factory=list)
    total_files: int = 0
    total_dirs: int = 0
    total_size: int = 0
    logical_size: int = 0
    ext_stats: dict = field(default_factory=dict)
    cat_stats: dict = field(default_factory=dict)
    age_stats: dict = field(default_factory=dict)
    top_files: list = field(default_factory=list)
    old_large_files: list = field(default_factory=list)
    skipped: list = field(default_factory=list)
    os_managed: list = field(default_factory=list)
    reparse_points: list = field(default_factory=list)
    errors: list = field(default_factory=list)
    volumes: dict = field(default_factory=dict)
    dup_candidates: dict = field(default_factory=dict)

    def sorted_top_files(self) -> list:
        """Largest first, as (size, path, mtime) triples."""
        return [
            (size, path, mtime)
            for size, _seq, path, mtime in sorted(self.top_files, reverse=True)
        ]

    def sorted_old_files(self) -> list:
        return [
            (size, path, mtime)
            for size, _seq, path, mtime in sorted(self.old_large_files, reverse=True)
        ]


def _age_label(mtime: float, now: float) -> str:
    days = (now - mtime) / SECONDS_PER_DAY
    for limit, label in AGE_BUCKETS:
        if limit is None or days < limit:
            return label
    return AGE_BUCKETS[-1][1]


def _is_volume_root(display_path: str) -> bool:
    stripped = display_path.rstrip("\\/")
    return len(stripped) == 2 and stripped[1] == ":"


def scan(
    config: ScanConfig,
    progress: Callable[[ScanResult, str], None] | None = None,
    file_sink: Callable[[str, int, float, str], None] | None = None,
    file_sink_min_size: int = 0,
) -> ScanResult:
    """Walk every configured root and return the accumulated statistics.

    ``file_sink`` receives ``(path, size, mtime, category)`` for every file at
    or above ``file_sink_min_size`` as it is encountered. Streaming the full
    listing out during the walk is what keeps a complete per-file inventory
    possible without ever holding millions of records in memory.
    """
    result = ScanResult(config=config)
    result.started_at = time.time()
    now = result.started_at

    exclusions = ExclusionSet(
        include_os=config.include_os,
        extra_dirs=config.exclude_dirs,
        extra_names=config.exclude_names,
    )

    # Local aliases: this loop runs millions of times, and attribute lookups
    # on hot paths are the difference between a 90-second and a 3-minute scan.
    ext_stats = result.ext_stats
    cat_stats = result.cat_stats
    age_stats = result.age_stats
    top_heap = result.top_files
    top_limit = config.top_files
    use_allocated = config.use_allocated
    cluster = config.cluster_size
    old_min_size = config.old_file_min_size
    old_min_age = config.old_file_min_days * SECONDS_PER_DAY
    want_dups = config.find_duplicates
    dup_min = config.duplicate_min_size
    dup_map = result.dup_candidates
    match_extension = categories.match_extension
    resolve_category = categories.resolve_category
    splitext = os.path.splitext
    heappush = heapq.heappush
    heappushpop = heapq.heappushpop

    seq = 0                 # tie-breaker so heap comparisons never touch paths
    report_every = 4000     # files between progress callbacks
    since_report = 0

    for root in config.roots:
        # Key by volume, not by scan root: two roots on the same drive share
        # one set of free-space numbers, and the gauge should say "C:\" rather
        # than repeating whichever subfolder was scanned.
        drive = os.path.splitdrive(os.path.abspath(root))[0] or root
        volume = drive + os.sep if drive.endswith(":") else drive
        if volume not in result.volumes:
            try:
                usage = shutil.disk_usage(root)
                result.volumes[volume] = (usage.total, usage.used, usage.free)
            except OSError as exc:
                result.errors.append((root, f"disk_usage failed: {exc}"))

        ext_root = to_extended_path(root)
        root_display = to_display_path(ext_root)
        root_node = DirNode(
            ext_root,
            root_display,
            None,
            0,
            resolve_category(None, root_display.lower().replace("\\", "/"), ""),
        )
        result.root_nodes.append(root_node)
        result.nodes.append(root_node)

        stack = [root_node]
        while stack:
            node = stack.pop()
            node_path = node.path
            node_category = node.category
            node_display = to_display_path(node_path)
            at_volume_root = _is_volume_root(node_display)

            try:
                iterator = os.scandir(node_path)
            except OSError as exc:
                result.errors.append((node_display, str(exc)))
                continue

            own_size = 0
            own_files = 0

            with iterator:
                while True:
                    try:
                        entry = next(iterator)
                    except StopIteration:
                        break
                    except OSError as exc:
                        # A directory can become unreadable mid-enumeration.
                        result.errors.append((node_display, str(exc)))
                        break

                    try:
                        info = entry.stat(follow_symlinks=False)
                    except OSError as exc:
                        result.errors.append((entry.path, str(exc)))
                        continue

                    attributes = getattr(info, "st_file_attributes", 0)
                    if attributes & FILE_ATTRIBUTE_REPARSE_POINT:
                        # Junction, symlink or mount point: record and move on.
                        result.reparse_points.append(to_display_path(entry.path))
                        continue

                    try:
                        entry_is_dir = entry.is_dir(follow_symlinks=False)
                    except OSError:
                        entry_is_dir = False

                    name = entry.name

                    if entry_is_dir:
                        child_display = to_display_path(entry.path)
                        reason = exclusions.reason_for(child_display, name)
                        if reason is not None:
                            result.skipped.append((child_display, reason))
                            continue

                        child_category = resolve_category(
                            node_category,
                            child_display.lower().replace("\\", "/"),
                            name.lower(),
                        )
                        child = DirNode(
                            entry.path, name, node, node.depth + 1, child_category
                        )
                        result.nodes.append(child)
                        stack.append(child)
                        continue

                    # ---- regular file ------------------------------------
                    logical = info.st_size
                    if exclusions.is_os_managed_file(name, at_volume_root):
                        result.os_managed.append((to_display_path(entry.path), logical))
                        continue

                    size = round_to_cluster(logical, cluster) if use_allocated else logical
                    own_size += size
                    own_files += 1
                    result.logical_size += logical

                    ext = splitext(name)[1].lower() or "(no extension)"
                    bucket = ext_stats.get(ext)
                    if bucket is None:
                        ext_stats[ext] = [1, size]
                    else:
                        bucket[0] += 1
                        bucket[1] += size

                    category = node_category or match_extension(ext, size) or "other"
                    bucket = cat_stats.get(category)
                    if bucket is None:
                        cat_stats[category] = [1, size]
                    else:
                        bucket[0] += 1
                        bucket[1] += size

                    mtime = info.st_mtime
                    label = _age_label(mtime, now)
                    bucket = age_stats.get(label)
                    if bucket is None:
                        age_stats[label] = [1, size]
                    else:
                        bucket[0] += 1
                        bucket[1] += size

                    seq += 1
                    record = (size, seq, to_display_path(entry.path), mtime)

                    if file_sink is not None and size >= file_sink_min_size:
                        file_sink(record[2], size, mtime, category)

                    if len(top_heap) < top_limit:
                        heappush(top_heap, record)
                    elif size > top_heap[0][0]:
                        heappushpop(top_heap, record)

                    if size >= old_min_size and (now - mtime) >= old_min_age:
                        result.old_large_files.append(record)

                    if want_dups and size >= dup_min:
                        dup_map.setdefault(size, []).append(record[2])

                    since_report += 1

            node.own_size = own_size
            node.own_files = own_files
            result.total_files += own_files
            result.total_size += own_size

            if progress is not None and since_report >= report_every:
                since_report = 0
                progress(result, node_display)

    result.total_dirs = len(result.nodes)
    _roll_up(result.nodes)
    result.elapsed = time.time() - result.started_at

    if progress is not None:
        progress(result, "")
    return result


def _roll_up(nodes: list) -> None:
    """Propagate per-directory totals up to their parents.

    Sorting by depth descending guarantees a child is finished before its
    parent is touched, which turns the rollup into a single linear pass with no
    recursion and no risk of blowing the stack on a deep tree.
    """
    for node in nodes:
        node.total_size = node.own_size
        node.total_files = node.own_files
        node.total_dirs = 0

    for node in sorted(nodes, key=lambda n: n.depth, reverse=True):
        parent = node.parent
        if parent is not None:
            parent.total_size += node.total_size
            parent.total_files += node.total_files
            parent.total_dirs += node.total_dirs + 1
