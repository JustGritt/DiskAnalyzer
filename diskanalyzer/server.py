"""A localhost-only web UI for driving a scan and exploring the result.

Why a browser and not a window
------------------------------
The report already had to solve "draw a whole drive so a person can see it".
Reusing the browser for the interactive version means the sunburst is real SVG
with real text rendering, and the package still depends on nothing but the
standard library. ``diskanalyzer --ui`` starts this server, opens a tab, and the
tool remains a single folder of Python with no build step.

What the API is for
-------------------
The static report ships one pruned tree and lets the page navigate inside it,
which caps how deep it can go before the file becomes enormous. Here the walk's
full node set stays in this process, so the page asks for one folder at a time
and can descend as far as the scan actually went, at a few kilobytes per step.

Keeping it to this machine
--------------------------
An HTTP server that can enumerate a disk is worth locking down even when it
only listens on loopback, because every other program on the machine can reach
loopback too, and so can any web page the user has open.

* The socket binds to 127.0.0.1, so nothing off this machine can connect.
* Every request must carry a token minted at startup and handed to the page in
  its opening URL. Software that merely guesses the port gets 403.
* The ``Host`` header must name the loopback address, which is what stops a
  hostile site from pointing a name it controls at 127.0.0.1 and reading the
  responses from script.
* Requests arriving with a cross-site ``Sec-Fetch-Site`` are refused outright.
* No path on disk is ever served. The page is a constant in this package and
  the API speaks only JSON, so there is no file-serving code to traverse.
"""

from __future__ import annotations

import json
import os
import secrets
import shutil
import string
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from . import report_csv, report_html, webapp
from .browse import ScanIndex
from .insights import build_report
from .launch import LaunchError, reveal
from .scanner import ScanConfig, scan

MAX_BODY = 64 * 1024
PREFERRED_PORT = 8731


class ScanCancelled(Exception):
    """Raised inside the progress callback to unwind a running walk."""


# ---------------------------------------------------------------------------
# Volumes
# ---------------------------------------------------------------------------
DRIVE_KINDS = {2: "removable", 3: "fixed", 4: "network", 5: "optical", 6: "ram"}


def _drive_letters() -> list:
    """Present drive letters, without poking absent ones.

    ``os.path.exists`` on an empty optical drive or card reader can block for
    seconds while the hardware is asked. The bitmask answers for every letter at
    once without touching a device.
    """
    if os.name != "nt":
        return []
    try:
        import ctypes

        mask = ctypes.windll.kernel32.GetLogicalDrives()
    except Exception:
        return [
            letter
            for letter in string.ascii_uppercase
            if os.path.exists(letter + ":" + os.sep)
        ]
    return [
        letter
        for index, letter in enumerate(string.ascii_uppercase)
        if mask & (1 << index)
    ]


def _drive_details(root: str) -> tuple:
    """``(label, kind)`` for a drive root, best effort."""
    label, kind = "", "fixed"
    if os.name != "nt":
        return label, kind
    try:
        import ctypes

        kernel32 = ctypes.windll.kernel32
        kind = DRIVE_KINDS.get(kernel32.GetDriveTypeW(root), "fixed")
        name = ctypes.create_unicode_buffer(261)
        if kernel32.GetVolumeInformationW(root, name, 261, None, None, None, None, 0):
            label = name.value
    except Exception:
        pass
    return label, kind


def list_volumes() -> list:
    volumes = []
    for letter in _drive_letters():
        root = letter + ":" + os.sep
        label, kind = _drive_details(root)
        if kind in ("optical", "removable") and not os.path.isdir(root):
            continue  # empty tray or slot
        try:
            usage = shutil.disk_usage(root)
        except OSError:
            continue
        volumes.append(
            {
                "root": root,
                "letter": letter,
                "label": label,
                "kind": kind,
                "total": usage.total,
                "used": usage.used,
                "free": usage.free,
            }
        )
    if not volumes:
        try:
            usage = shutil.disk_usage(os.sep)
        except OSError:
            return volumes
        volumes.append(
            {
                "root": os.sep,
                "letter": "",
                "label": "",
                "kind": "fixed",
                "total": usage.total,
                "used": usage.used,
                "free": usage.free,
            }
        )
    return volumes


# ---------------------------------------------------------------------------
# Session
# ---------------------------------------------------------------------------
class Session:
    """One scan at a time, plus everything the page can ask about it.

    Every field crossing the worker/request boundary is read and written under
    one lock. The walk touches it a few times a second and requests do the same,
    so contention is irrelevant here and correctness is not.
    """

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.phase = "idle"  # idle scanning analyzing ready error cancelled
        self.message = ""
        self.files = 0
        self.size = 0
        self.current = ""
        self.started = 0.0
        self.elapsed = 0.0
        self.roots: tuple = ()
        self.result = None
        self.index = None
        self.report = None
        self.exported = ""
        self._cancel = False
        self._thread = None

    # -- state -------------------------------------------------------------
    def status(self) -> dict:
        with self.lock:
            if self.phase in ("ready", "error", "cancelled"):
                elapsed = self.elapsed
            elif self.started:
                elapsed = time.time() - self.started
            else:
                elapsed = 0.0
            return {
                "phase": self.phase,
                "message": self.message,
                "files": self.files,
                "size": self.size,
                "current": self.current,
                "elapsed": elapsed,
                "roots": list(self.roots),
                "exported": self.exported,
            }

    # -- running -----------------------------------------------------------
    def start(self, config: ScanConfig) -> None:
        with self.lock:
            if self.phase in ("scanning", "analyzing"):
                raise RuntimeError("a scan is already running")
            self.phase = "scanning"
            self.message = ""
            self.files = 0
            self.size = 0
            self.current = ""
            self.started = time.time()
            self.elapsed = 0.0
            self.roots = config.roots
            self.result = None
            self.index = None
            self.report = None
            self.exported = ""
            self._cancel = False

        self._thread = threading.Thread(
            target=self._run, args=(config,), daemon=True, name="diskanalyzer-scan"
        )
        self._thread.start()

    def cancel(self) -> None:
        with self.lock:
            self._cancel = True

    def _progress(self, result, current_path: str) -> None:
        with self.lock:
            if self._cancel:
                # The walk has no cancel flag of its own, and adding one would
                # put a check in the hot loop for the sake of a button. Raising
                # from the callback it already calls unwinds it just as promptly
                # and costs the scanner nothing.
                raise ScanCancelled()
            self.files = result.total_files
            self.size = result.total_size
            self.current = current_path

    def _fail(self, exc: Exception) -> None:
        with self.lock:
            self.phase = "error"
            self.message = str(exc)
            self.elapsed = time.time() - self.started

    def _run(self, config: ScanConfig) -> None:
        try:
            result = scan(config, progress=self._progress)
        except ScanCancelled:
            with self.lock:
                self.phase = "cancelled"
                self.message = "Scan cancelled. Nothing was modified."
                self.elapsed = time.time() - self.started
            return
        except Exception as exc:  # pragma: no cover - defensive
            self._fail(exc)
            return

        # Rolling up and classifying a few hundred thousand nodes takes a moment
        # of its own, and looking for duplicates reads from the disk, so the
        # page is told the walk is over rather than left on a frozen counter.
        with self.lock:
            self.phase = "analyzing"
            self.current = ""
            self.files = result.total_files
            self.size = result.total_size

        try:
            index = ScanIndex(result)
            report = build_report(
                result, top_n=60, file_n=60, with_duplicates=config.find_duplicates
            )
        except Exception as exc:  # pragma: no cover - defensive
            self._fail(exc)
            return

        with self.lock:
            self.result = result
            self.index = index
            self.report = report
            self.phase = "ready"
            self.elapsed = result.elapsed

    # -- reading the result ------------------------------------------------
    def snapshot(self) -> tuple:
        with self.lock:
            return self.result, self.index, self.report

    def export(self) -> dict:
        result, _index, report = self.snapshot()
        if report is None:
            raise RuntimeError("no finished scan to export")

        stamp = time.strftime("%Y%m%d-%H%M%S")
        out_dir = os.path.join(os.getcwd(), "reports", "scan-" + stamp)
        os.makedirs(out_dir, exist_ok=True)

        report_html.write_html(report, os.path.join(out_dir, "report.html"))
        report_csv.write_folder_csv(
            result, os.path.join(out_dir, "folders.csv"), min_size=1024 * 1024
        )
        written = ["report.html", "folders.csv"]
        if result.errors:
            report_csv.write_error_csv(result, os.path.join(out_dir, "unreadable.csv"))
            written.append("unreadable.csv")

        with self.lock:
            self.exported = out_dir
        return {"dir": out_dir, "files": written}


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------
class Handler(BaseHTTPRequestHandler):
    server_version = "DiskAnalyzer"
    sys_version = ""
    protocol_version = "HTTP/1.1"

    # -- plumbing ----------------------------------------------------------
    def log_message(self, fmt, *args) -> None:
        return  # the terminal belongs to the scan, not to an access log

    def _send(self, code: int, body: bytes, content_type: str, extra=()) -> None:
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        for name, value in extra:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def _json(self, code: int, payload) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self._send(code, body, "application/json; charset=utf-8")

    def _fail(self, code: int, message: str) -> None:
        self._json(code, {"error": message})

    def _read_body(self) -> dict:
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return {}
        if length <= 0 or length > MAX_BODY:
            return {}
        try:
            payload = json.loads(self.rfile.read(length).decode("utf-8"))
        except (ValueError, UnicodeDecodeError):
            return {}
        return payload if isinstance(payload, dict) else {}

    # -- access control ----------------------------------------------------
    def _host_ok(self) -> bool:
        host = (self.headers.get("Host") or "").strip()
        name = host.rsplit(":", 1)[0].strip("[]").lower()
        return name in ("127.0.0.1", "localhost", "::1")

    def _allowed(self, query: dict) -> bool:
        if not self._host_ok():
            return False
        if (self.headers.get("Sec-Fetch-Site") or "same-origin") not in (
            "same-origin",
            "none",
        ):
            return False
        supplied = self.headers.get("X-DiskAnalyzer-Token") or ""
        if not supplied:
            supplied = (query.get("k") or [""])[0]
        return secrets.compare_digest(supplied, self.server.token)

    # -- routes ------------------------------------------------------------
    def do_GET(self) -> None:  # noqa: N802 - http.server spelling
        parsed = urlparse(self.path)
        query = parse_qs(parsed.query)
        route = parsed.path.rstrip("/") or "/"

        if not self._allowed(query):
            self._fail(403, "not authorized for this session")
            return

        if route == "/":
            self._send(
                200,
                webapp.render(self.server.token).encode("utf-8"),
                "text/html; charset=utf-8",
                extra=(
                    (
                        "Content-Security-Policy",
                        "default-src 'none'; style-src 'unsafe-inline'; "
                        "script-src 'unsafe-inline'; connect-src 'self'; "
                        "img-src data:; form-action 'none'; frame-ancestors 'none'",
                    ),
                ),
            )
            return

        if route == "/api/volumes":
            self._json(200, {"volumes": list_volumes(), "cwd": os.getcwd()})
            return

        if route == "/api/status":
            self._json(200, self.server.session.status())
            return

        if route == "/api/node":
            self._node(query)
            return

        if route == "/api/summary":
            _result, _index, report = self.server.session.snapshot()
            if report is None:
                self._fail(409, "no finished scan yet")
                return
            self._json(200, report_html.build_payload(report))
            return

        self._fail(404, "no such endpoint")

    def do_POST(self) -> None:  # noqa: N802
        parsed = urlparse(self.path)
        query = parse_qs(parsed.query)
        route = parsed.path.rstrip("/") or "/"

        if not self._allowed(query):
            self._fail(403, "not authorized for this session")
            return

        if route == "/api/scan":
            self._start_scan(self._read_body())
            return

        if route == "/api/cancel":
            self.server.session.cancel()
            self._json(200, {"ok": True})
            return

        if route == "/api/reveal":
            self._reveal(self._read_body())
            return

        if route == "/api/export":
            try:
                self._json(200, self.server.session.export())
            except Exception as exc:
                self._fail(409, str(exc))
            return

        if route == "/api/quit":
            self._json(200, {"ok": True})
            threading.Thread(target=self.server.shutdown, daemon=True).start()
            return

        self._fail(404, "no such endpoint")

    # -- handlers ----------------------------------------------------------
    def _start_scan(self, body: dict) -> None:
        roots = []
        for entry in body.get("roots") or []:
            if not isinstance(entry, str) or not entry.strip():
                continue
            candidate = os.path.abspath(entry.strip())
            if not os.path.isdir(candidate):
                self._fail(400, "not a folder: " + entry)
                return
            roots.append(candidate)
        if not roots:
            self._fail(400, "choose at least one folder to scan")
            return

        config = ScanConfig(
            roots=tuple(roots),
            include_os=bool(body.get("includeOs")),
            use_allocated=bool(body.get("allocated")),
            find_duplicates=bool(body.get("duplicates")),
        )
        try:
            self.server.session.start(config)
        except RuntimeError as exc:
            self._fail(409, str(exc))
            return
        self._json(200, {"ok": True})

    def _node(self, query: dict) -> None:
        _result, index, _report = self.server.session.snapshot()
        if index is None:
            self._fail(409, "no finished scan yet")
            return

        wanted = (query.get("path") or [""])[0]
        if wanted:
            node = index.node_for(wanted)
            if node is None:
                self._fail(404, "that folder is not part of this scan")
                return
        else:
            roots = index.roots()
            if not roots:
                self._fail(404, "the scan found nothing")
                return
            node = max(roots, key=lambda item: item.total_size)

        self._json(200, index.view(node))

    def _reveal(self, body: dict) -> None:
        _result, index, _report = self.server.session.snapshot()
        if index is None:
            self._fail(409, "no finished scan yet")
            return
        path = body.get("path")
        if not isinstance(path, str):
            self._fail(400, "no path given")
            return
        try:
            reveal(path, verify=index.contains)
        except LaunchError as exc:
            self._fail(400, str(exc))
            return
        self._json(200, {"ok": True})


class UiServer(ThreadingHTTPServer):
    daemon_threads = True
    # A port already in use should fail loudly rather than be quietly shared
    # with whatever is holding it.
    allow_reuse_address = False

    def __init__(self, address, token: str):
        super().__init__(address, Handler)
        self.token = token
        self.session = Session()


def create(port: int = 0) -> UiServer:
    """Bind a loopback server, preferring a stable port so reloads keep working."""
    token = secrets.token_urlsafe(24)
    candidates = [port] if port else [PREFERRED_PORT, 0]
    last: OSError | None = None
    for candidate in candidates:
        try:
            return UiServer(("127.0.0.1", candidate), token)
        except OSError as exc:
            last = exc
    raise last if last is not None else OSError("could not bind a local port")


def url_for(server: UiServer) -> str:
    return "http://127.0.0.1:%d/?k=%s" % (server.server_address[1], server.token)
