"""Which parts of the drive the scanner skips, and why.

The goal is "everything except the operating system". That is a narrower cut
than "everything except Microsoft": leftovers such as ``C:\Windows.old`` or the
Recycle Bin are Microsoft-made but *are* reclaimable, so they stay in the scan.
Only the live OS itself and paths that cannot be read without SYSTEM rights are
skipped by default.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .util import normalize_key

# The live operating system. Deleting anything here breaks Windows, so there is
# no point spending scan time on it or cluttering the report with it.
LIVE_OS_DIRS: tuple[str, ...] = (
    r"C:\Windows",
    r"C:\Program Files\WindowsApps",
    r"C:\ProgramData\Microsoft\Windows Defender",
    r"C:\ProgramData\Microsoft\Windows Defender Advanced Threat Protection",
    r"C:\Config.Msi",
    r"C:\PerfLogs",
    r"C:\Recovery",
    r"C:\$SysReset",
    r"C:\$WinREAgent",
    r"C:\OneDriveTemp",
)

# Readable only as SYSTEM. Descending here yields nothing but access errors.
UNREADABLE_DIRS: tuple[str, ...] = (
    r"C:\System Volume Information",
)

# Legacy junctions kept for backwards compatibility. They loop back into real
# folders, so following them would double-count. Reparse-point detection
# already catches these, but naming them keeps the skip report honest.
COMPAT_JUNCTIONS: tuple[str, ...] = (
    r"C:\Documents and Settings",
    r"C:\Users\All Users",
    r"C:\Users\Default User",
)

# Deliberately NOT excluded, even though they look like OS paths. Each is a
# well-known source of many gigabytes that the user can legitimately recover.
NOTABLE_INCLUSIONS: tuple[tuple[str, str], ...] = (
    (r"C:\Windows.old", "leftover from a Windows upgrade; removable via Disk Cleanup"),
    (r"C:\$Windows.~BT", "Windows upgrade staging files"),
    (r"C:\$Windows.~WS", "Windows upgrade staging files"),
    (r"C:\$Recycle.Bin", "deleted files still occupying space"),
    (r"C:\ProgramData\Package Cache", "installer caches kept by MSI/VS"),
)

# Volume-level files managed by Windows. They are counted and reported on their
# own line rather than mixed into folder totals, because their size is a system
# setting rather than something sitting in a folder.
OS_MANAGED_FILENAMES: frozenset[str] = frozenset(
    {
        "pagefile.sys",
        "hiberfil.sys",
        "swapfile.sys",
        "dumpstack.log",
        "dumpstack.log.tmp",
        "bootmgr",
        "bootnxt",
    }
)


@dataclass
class ExclusionSet:
    """Decides, per directory, whether the scanner descends into it."""

    include_os: bool = False
    extra_dirs: tuple[str, ...] = ()
    extra_names: tuple[str, ...] = ()

    _dirs: dict = field(default_factory=dict, init=False, repr=False)
    _names: set = field(default_factory=set, init=False, repr=False)

    def __post_init__(self) -> None:
        if not self.include_os:
            for path in LIVE_OS_DIRS:
                self._dirs[normalize_key(path)] = "windows os"
            for path in UNREADABLE_DIRS:
                self._dirs[normalize_key(path)] = "system-protected"
            for path in COMPAT_JUNCTIONS:
                self._dirs[normalize_key(path)] = "legacy junction"
        for path in self.extra_dirs:
            self._dirs[normalize_key(path)] = "user-excluded"
        self._names = {name.strip().lower() for name in self.extra_names if name.strip()}

    def reason_for(self, display_path: str, name: str) -> str | None:
        """Return why this directory is skipped, or None to descend into it."""
        reason = self._dirs.get(normalize_key(display_path))
        if reason is not None:
            return reason
        if name.lower() in self._names:
            return "user-excluded name"
        return None

    def is_os_managed_file(self, name: str, parent_is_volume_root: bool) -> bool:
        if not parent_is_volume_root:
            return False
        return name.lower() in OS_MANAGED_FILENAMES
