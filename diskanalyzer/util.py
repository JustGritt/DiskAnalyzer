"""Small helpers shared across the package: path handling and formatting."""

from __future__ import annotations

import os
import sys
import time

EXTENDED_PREFIX = "\\\\?\\"
UNC_EXTENDED_PREFIX = "\\\\?\\UNC\\"


# --------------------------------------------------------------------------
# Paths
# --------------------------------------------------------------------------
def to_extended_path(path: str) -> str:
    """Return a ``\\\\?\\``-prefixed path so the walk is not capped at MAX_PATH.

    Windows silently refuses to enumerate directories once a path exceeds 260
    characters unless the extended-length prefix is used. Deep node_modules or
    build trees hit that limit routinely, and those are exactly the trees we
    care about, so every root is prefixed before scanning.
    """
    if os.name != "nt":
        return os.path.abspath(path)
    if path.startswith(EXTENDED_PREFIX):
        return path
    absolute = os.path.abspath(path)
    if absolute.startswith("\\\\"):
        return UNC_EXTENDED_PREFIX + absolute[2:]
    return EXTENDED_PREFIX + absolute


def to_display_path(path: str) -> str:
    """Strip the extended-length prefix so paths are readable again."""
    if path.startswith(UNC_EXTENDED_PREFIX):
        return "\\\\" + path[len(UNC_EXTENDED_PREFIX):]
    if path.startswith(EXTENDED_PREFIX):
        return path[len(EXTENDED_PREFIX):]
    return path


def normalize_key(path: str) -> str:
    """Case-insensitive comparison key for a Windows path."""
    return to_display_path(path).rstrip("\\/").lower()


def path_depth(path: str) -> int:
    return to_display_path(path).count("\\")


# --------------------------------------------------------------------------
# Formatting
# --------------------------------------------------------------------------
_SIZE_UNITS = ("B", "KB", "MB", "GB", "TB", "PB")


def human_size(num_bytes: float) -> str:
    """Format bytes the way Explorer does (binary multiples, decimal labels)."""
    value = float(num_bytes)
    for unit in _SIZE_UNITS:
        if abs(value) < 1024.0 or unit == _SIZE_UNITS[-1]:
            if unit == "B":
                return f"{int(value)} B"
            return f"{value:,.1f} {unit}"
        value /= 1024.0
    return f"{value:,.1f} PB"


def human_count(num: int) -> str:
    return f"{num:,}"


def human_duration(seconds: float) -> str:
    seconds = int(seconds)
    if seconds < 60:
        return f"{seconds}s"
    minutes, secs = divmod(seconds, 60)
    if minutes < 60:
        return f"{minutes}m {secs:02d}s"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes:02d}m {secs:02d}s"


def human_age(timestamp: float, now: float | None = None) -> str:
    """Render a modification time as an approximate age."""
    if timestamp <= 0:
        return "unknown"
    now = time.time() if now is None else now
    days = max(0, int((now - timestamp) / 86400))
    if days < 1:
        return "today"
    if days < 60:
        return f"{days}d"
    if days < 730:
        return f"{days // 30}mo"
    return f"{days // 365}y"


def truncate_middle(text: str, width: int) -> str:
    """Shorten a path but keep both ends, which is where the meaning lives."""
    if width <= 3 or len(text) <= width:
        return text
    keep = width - 3
    head = keep // 2
    tail = keep - head
    return text[:head] + "..." + text[len(text) - tail:]


def round_to_cluster(size: int, cluster: int) -> int:
    """Approximate on-disk size: every file occupies whole clusters."""
    if cluster <= 1:
        return size
    if size == 0:
        return 0
    return ((size + cluster - 1) // cluster) * cluster


# --------------------------------------------------------------------------
# Terminal
# --------------------------------------------------------------------------
def supports_color(stream=None) -> bool:
    stream = stream or sys.stdout
    if os.environ.get("NO_COLOR"):
        return False
    if not hasattr(stream, "isatty") or not stream.isatty():
        return False
    if os.name == "nt":
        # Windows Terminal / VS Code / recent conhost all handle VT sequences.
        return bool(
            os.environ.get("WT_SESSION")
            or os.environ.get("TERM_PROGRAM")
            or os.environ.get("ANSICON")
            or sys.getwindowsversion().major >= 10
        )
    return True


def terminal_width(default: int = 100) -> int:
    try:
        width = os.get_terminal_size().columns
    except OSError:
        return default
    return max(60, min(width, 140))
