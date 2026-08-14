"""Read-only enforcement.

DiskAnalyzer is an inspection tool. It must never delete, move, rename,
truncate or otherwise modify anything on the drive it inspects.

Rather than merely promising that in a comment, :func:`audit_source` reads this
package's own source files and looks for the APIs that could cause damage. The
CLI runs the audit on every launch and aborts before touching the disk if
anything suspicious appears, so a careless future edit fails loudly instead of
silently gaining the ability to delete files.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass

# Modules permitted to open files for writing. These only ever write to the
# user-specified output directory, never to a scanned path.
WRITER_MODULES = frozenset({"report_html.py", "report_csv.py", "cli.py"})

# Modules permitted to start another program. Launching is how a read-only tool
# could still cause damage without ever calling a destructive API itself: hand a
# path to the shell and the shell will run it. A disk report is full of .exe
# files, so that would be one click away from every segment on the map. One
# module owns every launch, which makes reviewing this capability a matter of
# reading one file.
LAUNCHER_MODULES = frozenset({"launch.py"})

# Any occurrence of these outside an allowlisted context aborts the run.
DESTRUCTIVE_PATTERNS: tuple[tuple[str, str], ...] = (
    (r"\bos\.remove\b", "os.remove deletes a file"),
    (r"\bos\.unlink\b", "os.unlink deletes a file"),
    (r"\bos\.rmdir\b", "os.rmdir deletes a directory"),
    (r"\bos\.removedirs\b", "os.removedirs deletes directories"),
    (r"\bos\.rename\b", "os.rename moves a file"),
    (r"\bos\.replace\b", "os.replace overwrites a file"),
    (r"\bos\.truncate\b", "os.truncate destroys file contents"),
    (r"\bos\.chmod\b", "os.chmod modifies permissions"),
    (r"\bos\.utime\b", "os.utime rewrites timestamps"),
    (r"\bshutil\.rmtree\b", "shutil.rmtree deletes a tree"),
    (r"\bshutil\.move\b", "shutil.move relocates files"),
    (r"\bshutil\.copy", "shutil.copy writes files"),
    (r"\bsend2trash\b", "send2trash deletes files"),
    (r"\bpathlib\.Path\([^)]*\)\.unlink\b", "Path.unlink deletes a file"),
    (r"\.unlink\(", "unlink() deletes a file"),
    (r"\.rmdir\(", "rmdir() deletes a directory"),
    (r"\.write_text\(", "write_text() writes to disk"),
    (r"\.write_bytes\(", "write_bytes() writes to disk"),
    (r"\bRemove-Item\b", "Remove-Item deletes files"),
    (r"\bdel\s+/[fqs]", "del command deletes files"),
    (r"\bos\.system\b", "os.system runs a shell command"),
    (r"\bos\.startfile\b", "os.startfile executes the target"),
    (r"shell\s*=\s*True", "shell=True lets a path become a command"),
)

# Starting a process at all. Permitted only in LAUNCHER_MODULES.
SPAWN_PATTERN = re.compile(
    r"\b(?:subprocess\.\w+|os\.spawn\w*|os\.popen|os\.exec\w*|webbrowser\.\w+)\s*\("
)

# open() calls in a write/append/update mode.
WRITE_OPEN_PATTERN = re.compile(r"""open\s*\([^)]*?["'][rwxa+][^"']*?["']""")
SAFE_OPEN_MODES = re.compile(r"""["'](?:rb?|rt)["']""")


@dataclass(frozen=True)
class Violation:
    module: str
    line_no: int
    line: str
    reason: str

    def describe(self) -> str:
        return f"{self.module}:{self.line_no}: {self.reason}\n    {self.line.strip()}"


def _iter_source_files(package_dir: str):
    for name in sorted(os.listdir(package_dir)):
        if name.endswith(".py"):
            yield name, os.path.join(package_dir, name)


def audit_source(package_dir: str | None = None) -> list[Violation]:
    """Scan this package's source for destructive operations.

    Returns a list of violations; an empty list means the code on disk cannot
    modify anything it scans.
    """
    package_dir = package_dir or os.path.dirname(os.path.abspath(__file__))
    violations: list[Violation] = []

    for module_name, module_path in _iter_source_files(package_dir):
        if module_name == "safety.py":
            # This module names the forbidden APIs in order to detect them.
            continue
        try:
            with open(module_path, "r", encoding="utf-8") as handle:
                lines = handle.read().splitlines()
        except OSError as exc:  # pragma: no cover - unreadable own source
            violations.append(Violation(module_name, 0, "", f"cannot audit: {exc}"))
            continue

        for line_no, line in enumerate(lines, start=1):
            code = line.split("#", 1)[0]
            if not code.strip():
                continue

            for pattern, reason in DESTRUCTIVE_PATTERNS:
                if re.search(pattern, code):
                    violations.append(Violation(module_name, line_no, line, reason))

            match = WRITE_OPEN_PATTERN.search(code)
            if match and not SAFE_OPEN_MODES.search(match.group(0)):
                if module_name not in WRITER_MODULES:
                    violations.append(
                        Violation(
                            module_name,
                            line_no,
                            line,
                            "open() in write mode outside a report writer",
                        )
                    )

            if module_name not in LAUNCHER_MODULES and SPAWN_PATTERN.search(code):
                violations.append(
                    Violation(
                        module_name,
                        line_no,
                        line,
                        "starts a process outside the single audited launcher",
                    )
                )

    return violations


class ReadOnlyViolation(RuntimeError):
    """Raised when the package source contains a destructive operation."""


def enforce_read_only(package_dir: str | None = None) -> None:
    """Abort the process if the source could modify scanned files."""
    violations = audit_source(package_dir)
    if violations:
        details = "\n".join(v.describe() for v in violations)
        raise ReadOnlyViolation(
            "DiskAnalyzer refuses to run: its source contains operations that "
            "could modify the disk.\n\n" + details
        )


def open_for_read(path: str):
    """The only way this package is allowed to open a scanned file."""
    return open(path, "rb")
