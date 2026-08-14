"""Turning raw scan totals into the handful of facts worth acting on."""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field

from . import categories
from .safety import open_for_read
from .scanner import AGE_BUCKETS, ScanResult

# When a folder holds this much of its parent, the parent tells you nothing the
# child did not already say, so only the child is worth listing.
CHAIN_COLLAPSE_RATIO = 0.90


@dataclass
class DuplicateGroup:
    size: int
    paths: list

    @property
    def wasted(self) -> int:
        """Bytes recoverable if all but one copy were removed."""
        return self.size * (len(self.paths) - 1)


@dataclass
class Report:
    result: ScanResult
    top_folders: list = field(default_factory=list)
    hotspots: list = field(default_factory=list)
    root_breakdown: list = field(default_factory=list)
    category_rows: list = field(default_factory=list)
    extension_rows: list = field(default_factory=list)
    age_rows: list = field(default_factory=list)
    top_files: list = field(default_factory=list)
    old_files: list = field(default_factory=list)
    duplicates: list = field(default_factory=list)
    duplicates_truncated: bool = False
    reclaimable: dict = field(default_factory=dict)


def _max_child_size(result: ScanResult) -> dict:
    """Largest total_size among each node's direct children, keyed by id()."""
    biggest: dict = {}
    for node in result.nodes:
        parent = node.parent
        if parent is None:
            continue
        key = id(parent)
        if node.total_size > biggest.get(key, 0):
            biggest[key] = node.total_size
    return biggest


def collapse_chains(result: ScanResult, limit: int) -> list:
    """Rank folders by total size, keeping only the point where size fans out.

    A path like Users > Gritt > Desktop > Current > Grimdark can hold the same
    12 GB at every level. Listing all five wastes the report on one fact. A node
    is kept only when no single child accounts for nearly all of it, so the
    listed folder is the deepest one that still explains the whole total.
    """
    biggest = _max_child_size(result)
    interesting = [
        node
        for node in result.nodes
        if node.total_size > 0
        and biggest.get(id(node), 0) < node.total_size * CHAIN_COLLAPSE_RATIO
    ]
    interesting.sort(key=lambda n: n.total_size, reverse=True)
    return interesting[:limit]


def find_duplicates(
    result: ScanResult,
    max_groups: int = 400,
    read_budget: int = 8 * 1024 * 1024 * 1024,
) -> tuple[list, bool]:
    """Identify same-size files whose head and tail bytes also match.

    Files are only ever opened for reading, and only the first and last 64 KB
    of each are read, so a group of 40 GB virtual disks costs a few megabytes
    of I/O rather than 40 GB. Matching head, tail and exact size is strong
    evidence of a duplicate but not proof; the report says "likely" for that
    reason and never acts on the finding.
    """
    chunk = 64 * 1024
    groups: list = []
    truncated = False
    spent = 0

    candidates = sorted(
        ((size, paths) for size, paths in result.dup_candidates.items() if len(paths) > 1),
        key=lambda item: item[0] * (len(item[1]) - 1),
        reverse=True,
    )

    for size, paths in candidates:
        if len(groups) >= max_groups or spent >= read_budget:
            truncated = True
            break

        by_signature: dict = {}
        for path in paths:
            try:
                with open_for_read(path) as handle:
                    head = handle.read(chunk)
                    if size > chunk * 2:
                        handle.seek(-chunk, 2)
                        tail = handle.read(chunk)
                    else:
                        tail = b""
            except OSError as exc:
                result.errors.append((path, f"duplicate check: {exc}"))
                continue

            spent += len(head) + len(tail)
            digest = hashlib.blake2b(head + tail, digest_size=16).hexdigest()
            by_signature.setdefault(digest, []).append(path)

        for matched in by_signature.values():
            if len(matched) > 1:
                groups.append(DuplicateGroup(size=size, paths=sorted(matched)))

    groups.sort(key=lambda g: g.wasted, reverse=True)
    return groups, truncated


def build_report(
    result: ScanResult,
    top_n: int = 25,
    file_n: int = 30,
    with_duplicates: bool = False,
) -> Report:
    report = Report(result=result)

    report.top_folders = collapse_chains(result, top_n)

    hotspots = [n for n in result.nodes if n.own_size > 0]
    hotspots.sort(key=lambda n: n.own_size, reverse=True)
    report.hotspots = hotspots[:top_n]

    for root in result.root_nodes:
        children = [n for n in result.nodes if n.parent is root and n.total_size > 0]
        children.sort(key=lambda n: n.total_size, reverse=True)
        report.root_breakdown.append((root, children))

    total = result.total_size or 1

    rows = []
    for key, (count, size) in result.cat_stats.items():
        category = categories.CATEGORY_BY_KEY.get(key)
        if category is None:
            continue
        rows.append((category, count, size, size / total))
    rows.sort(key=lambda row: row[2], reverse=True)
    report.category_rows = rows

    ext_rows = [
        (ext, count, size, size / total)
        for ext, (count, size) in result.ext_stats.items()
    ]
    ext_rows.sort(key=lambda row: row[2], reverse=True)
    report.extension_rows = ext_rows

    report.age_rows = [
        (label, *result.age_stats.get(label, (0, 0)))
        for _limit, label in AGE_BUCKETS
    ]

    report.top_files = result.sorted_top_files()[:file_n]
    report.old_files = result.sorted_old_files()[:file_n]

    if with_duplicates:
        report.duplicates, report.duplicates_truncated = find_duplicates(result)

    reclaimable: dict = {"safe": 0, "review": 0, "info": 0}
    for category, _count, size, _share in rows:
        reclaimable[category.confidence] += size
    reclaimable["duplicates"] = sum(g.wasted for g in report.duplicates)
    report.reclaimable = reclaimable

    return report


def build_tree(result: ScanResult, min_size: int, max_nodes: int = 3000) -> dict | None:
    """A nested structure for the HTML treemap, pruned to what is visible.

    Anything below ``min_size`` cannot render as more than a sliver, so it is
    folded into its parent's own-bytes rather than shipped to the browser.
    """
    kept = [n for n in result.nodes if n.total_size >= min_size]
    if not kept:
        return None
    if len(kept) > max_nodes:
        kept.sort(key=lambda n: n.total_size, reverse=True)
        kept = kept[:max_nodes]

    keep_ids = {id(n) for n in kept}
    children: dict = {}
    for node in kept:
        parent = node.parent
        if parent is not None and id(parent) in keep_ids:
            children.setdefault(id(parent), []).append(node)

    def emit(node) -> dict:
        kids = [emit(child) for child in children.get(id(node), ())]
        kids.sort(key=lambda item: item["size"], reverse=True)
        shown = sum(item["size"] for item in kids)
        payload = {
            "name": node.name,
            "path": node.display,
            "size": node.total_size,
            "files": node.total_files,
            "cat": node.category or "other",
        }
        # Bytes not covered by a listed child: files directly here plus every
        # subfolder too small to draw. Without this the treemap would not add up.
        remainder = node.total_size - shown
        if kids:
            payload["children"] = kids
            if remainder > 0:
                payload["rest"] = remainder
        return payload

    roots = [emit(root) for root in result.root_nodes if root.total_size > 0]
    if not roots:
        return None
    if len(roots) == 1:
        return roots[0]
    return {
        "name": "Scanned roots",
        "path": "",
        "size": sum(item["size"] for item in roots),
        "files": result.total_files,
        "cat": "other",
        "children": roots,
    }
