"""The one place this package is allowed to start another program.

DiskAnalyzer tells you where the space went and then stops. "Reveal in
Explorer" is the whole of its hand-off: it opens a File Explorer window with
the item selected, and what happens next is yours to do, in a program that was
actually designed to delete things.

Two rules make that safe to offer.

*Explorer is the only program ever launched, and only ever with ``/select``.*
``/select`` shows an item inside its folder. It never opens or runs it, which
matters because half of what a disk report surfaces is executable. Handing the
path to the desktop's default-open call instead - ShellExecute and its wrappers -
would run a ``.exe`` the moment someone clicked its slice on the map, which is
why :mod:`diskanalyzer.safety` now rejects those by name anywhere in this
package, including here.

*The caller has to prove the path came from the scan.* :func:`reveal` takes a
``verify`` predicate and refuses any path it rejects, so a request that reached
the local API some other way cannot steer Explorer somewhere the scan never
looked.

:mod:`diskanalyzer.safety` allowlists this module by name for process launches
and rejects them everywhere else, so this file stays the only door. Opening the
UI in a browser lives here for the same reason: it is a launch, and launches
belong behind one audited door rather than scattered through the codebase.
"""

from __future__ import annotations

import os
import subprocess
import sys
import webbrowser
from typing import Callable


class LaunchError(RuntimeError):
    """Nothing could be started, and nothing was."""


def reveal(path: str, verify: Callable[[str], bool] | None = None) -> None:
    """Open Explorer with ``path`` selected. Never opens the item itself."""
    if not path or "\x00" in path:
        raise LaunchError("not a usable path")

    if verify is not None and not verify(path):
        raise LaunchError("that path is not part of the current scan")

    if not os.path.exists(path):
        raise LaunchError("that path no longer exists")

    if sys.platform != "win32":
        raise LaunchError("revealing in Explorer is a Windows feature")

    # One argument, not two: Explorer parses "/select,<path>" as a single
    # token, and splitting it makes it silently open Documents instead. The
    # list form means no shell is involved, so nothing inside the path can be
    # read as a command.
    try:
        subprocess.Popen(
            ["explorer.exe", "/select," + os.path.normpath(path)],
            close_fds=True,
        )
    except OSError as exc:  # pragma: no cover - Explorer missing is exotic
        raise LaunchError(f"could not start Explorer: {exc}") from exc


def open_browser(url: str) -> bool:
    """Show the UI. Only ever called with this process's own loopback URL."""
    if not url.startswith("http://127.0.0.1:"):
        raise LaunchError("refusing to open a non-loopback URL")
    try:
        return webbrowser.open(url)
    except Exception:  # pragma: no cover - depends on the desktop session
        return False
