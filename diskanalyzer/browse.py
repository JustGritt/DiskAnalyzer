"""An index that makes a finished scan navigable.

The scan keeps one node per directory with a pointer to its *parent*, which is
all a static report needs: totals roll upward and the walk is over. An
interactive view asks the opposite question - "what is inside this folder?" -
and answering that by filtering every node on each click would cost a pass over
a few hundred thousand nodes per interaction. Inverting the parent pointers once,
after the walk, turns every navigation step into a dictionary lookup.

The sunburst also needs something the report never did: a subtree pruned to what
can actually be *seen*. A ring segment thinner than a hairline is not
information, it is noise that costs bandwidth and draw time, so anything under a
fraction of the folder being viewed is folded into a single remainder slice. The
remainder is emitted rather than dropped, because a ring that does not add up to
its parent is a lie about where the space went.
"""

from __future__ import annotations

import os

from .scanner import ScanResult
from .util import normalize_key

# Share of the folder in view below which a child is not worth sending. Matched
# to the angle the UI refuses to draw below, so the cut happens here, once, on
# the numbers, instead of the browser receiving slices it will silently discard.
MIN_VISIBLE_SHARE = 0.001

# Hard ceiling on slices per ring, for the pathological case of a folder with
# thousands of similarly sized children.
MAX_SLICES = 140

# How many rings deep a single response reaches. Five is about where segments
# stop being clickable anyway.
DEFAULT_DEPTH = 5


class ScanIndex:
    """Child lists, per-folder file lists and path lookup for one scan."""

    def __init__(self, result: ScanResult) -> None:
        self.result = result
        self._children: dict[int, list] = {}
        self._by_key: dict[str, object] = {}
        self._files: dict[str, list] = {}
        self._build()

    # -- construction ------------------------------------------------------
    def _build(self) -> None:
        for node in self.result.nodes:
            self._by_key[normalize_key(node.display)] = node
            parent = node.parent
            if parent is not None:
                self._children.setdefault(id(parent), []).append(node)

        for kids in self._children.values():
            kids.sort(key=lambda n: n.total_size, reverse=True)

        # The walk never kept a record per file, but it did keep the largest
        # few thousand. Grouping those by their folder lets the map show a
        # single 40 GB disk image as its own slice instead of burying it in an
        # anonymous "files in this folder" remainder, which is usually the one
        # thing the user came to find.
        for size, path, mtime in self.result.sorted_top_files():
            parent = os.path.dirname(path)
            self._files.setdefault(normalize_key(parent), []).append((size, path, mtime))

    # -- lookup ------------------------------------------------------------
    def node_for(self, display_path: str):
        return self._by_key.get(normalize_key(display_path))

    def children(self, node) -> list:
        return self._children.get(id(node), [])

    def files_in(self, node) -> list:
        return self._files.get(normalize_key(node.display), [])

    def roots(self) -> list:
        return list(self.result.root_nodes)

    def contains(self, display_path: str) -> bool:
        """True if the path is a folder this scan actually visited.

        Every path the UI can act on has to come from the scan itself. That is
        what keeps a request forged by something else on this machine from
        pointing the tool at an arbitrary location.
        """
        key = normalize_key(display_path)
        if key in self._by_key:
            return True
        return any(
            entry[1].lower() == display_path.lower()
            for entry in self._files.get(normalize_key(os.path.dirname(display_path)), ())
        )

    # -- payloads ----------------------------------------------------------
    def crumbs(self, node) -> list:
        trail = []
        current = node
        while current is not None:
            trail.append({"name": current.name, "path": current.display})
            current = current.parent
        trail.reverse()
        return trail

    def view(self, node, depth: int = DEFAULT_DEPTH) -> dict:
        """The node itself plus the rings drawn inside it."""
        floor = max(1, int(node.total_size * MIN_VISIBLE_SHARE))
        payload = self._emit(node, depth, floor)
        payload["crumbs"] = self.crumbs(node)
        payload["parent"] = node.parent.display if node.parent is not None else None
        payload["own"] = node.own_size
        payload["ownFiles"] = node.own_files
        return payload

    def _emit(self, node, depth: int, floor: int) -> dict:
        payload = {
            "name": node.name,
            "path": node.display,
            "size": node.total_size,
            "files": node.total_files,
            "dirs": node.total_dirs,
            "cat": node.category or "other",
            "kind": "dir",
        }

        kids = self.children(node)
        if depth <= 0:
            # Tell the UI something is down there even though this response
            # stops here, so the segment stays clickable rather than looking
            # like a dead end.
            payload["more"] = bool(kids)
            return payload

        slices: list = []
        shown = 0

        for child in kids:
            if child.total_size < floor:
                continue
            slices.append(self._emit(child, depth - 1, floor))
            shown += child.total_size

        for size, path, mtime in self.files_in(node):
            if size < floor:
                continue
            slices.append(
                {
                    "name": os.path.basename(path) or path,
                    "path": path,
                    "size": size,
                    "files": 1,
                    "cat": node.category or "other",
                    "kind": "file",
                    "mtime": mtime,
                }
            )
            shown += size

        slices.sort(key=lambda item: item["size"], reverse=True)
        if len(slices) > MAX_SLICES:
            for dropped in slices[MAX_SLICES:]:
                shown -= dropped["size"]
            del slices[MAX_SLICES:]

        remainder = node.total_size - shown
        if slices and remainder > 0:
            slices.append(
                {
                    "name": "everything else here",
                    "path": node.display,
                    "size": remainder,
                    "files": 0,
                    "cat": node.category or "other",
                    "kind": "rest",
                }
            )

        payload["children"] = slices
        return payload
