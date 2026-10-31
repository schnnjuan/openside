"""Command-line entry point for the iPASide Engine.

Human-readable by default; pass ``--json`` for machine-readable output (this is
how the desktop shell consumes the engine).
"""

from __future__ import annotations

import argparse
import base64
import datetime
import difflib
import getpass
import io
import json
import os
import re
import sys
import threading
from pathlib import Path
from typing import Any

from . import (
    __version__,
    account,
    anisette,
    apple_support,
    apps,
    developer,
    developer_mode,
    device,
    doctor,
    gsa,
    ipa,
    jailbreak,
    livecontainer,
    lockdown,
    pairing,
    provision,
    refresh,
    sideload,
    signing,
    tweak,
)
from .errors import EngineError

# --------------------------------------------------------------------------- #
# Pretty human output
#
# A small Ollama/OpenCode-style status layer for the human path of the CLI:
# an animated spinner line for long phases, and ✓/✗ result lines. The --json
# path (which the desktop shell uses) never runs this code, and when stdout or
# stderr is piped the colours and spinner degrade to plain text lines.
# --------------------------------------------------------------------------- #

_TTY = sys.stdout.isatty() and sys.stderr.isatty()
if _TTY and not os.environ.get("NO_COLOR") and os.environ.get("TERM") != "dumb":
    _C_RESET, _C_DIM, _C_BOLD = "\x1b[0m", "\x1b[2m", "\x1b[1m"
    _C_CYAN, _C_GREEN, _C_RED, _C_YELLOW = "\x1b[36m", "\x1b[32m", "\x1b[31m", "\x1b[33m"
else:
    _C_RESET = _C_DIM = _C_BOLD = _C_CYAN = _C_GREEN = _C_RED = _C_YELLOW = ""

# Friendly names for the phases the engine reports; anything unmapped is shown
# as-is, capitalised.
_PHASE_LABELS = {
    "provision": "Provisioning",
    "sign": "Signing",
    "install": "Installing",
    "upload": "Uploading",
    "download": "Downloading",
    "read": "Reading IPA",
    "extract": "Extracting",
    "finalize": "Wrapping up",
}

# Apple service errors where the fix is a command the user can run right now.
_ERROR_HINTS = (
    ("7460",
     "Apple thinks another certificate request is pending. Run `slots` to list "
     "your certificates and `revoke-cert <serial>` to free the stale one."),
)


def _hint_for(message: str) -> str | None:
    for needle, hint in _ERROR_HINTS:
        if needle in message:
            return hint
    return None


class _PrettyProgress:
    """One animated status line, shared by every human-mode progress callback.

    Each callback just calls ``emit``; a daemon timer redraws the spinner
    between callbacks so the line keeps breathing during silent stretches.
    """

    def __init__(self, stream: Any) -> None:
        self._stream = stream
        self._lock = threading.Lock()
        self._timer: threading.Timer | None = None
        self._active = False
        self._frame = 0
        self._label = ""
        self._detail = ""
        self._percent: int | float | None = None
        self._last_label: str | None = None

    def emit(self, phase: Any, percent: Any = None, step: Any = None) -> None:
        label = _PHASE_LABELS.get(phase) or (str(phase).capitalize() if phase else "Working")
        with self._lock:
            self._label = label
            self._detail = str(step or "")
            self._percent = percent if isinstance(percent, (int, float)) else None
            phase_changed = self._label != self._last_label
            self._last_label = label
            if not self._active:
                self._active = True
                self._frame = 0
            if _TTY:
                self._draw()
                self._ensure_timer()
            elif phase_changed:
                # Piped: one plain line per phase, never per percent tick,
                # so a captured log shows phases, not a waterfall of updates.
                text = f"{self._label}…"
                if self._detail:
                    text += f" {self._detail}"
                if self._percent is not None:
                    text = f"{int(self._percent)}% {text}"
                print(text, file=self._stream, flush=True)

    def stop(self) -> None:
        with self._lock:
            timer, self._timer, self._active = self._timer, None, False
        if timer:
            timer.cancel()

    def erase(self) -> None:
        self.stop()
        if _TTY:
            try:
                self._stream.write("\r\x1b[2K")
                self._stream.flush()
            except (OSError, ValueError):
                pass

    _BAR_W = 24

    def _determinate(self) -> str:
        filled = max(0, min(self._BAR_W, int(self._percent / 100 * self._BAR_W)))
        return f"{_C_CYAN}{'█' * filled}{_C_RESET}{_C_DIM}{'░' * (self._BAR_W - filled)}{_C_RESET}"

    def _indeterminate(self) -> str:
        # No percent from Apple (provision): a 5-cell block sweeping the bar.
        seg = 5
        pos = self._frame % max(1, self._BAR_W - seg + 1)
        return (f"{_C_DIM}{'░' * pos}{_C_RESET}{_C_CYAN}{'█' * seg}{_C_RESET}"
                f"{_C_DIM}{'░' * (self._BAR_W - seg - pos)}{_C_RESET}")

    def _draw(self) -> None:
        if not _TTY:
            return
        frame = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"[self._frame % 10]
        bar = self._determinate() if self._percent is not None else self._indeterminate()
        line = f"{_C_CYAN}{frame}{_C_RESET} {_C_BOLD}{self._label}{_C_RESET}  {bar}"
        if self._percent is not None:
            line += f"  {_C_BOLD}{int(self._percent)}%{_C_RESET}"
        if self._detail:
            line += f"  {_C_DIM}{self._detail}{_C_RESET}"
        try:
            self._stream.write("\r\x1b[2K" + line)
            self._stream.flush()
        except (OSError, ValueError):
            return  # stderr closed mid-run; nothing to draw on

    def _ensure_timer(self) -> None:
        if self._timer is None:
            self._timer = threading.Timer(0.12, self._tick)
            self._timer.daemon = True
            self._timer.start()

    def _tick(self) -> None:
        with self._lock:
            self._timer = None
            if not self._active:
                return
            self._frame += 1
            self._draw()
            self._ensure_timer()


_PROGRESS = _PrettyProgress(sys.stderr)


def _settle() -> None:
    """Stop the spinner and clear its line before printing a result line."""
    _PROGRESS.erase()


def _ok(text: str) -> None:
    _settle()
    print(f"{_C_GREEN}✓{_C_RESET} {text}", flush=True)


def _note(text: str) -> None:
    print(f"{_C_DIM}  {text}{_C_RESET}", flush=True)


# Slash-style menu: what `ipaside` shows with no arguments. Short, curated
# descriptions for exactly every subcommand that exists (verified against
# _HANDLERS by a test), so the menu can't promise a command that doesn't run.
_MENU_GROUPS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("Getting started", (
        "doctor", "devices", "device-info", "login",
    )),
    ("The main event", (
        "sideload", "installs", "refresh", "signed",
    )),
    ("Apple ID slots", (
        "teams", "provision", "sign", "slots", "delete-app-id", "revoke-cert",
    )),
    ("Device extras", (
        "developer-mode", "pairing", "livecontainer", "jailbreak",
    )),
    ("IPA tooling", (
        "inspect", "prepare", "resolve-tweak", "apps", "install", "uninstall",
        "app-icons",
    )),
    ("Infrastructure", (
        "anisette", "apple-support", "forget", "version", "serve",
    )),
)

_MENU_HELP = {
    "doctor": "check this PC is ready to sideload",
    "devices": "list connected iPhones (USB / Wi-Fi)",
    "device-info": "details for one connected device",
    "login": "sign in to an Apple ID (or --code, --status)",
    "sideload": "sign + install an .ipa, one shot",
    "installs": "what is sideloaded + days until expiry",
    "refresh": "re-sign + reinstall expiring apps",
    "signed": "kept signed .ipas (or --clean)",
    "teams": "development teams on the Apple ID",
    "provision": "register the device + mint signing assets",
    "sign": "sign an .ipa into a new file",
    "slots": "certificates / app IDs / devices in the slot",
    "delete-app-id": "free one app-ID identifier",
    "revoke-cert": "free a signing certificate",
    "developer-mode": "show or enable Developer Mode",
    "pairing": "manage + deliver the pairing file",
    "livecontainer": "install / inspect LiveContainer + guests",
    "jailbreak": "advise or install Dopamine",
    "inspect": "read an .ipa's metadata + encryption",
    "prepare": "pre-process an .ipa before signing",
    "resolve-tweak": "dylibs inside a .deb/.dylib",
    "apps": "installed apps on the device",
    "install": "install an already-signed .ipa",
    "uninstall": "remove an app by bundle id",
    "app-icons": "home-screen icons of installed apps",
    "anisette": "anisette status or headers",
    "apple-support": "Apple USB device-support status",
    "forget": "drop one sideload record",
    "version": "print the engine version",
    "serve": "persistent engine server (for the desktop app)",
}


# Check-style marks for status reports (doctor, pairing status, ...).
_CHECK_MARK = {
    "ok": f"{_C_GREEN}✓{_C_RESET}",
    "warn": f"{_C_YELLOW}!{_C_RESET}",
    "fail": f"{_C_RED}✗{_C_RESET}",
}
_CHECK_DIM = f"{_C_DIM}·{_C_RESET}"

# The wordmark: lowercase pixel-art glyphs in the same family as the
# "ratscope" wordmark, rendered with half-block cells (▀/▄) so each cell is
# two pixels tall. Color ramps light cyan -> dark cyan so the mark reads as
# depth, not noise. Only drawn on a real terminal; piped logs never see it.
_PIXEL_GLYPHS = {
    "i": (".####.", "..##..", "..##..", "..##..", "..##..", "..##..", "..##..", ".####."),
    "p": ("#####.", "##..##", "##..##", "##..##", "#####.", "##....", "##....", "##....", "##....", "##...."),
    "a": (".####.", "....##", "....##", ".####.", "##..##", "##..##", "##..##", ".####."),
    "s": (".#####", "##....", "##....", ".####.", "....##", "....##", "....##", "#####."),
    "d": (".####.", "##..##", "##..##", "##..##", "##..##", "##..##", "##..##", ".####."),
    "e": (".####.", "##..##", "##..##", "######", "##....", "##....", "##..##", ".####."),
}


def _pixel_grid(word: str) -> list[str]:
    width = len(word) * 6 + (len(word) - 1) * 1
    rows = max(len(_PIXEL_GLYPHS[ch]) for ch in word)
    grid = [" " * width for _ in range(rows)]
    x = 0
    for ch in word:
        glyph = _PIXEL_GLYPHS[ch]
        for y, row in enumerate(glyph):
            grid[y] = grid[y][:x] + row + grid[y][x + len(row):]
        x += 7
    return grid


def _print_banner() -> None:
    if not _TTY:
        return
    grid = _pixel_grid("ipaside")
    pairs = (len(grid) + 1) // 2
    ramp = ("51", "48", "45", "42", "39")
    lines = []
    for pair in range(pairs):
        top_row = grid[pair * 2]
        bot_row = grid[pair * 2 + 1] if pair * 2 + 1 < len(grid) else " " * len(top_row)
        color = f"\x1b[38;5;{ramp[pair % len(ramp)]}m"
        cells = []
        for top, bottom in zip(top_row, bot_row):
            t_on, b_on = top != " ", bottom != " "
            if t_on and b_on:
                cells.append(f"\x1b[38;5;{ramp[pair % len(ramp)]}m\x1b[48;5;{ramp[min(pair + 1, len(ramp) - 1)]}m▀")
            elif t_on:
                cells.append("▀")
            elif b_on:
                cells.append("▄")
            else:
                cells.append(" ")
        lines.append(f"{color}{''.join(cells)}\x1b[0m")
    for line in lines:
        print(line)
    print(f"  {_C_DIM}iOS sideloading · v{__version__}{_C_RESET}\n")



def _fail(title: str, detail: Any = None, hint: str | None = None) -> None:
    _settle()
    print(f"{_C_RED}✗{_C_RESET} {_C_BOLD}{title}{_C_RESET}", flush=True)
    if detail is not None and str(detail):
        for line in str(detail).splitlines():
            print(f"  {line}", flush=True)
    if hint:
        print(f"{_C_DIM}  hint: {hint}{_C_RESET}", flush=True)


def _print_menu() -> int:
    """The slash-style palette `ipaside` shows when no command is given."""
    _print_banner()
    for group, names in _MENU_GROUPS:
        print(f"  {_C_BOLD}{group}{_C_RESET}")
        for name in names:
            print(f"    {name:<16} {_C_DIM}{_MENU_HELP.get(name, '')}{_C_RESET}")
    print(f"\n  {_C_DIM}details:  ipaside <command> --help   ·   quick start:"
          f" ipaside login, then ipaside sideload <file>.ipa{_C_RESET}")
    return 0


class _RootParser(argparse.ArgumentParser):
    """Root parser that adds a 'did you mean' hint for unknown subcommands."""

    subcommands: list[str] = []

    def error(self, message: str) -> None:  # type: ignore[override]
        match = re.search(r"invalid choice: '([^']+)'", message)
        candidates = (
            difflib.get_close_matches(match.group(1), self.subcommands, 3, 0.4)
            if match else []
        )
        if candidates:
            message = (
                f"unknown command '{match.group(1)}'\n\n  did you mean:\n"
                + "".join(f"      {c}\n" for c in candidates)
            )
        self.exit(2, f"{self.prog}: error: {message}\n")

# SideStore runs an iOS Shortcut by this exact name after a refresh, to drop the local
# tunnel it needed. The name is hardcoded in SideStore, and when no such Shortcut exists
# the Shortcuts app opens with "could not find the shortcut". The refresh still completes -
# it is noise, not a failure - but it is alarming noise, and only the user can fix it:
# Shortcuts cannot be created from a PC.
SIDESTORE_SHORTCUT_NOTE = (
    "SideStore also runs an iOS Shortcut named exactly 'TurnOffData' when it finishes, to "
    "disconnect the local tunnel. If you have not made one, Shortcuts will open with an "
    "error - the refresh still worked. Create a Shortcut with that name containing a "
    "disconnect-VPN action to silence it."
)


def _enable_fault_handler() -> None:
    """Dump every thread to stderr if a native library aborts the process.

    Anisette drives the Unicorn emulator. A segfault or a Windows fail-fast
    there is not a Python exception, so the serve loop cannot turn it into a
    result frame. The desktop app keeps stderr and shows the last lines.
    """
    import faulthandler

    try:
        faulthandler.enable(file=sys.stderr, all_threads=True)
    except (AttributeError, OSError, RuntimeError, ValueError):
        return


def _force_utf8() -> None:
    """Force UTF-8 stdout/stderr.

    Device names, app names, and Apple account data routinely contain non-ASCII
    characters (e.g. curly quotes, RTL marks). Windows consoles default to a
    legacy code page (cp1252) that raises ``UnicodeEncodeError`` on those, so we
    switch the streams to UTF-8 up front.
    """
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            try:
                reconfigure(encoding="utf-8")
            except (ValueError, OSError):
                pass


def _json_default(obj: Any) -> str:
    """Serialize plist-native types (dates, data) that JSON can't handle."""
    if isinstance(obj, (datetime.datetime, datetime.date)):
        return obj.isoformat()
    if isinstance(obj, (bytes, bytearray)):
        return base64.b64encode(bytes(obj)).decode()
    return str(obj)


def _emit(args: argparse.Namespace, payload: Any) -> None:
    """Print a JSON payload (only used on the --json path)."""
    print(json.dumps(payload, indent=2, default=_json_default))


def _print_doctor(report: dict[str, Any]) -> None:
    print(f"{_C_BOLD}iPASide doctor{_C_RESET}")
    for check in report["checks"]:
        glyph = _CHECK_MARK.get(check["status"], _CHECK_DIM)
        print(f"  {glyph}  {check['name']}")
        print(f"      {_C_DIM}{check['detail']}{_C_RESET}")
    overall = report["overall"]
    glyph = _CHECK_MARK.get(overall, overall)
    print(f"\n  {glyph}  overall: {overall}")


def _cmd_doctor(args: argparse.Namespace) -> int:
    report = doctor.run_doctor()
    if args.json:
        _emit(args, report)
    else:
        _print_doctor(report)
    return 1 if report["overall"] == "fail" else 0


def _cmd_devices(args: argparse.Namespace) -> int:
    devices = device.list_devices()
    if args.json:
        _emit(args, devices)
        return 0
    if not devices:
        _note("No iOS devices visible. Plug in via USB, unlock, and tap 'Trust'.")
        return 0
    conn_tty = isinstance(devices[0].get("connection_type"), str)
    for dev in devices:
        conn = str(dev.get("connection_type", "?"))
        mark = _CHECK_MARK.get("ok", "") if conn.lower() == "usb" else _CHECK_DIM
        print(f"  {mark}  {conn:<5} {_C_BOLD}{dev.get('serial', '?')}{_C_RESET}  "
              f"{_C_DIM}(id={dev.get('device_id', '?')}){_C_RESET}")
    print(f"\n  {_C_DIM}{len(devices)} device(s){_C_RESET}")
    return 0


def _cmd_device_info(args: argparse.Namespace) -> int:
    info = device.get_device_info(args.udid)
    if args.json:
        _emit(args, info)
    else:
        if not info:
            _note("No device info available (is a device connected and trusted?).")
            return 0
        width = max(len(str(key)) for key in info)
        for key, value in info.items():
            print(f"  {_C_DIM}{str(key):<{width}}{_C_RESET} {value}")
    return 0


def _cmd_apps(args: argparse.Namespace) -> int:
    installed = apps.list_installed(args.udid, app_type=args.type)
    if args.json:
        _emit(args, installed)
        return 0
    if not installed:
        _note("No apps found.")
        return 0
    names = [str(meta.get("name") or "") for meta in installed.values()]
    width = max(map(len, names), default=16)
    for bundle_id, meta in sorted(installed.items()):
        name = str(meta.get("name") or "")
        version = str(meta.get("version") or "")
        print(f"  {name:<{width}}  {_C_CYAN}{version}{_C_RESET}  {_C_DIM}{bundle_id}{_C_RESET}")
    print(f"\n  {_C_DIM}{len(installed)} app(s){_C_RESET}")
    return 0


def _cmd_developer_mode(args: argparse.Namespace) -> int:
    if args.enable:
        if not args.yes:
            msg = "Refusing to enable Developer Mode without --yes (this reboots the device)."
            if args.json:
                _emit(args, {"ok": False, "reason": msg})
            else:
                _fail("Developer Mode not enabled", "--yes required (this reboots the device)")
            return 2
        result = developer_mode.enable(args.udid)
        if args.json:
            _emit(args, result)
        else:
            _ok(result.get("note", "Developer Mode enable triggered."))
        return 0

    state = developer_mode.status(args.udid)
    if args.json:
        _emit(args, state)
    else:
        enabled = state.get("enabled")
        label = {
            True: f"{_C_GREEN}enabled{_C_RESET}",
            False: f"{_C_RED}disabled{_C_RESET}",
            None: f"{_C_DIM}unknown{_C_RESET}",
        }.get(enabled, "unknown")
        print(f"Developer Mode: {label}")
        if state.get("reason"):
            _note(state["reason"])
        if state.get("product_version"):
            _note(f"iOS {state['product_version']}")
    return 0


def _cmd_install(args: argparse.Namespace) -> int:
    def progress(update: dict[str, Any]) -> None:
        percent = update.get("percent")
        phase = update.get("phase")
        status = update.get("status")
        label = status or phase or ""
        # Progress goes to stderr so stdout stays a single clean JSON result
        # for programmatic callers (the desktop app); CLI users still see it.
        if args.json:
            print(json.dumps({"event": "progress", "phase": phase, "percent": percent, "status": status}),
                  file=sys.stderr, flush=True)
        else:
            _PROGRESS.emit(phase, percent, label)

    apps.install(args.ipa, args.udid, progress=progress)
    if args.json:
        _emit(args, {"ok": True, "installed": args.ipa})
    else:
        _ok("Install complete.")
    return 0


def _cmd_app_icons(args: argparse.Namespace) -> int:
    found = apps.icons(args.bundle_id or None, args.udid, app_type=args.type)
    if args.json:
        _emit(args, found)
        return 0
    if not found:
        _note("No icons available.")
        return 0
    for bundle_id in sorted(found):
        print(f"  {_C_DIM}{bundle_id}  ({len(found[bundle_id])} chars){_C_RESET}")
    return 0


def _cmd_uninstall(args: argparse.Namespace) -> int:
    apps.uninstall(args.bundle_id, args.udid)
    if args.json:
        _emit(args, {"ok": True, "uninstalled": args.bundle_id})
    else:
        _ok(f"Uninstalled {args.bundle_id}")
    return 0


def _cmd_anisette(args: argparse.Namespace) -> int:
    if args.refresh:
        headers = anisette.get_headers()
        if args.json:
            _emit(args, headers)
        else:
            print("Anisette headers generated:")
            for key in sorted(headers):
                value = str(headers[key])
                preview = value[:16] + ("..." if len(value) > 16 else "")
                print(f"  {key}: {preview}")
        return 0

    state = anisette.status()
    if args.json:
        _emit(args, state)
    else:
        pkg = str(state["package_version"] or "")
        if pkg:
            print(f"  {_C_GREEN}✓{_C_RESET} Anisette {_C_CYAN}{pkg}{_C_RESET}")
        else:
            print(f"  {_C_RED}✗{_C_RESET} Anisette not installed")
        print(f"  {_C_DIM}provisioning state cached: {state['state_cached']}")
        print(f"  {state['state_path']}{_C_RESET}")
    return 0


def _cmd_apple_support(args: argparse.Namespace) -> int:
    """Report Apple's device stack, or fetch / start what USB needs.

    Status by default, each action behind its own flag, the way ``anisette`` and
    ``signed`` are shaped. The installer is downloaded and verified but never run:
    the engine hands back a path it has proved is Apple's, and the app launches it.
    """
    if args.download:
        result = apple_support.download_itunes(args.dir, on_progress=_progress_to_stderr(json_mode=args.json))
        if args.json:
            _emit(args, result)
        else:
            _ok("Downloaded and verified Apple's iTunes installer.")
            print(f"  file:   {result['path']} ({_mb(result['bytes'])})")
            print(f"  signer: {result['signer']}")
            _note("Run it to install Apple Mobile Device Service.")
        return 0

    if args.start_service:
        result = apple_support.start_service()
        if args.json:
            _emit(args, result)
        else:
            if result["started"]:
                _ok("Apple Mobile Device Service started")
            else:
                _fail("Apple Mobile Device Service not started")
            print(f"  {result['detail']}")
        return 0

    report = apple_support.status()
    if args.json:
        _emit(args, report)
        return 0
    state = str(report["state"])
    mark = _CHECK_MARK.get("ok", "") if state == "ready" else _CHECK_MARK.get("warn", "")
    print(f"  {mark}  Apple device support: {state}")
    print(f"      {_C_DIM}{report['detail']}{_C_RESET}")
    if report["itunes_installed"]:
        version = report["itunes_version"]
        label = f"iTunes {version} installed" if version else "iTunes installed"
        print(f"      {_C_DIM}{label}{_C_RESET}")
    return 0


def _cmd_teams(args: argparse.Namespace) -> int:
    teams = developer.list_teams()
    if args.json:
        _emit(args, teams)
        return 0
    if not teams:
        _note("No development teams found for this account.")
        return 0
    for team in teams:
        print(f"  {_C_BOLD}{team.get('teamId')}{_C_RESET}  {team.get('name')}  "
              f"{_C_DIM}({team.get('type')}, status={team.get('status')}){_C_RESET}")
    print(f"\n  {_C_DIM}{len(teams)} team(s){_C_RESET}")
    return 0


def _cmd_provision(args: argparse.Namespace) -> int:
    try:
        # Provisioning registers a UDID with Apple and burns a device slot, so it
        # must not guess which phone it means any more than a sideload does.
        udid = device.resolve_serial(args.udid)
    except device.DeviceError as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Device error", exc)
        return 2

    try:
        bundle = provision.ensure_signing_assets(
            args.bundle_id,
            udid,
            app_name=args.app_name or args.bundle_id,
            device_name=args.device_name or "iPASide device",
        )
    except (gsa.GsaError, developer.DeveloperServicesError) as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Provisioning failed", exc)
        return 1

    if args.json:
        _emit(args, bundle)
    else:
        _ok(f"Provisioned signing assets for {bundle['bundle_id']}")
        print(f"  team:        {bundle['team_name']} ({bundle['team_id']})")
        print(f"  device:      {bundle['udid']}")
        print(f"  App ID:      {bundle['app_id_id']}")
        print(f"  certificate: serial {bundle['certificate_serial']}")
        print(f"  identity:    {bundle['p12_path']}")
        print(f"  profile:     {bundle['profile_path']}")
    return 0


def _cmd_sign(args: argparse.Namespace) -> int:
    try:
        bundle = provision.load_bundle()
        result = signing.sign_ipa(
            args.ipa,
            args.output,
            p12_path=bundle["p12_path"],
            p12_password=bundle["p12_password"],
            profile_path=bundle["profile_path"],
            bundle_id=args.bundle_id or bundle["bundle_id"],
            remove_extensions=args.remove_extensions,
            remove_uisd=args.remove_device_restrictions,
            legacy_sha1=args.legacy_sha1,
        )
    except (gsa.GsaError, signing.SigningError) as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Signing failed", exc)
        return 1

    if args.json:
        _emit(args, {"status": "signed", "output": result["output"]})
    else:
        _ok(f"Signed → {result['output']}")
    return 0


def _progress_to_stderr(bundle_id: str | None = None, json_mode: bool = True):
    """--json: NDJSON progress events on stderr; human: animated status line."""
    def emit(phase_name: str, percent: Any = None, step: str | None = None) -> None:
        if json_mode:
            event = {"event": "progress", "phase": phase_name, "percent": percent, "step": step}
            if bundle_id:
                event["bundle_id"] = bundle_id
            print(json.dumps(event), file=sys.stderr, flush=True)
        else:
            _PROGRESS.emit(phase_name, percent, step)
    return emit


_SIDELOAD_ERRORS = (
    gsa.GsaError,
    developer.DeveloperServicesError,
    signing.SigningError,
    sideload.SideloadError,
    ValueError,
)


def _candidate_ipas(root: Path | None = None) -> list[Path]:
    """IPAs in the usual places (and one level deeper under projects/).

    Roots can be overridden/augmented with ``IPASIDE_IPA_DIRS`` (os.pathsep
    separated) so the scan follows wherever the user keeps their IPAs.
    """
    if root is not None:
        roots = [root]
        depth = 1
    else:
        env = os.environ.get("IPASIDE_IPA_DIRS")
        if env:
            roots = [Path(p).expanduser() for p in env.split(os.pathsep) if p.strip()]
        else:
            roots = [Path.cwd(), Path.home() / "Downloads", Path.home() / "Desktop",
                     Path.home() / "Documents"]
        depth = 2
    seen: set[Path] = set()
    found: list[Path] = []
    stack = [(r, 0) for r in roots if r.is_dir()]
    while stack:
        current, level = stack.pop()
        try:
            entries = list(current.iterdir())
        except OSError:
            continue
        for entry in entries:
            if entry.is_file() and entry.suffix.lower() == ".ipa":
                try:
                    key = entry.resolve()
                except OSError:
                    continue
                if key in seen:
                    continue
                seen.add(key)
                found.append(entry)
            elif entry.is_dir() and level < depth:
                stack.append((entry, level + 1))
    found.sort(key=lambda p: p.stat().st_mtime if p.exists() else 0, reverse=True)
    return found[:40]


def _browse_for_ipa(start: Path | None = None) -> str | None:
    """A tiny interactive file browser for picking an .ipa without a GUI dialog."""
    current = (start or Path.home()).expanduser().resolve()
    while True:
        try:
            entries = sorted(current.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower()))
        except OSError as exc:
            _fail("Cannot read folder", exc)
            return None
        dirs = [p for p in entries if p.is_dir() and not p.name.startswith(".")]
        ipas = [p for p in entries if p.is_file() and p.suffix.lower() == ".ipa"]
        shown = []
        if current.parent != current:
            shown.append(("..", current.parent, "dir"))
        for p in dirs[:60]:
            shown.append((p.name + "/", p, "dir"))
        for p in ipas:
            shown.append((p.name, p, "ipa"))
        print(f"\n{_C_BOLD}{current}{_C_RESET}")
        if not shown:
            _note("empty folder")
        for index, (label, _path, kind) in enumerate(shown, 1):
            mark = _C_DIM if kind == "dir" else _C_GREEN
            print(f"  {index:>2}) {mark}{label}{_C_RESET}")
        print(f"  {_C_DIM}type a path to jump, or q to quit{_C_RESET}")
        try:
            raw = input(f"{_C_DIM}> {_C_RESET}").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return None
        if raw.lower() in ("q", "quit", "exit"):
            return None
        if raw == "":
            continue
        # direct path typed
        if raw.startswith("/") or raw.startswith("~") or raw.startswith("."):
            candidate = Path(raw).expanduser()
            if candidate.is_file() and candidate.suffix.lower() == ".ipa":
                return str(candidate.resolve())
            if candidate.is_dir():
                current = candidate.resolve()
                continue
            _note("not a folder or .ipa")
            continue
        if raw.isdigit() and 1 <= int(raw) <= len(shown):
            label, path, kind = shown[int(raw) - 1]
            if kind == "ipa":
                return str(path.resolve())
            current = path.resolve()
            continue
        _note("invalid choice")


def _pick_ipa_interactively() -> str | None:
    candidates = _candidate_ipas()
    if not candidates:
        _note("No .ipa found in the usual places.")
        try:
            raw = input(f"{_C_DIM}browse? [Y/n] or paste a path:{_C_RESET} ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return None
        if raw.lower() in ("n", "no"):
            return None
        if raw.lower() in ("", "y", "yes"):
            return _browse_for_ipa()
        return str(Path(raw).expanduser())
    print(f"{_C_BOLD}Choose an .ipa to sideload{_C_RESET}")
    for index, path in enumerate(candidates, 1):
        try:
            when = datetime.datetime.fromtimestamp(path.stat().st_mtime).strftime("%Y-%m-%d")
        except OSError:
            when = ""
        print(f"  {index:>2}) {path.name}  {_C_DIM}{path.parent}  {when}{_C_RESET}")
    while True:
        try:
            raw = input(f"{_C_DIM}number (Enter = 1, q = quit):{_C_RESET} ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return None
        if raw.lower() in ("q", "quit", "exit"):
            return None
        if raw == "":
            return str(candidates[0])
        if raw.isdigit() and 1 <= int(raw) <= len(candidates):
            return str(candidates[int(raw) - 1])
        _note("invalid choice")


def _pick_device_interactively() -> str | None:
    entries = device.list_devices()
    udids: list[str] = []
    for entry in entries:
        serial = entry.get("serial")
        if serial and str(serial) not in udids:
            udids.append(str(serial))
    if not udids:
        _fail("No device connected", "connect an iPhone (or pass --udid)")
        return None
    if len(udids) == 1:
        return udids[0]
    print(f"{_C_BOLD}Choose a device{_C_RESET}")
    for index, serial in enumerate(udids, 1):
        kinds = ", ".join(
            str(e.get("connection_type")) for e in entries if e.get("serial") == serial
        )
        print(f"  {index:>2}) {serial}  {_C_DIM}{kinds}{_C_RESET}")
    while True:
        try:
            raw = input(f"{_C_DIM}number (Enter = 1, q = quit):{_C_RESET} ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            return None
        if raw.lower() in ("q", "quit", "exit"):
            return None
        if raw == "":
            return udids[0]
        if raw.isdigit() and 1 <= int(raw) <= len(udids):
            return udids[int(raw) - 1]
        _note("invalid choice")


def _cmd_sideload(args: argparse.Namespace) -> int:
    """One-shot: provision -> sign -> install, streaming phases/progress on stderr."""
    if not args.ipa:
        if args.json or not sys.stdin.isatty():
            _fail("Missing IPA", "pass an .ipa path, e.g. ipaside sideload app.ipa")
            return 2
        chosen = _pick_ipa_interactively()
        if not chosen:
            return 1
        args.ipa = chosen
    # A directory means "pick an .ipa inside it".
    if Path(args.ipa).is_dir():
        candidates = _candidate_ipas(Path(args.ipa))
        if not candidates:
            _fail("No .ipa in that folder", args.ipa)
            return 1
        if not sys.stdin.isatty():
            args.ipa = str(candidates[0])
        else:
            print(f"{_C_BOLD}Choose an .ipa in {args.ipa}{_C_RESET}")
            for index, path in enumerate(candidates, 1):
                print(f"  {index:>2}) {path.name}")
            while True:
                try:
                    raw = input(f"{_C_DIM}number (Enter = 1):{_C_RESET} ").strip()
                except (EOFError, KeyboardInterrupt):
                    print()
                    return 1
                if raw == "":
                    args.ipa = str(candidates[0]); break
                if raw.isdigit() and 1 <= int(raw) <= len(candidates):
                    args.ipa = str(candidates[int(raw)-1]); break
                _note("invalid choice")
    if not args.udid and not args.json and sys.stdin.isatty():
        try:
            devices = device.list_devices()
        except Exception:  # noqa: BLE001
            devices = []
        udids = []
        for entry in devices:
            serial = entry.get("serial")
            if serial and str(serial) not in udids:
                udids.append(str(serial))
        if len(udids) > 1:
            chosen_dev = _pick_device_interactively()
            if not chosen_dev:
                return 1
            args.udid = chosen_dev
    try:
        result = sideload.run_sideload(
            args.ipa,
            args.udid,
            bundle_id=args.bundle_id,
            allow_other_platform=args.allow_other_platform,
            display_name=args.name,
            bundle_version=args.set_version,
            dylibs=args.dylib,
            weak_dylibs=args.weak_dylibs,
            inject_into_extensions=args.inject_extensions,
            remove_extensions=args.remove_extensions,
            remove_uisd=args.remove_device_restrictions,
            enable_file_sharing=args.enable_file_sharing,
            keep_signed=args.keep_signed,
            signed_dir=args.signed_dir,
            on_progress=_progress_to_stderr(json_mode=args.json),
        )
    except _SIDELOAD_ERRORS as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Sideload failed", exc, _hint_for(str(exc)))
        return 1

    if args.json:
        _emit(args, result)
    else:
        _print_banner()
        _ok(f"Sideloaded {result['name']} ({result['bundle_id']})")
        _note("Trust the developer on your device to launch it.")
    return 0


def _livecontainer_host(args: argparse.Namespace) -> str:
    """The installed LiveContainer's bundle id, or a ``LiveContainerError`` saying why not."""
    state = livecontainer.status(args.udid)
    if not state.get("installed"):
        raise livecontainer.LiveContainerError(
            "LiveContainer is not installed on this device. Run `livecontainer --setup` "
            "first; apps run inside it, so it has to be there before one can be added."
        )
    return str(state["bundle_id"])


def _cmd_livecontainer(args: argparse.Namespace) -> int:
    """Report on LiveContainer, set it up, or manage the apps running inside it."""
    if args.add:
        host = _livecontainer_host(args)
        result = livecontainer.install_guest(
            args.add, host, args.udid, on_progress=_progress_to_stderr(json_mode=args.json)
        )
        if args.json:
            _emit(args, result)
            return 0
        _ok(f"Added {result['name']} to LiveContainer ({result['bundle_id']})")
        _note(
            "It is not installed on the phone, so it uses none of your three app slots. "
            "Open LiveContainer and it will sign it on first launch."
        )
        return 0

    if args.remove:
        host = _livecontainer_host(args)
        livecontainer.remove_guest(args.remove, host, args.udid)
        if args.json:
            _emit(args, {"status": "removed", "bundle_id": args.remove})
        else:
            _ok(f"Removed {args.remove} from LiveContainer")
        return 0

    if args.apps:
        host = _livecontainer_host(args)
        guests = livecontainer.guest_apps(host, args.udid)
        if args.json:
            _emit(args, guests)
            return 0
        if not guests:
            _note("No apps inside LiveContainer yet. Add one with --add <ipa>.")
            return 0
        print(f"  {_C_BOLD}{len(guests)} app(s){_C_RESET} {_C_DIM}inside LiveContainer, using no app slots:{_C_RESET}")
        for guest in guests:
            print(f"      {_C_DIM}{guest['bundle_id']}{_C_RESET}")
        return 0

    if args.download and not args.setup:
        result = livecontainer.download(
            args.dir, variant=args.variant, on_progress=_progress_to_stderr(json_mode=args.json)
        )
        if args.json:
            _emit(args, result)
        else:
            _ok(f"Downloaded {result['asset_name']} ({result['version']}) → {result['path']}")
        return 0

    if args.setup:
        ipa_path = args.ipa
        if not ipa_path:
            ipa_path = livecontainer.download(
                args.dir, variant=args.variant, on_progress=_progress_to_stderr(json_mode=args.json)
            )["path"]
        result = livecontainer.setup(
            ipa_path,
            args.udid,
            keep_signed=args.keep_signed,
            signed_dir=args.signed_dir,
            automatic_certificate=not args.manual_certificate,
            on_progress=_progress_to_stderr(json_mode=args.json),
        )
        if args.json:
            _emit(args, result)
            return 0

        certificate = result["certificate"]
        _ok(f"Installed LiveContainer ({result['bundle_id']})")
        if certificate.get("automatic"):
            _note(
                "Open it once and it will import the signing certificate itself, "
                "then JIT-less mode is ready."
            )
        elif certificate.get("seeded"):
            _note(certificate["instructions"])
        else:
            _fail("The certificate could not be delivered", certificate.get("error"))
            _note(certificate["instructions"])

        pairing = result.get("pairing") or {}
        if result.get("has_sidestore"):
            if pairing.get("paired"):
                _note(
                    "This build carries SideStore, and it has been given this PC's "
                    "pairing file, so it can refresh apps on the phone itself."
                )
            else:
                _note(
                    "This build carries SideStore, but the pairing file could not be "
                    f"delivered ({pairing.get('error')}), so on-device refresh will not "
                    "work until it is."
                )
            _note(SIDESTORE_SHORTCUT_NOTE)
        return 0

    result = livecontainer.status(args.udid)
    if args.json:
        _emit(args, result)
        return 0

    if not result["installed"]:
        _note("LiveContainer is not installed. Run with --setup to install it.")
        return 0
    version = result.get("version") or ""
    suffix = f" {version}" if version else ""
    print(f"  {_C_BOLD}LiveContainer{suffix}{_C_RESET}  {_C_DIM}{result['bundle_id']}{_C_RESET}")
    if result.get("certificate_pending"):
        _note("A certificate import is waiting; open LiveContainer to complete it.")
    elif not result.get("launched"):
        _note("Installed but never opened.")
    else:
        _note("Set up. Its certificate is stored in the shared app group.")
    guests = livecontainer.guest_apps(str(result["bundle_id"]), args.udid)
    _note(f"{len(guests)} app(s) inside it, using no app slots.")
    for guest in guests:
        print(f"      {_C_DIM}{guest['bundle_id']}{_C_RESET}")
    return 0


_JAILBREAK_ERRORS = (
    jailbreak.JailbreakError,
    gsa.GsaError,
    developer.DeveloperServicesError,
    signing.SigningError,
    sideload.SideloadError,
    device.DeviceError,
    ValueError,
)


def _jailbreak_advice(args: argparse.Namespace) -> tuple[dict[str, Any], str]:
    """Evaluate one device and pin its resolved UDID for any following install."""
    compat = jailbreak.fetch_compat()
    try:
        info = device.get_device_info(args.udid)
    except device.DeviceError:
        raise
    except Exception as exc:  # noqa: BLE001 - usbmuxd down, device locked, refused
        raise jailbreak.JailbreakError(
            "Couldn't read the device. Connect an iPhone over USB, unlock it and "
            "trust this computer, then try again."
        ) from exc
    resolved_udid = info.get("UniqueDeviceID") or args.udid
    if not isinstance(resolved_udid, str) or not resolved_udid:
        raise jailbreak.JailbreakError(
            "Couldn't identify the connected device. Reconnect it over USB and try again."
        )
    advice = jailbreak.advise(
        info.get("ProductType"),
        info.get("ProductVersion"),
        compat,
        info.get("BuildVersion"),
    )
    return advice, resolved_udid


def _cmd_jailbreak(args: argparse.Namespace) -> int:
    """Advise on jailbreak compatibility, or download / install Dopamine.

    With no flags it reads the connected device and reports whether Dopamine fits it.
    ``--install`` signs and installs Dopamine (downloading the latest IPA unless ``--ipa``
    is given); ``--download`` only fetches the IPA.
    """
    try:
        if args.download and not args.install:
            result = jailbreak.download(args.dir, on_progress=_progress_to_stderr(json_mode=args.json))
            if args.json:
                _emit(args, result)
            else:
                _ok(f"Downloaded {result['asset_name']} ({result['version']}) → {result['path']}")
            return 0

        if args.install:
            advice, resolved_udid = _jailbreak_advice(args)
            if not advice.get("can_install"):
                raise jailbreak.JailbreakError(
                    str(advice.get("summary") or "Dopamine does not support this device.")
                )
            result = jailbreak.install(
                resolved_udid,
                ipa_path=args.ipa,
                keep_signed=args.keep_signed,
                signed_dir=args.signed_dir,
                on_progress=_progress_to_stderr(json_mode=args.json),
            )
            if args.json:
                _emit(args, result)
            else:
                _ok(f"Installed {jailbreak.TOOL['name']} ({result['bundle_id']})")
                _note(
                    "Open it on your iPhone to run the jailbreak. iPASide will refresh it "
                    "before its 7-day profile expires."
                )
            return 0

        # Default: advise on the connected device. The compatibility list is fetched
        # live (no bundled fallback), so a network failure here surfaces as an error the
        # UI answers with a Retry button.
        advice, _resolved_udid = _jailbreak_advice(args)
        if args.json:
            _emit(args, advice)
        else:
            print(advice["summary"])
        return 0
    except _JAILBREAK_ERRORS as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Jailbreak command failed", exc)
        return 1


def _cmd_installs(args: argparse.Namespace) -> int:
    records = refresh.records()
    if args.json:
        _emit(args, records)
        return 0
    if not records:
        _note("No sideloaded apps recorded yet.")
        return 0
    width = max(len(str(r.get("name") or "?")) for r in records)
    for record in records:
        days = record.get("days_left")
        name = str(record.get("name") or "?")
        if record.get("expired"):
            state = f"{_C_RED}expired{_C_RESET}"
        elif days is not None:
            days_s = f"{days:g}d left"
            state = (f"{_C_RED}{days_s}{_C_RESET}" if days <= 2
                     else f"{_C_DIM}{days_s}{_C_RESET}")
        else:
            state = f"{_C_DIM}unknown{_C_RESET}"
        print(f"  {state:<10} {_C_BOLD}{name:<{width}}{_C_RESET}  {_C_DIM}{record['bundle_id']}{_C_RESET}")
    print(f"\n  {_C_DIM}{len(records)} app(s) sideloaded{_C_RESET}")
    return 0


def _cmd_refresh(args: argparse.Namespace) -> int:
    if args.bundle_id:
        targets = [r for r in refresh.records() if r["bundle_id"] == args.bundle_id]
        if not targets:
            msg = f"'{args.bundle_id}' is not a recorded sideload."
            _emit(args, {"status": "error", "error": msg}) if args.json else print(msg)
            return 1
    elif args.all:
        targets = refresh.records()
    else:
        targets = refresh.due(args.within)

    results: list[dict[str, Any]] = []
    for entry in targets:
        try:
            res = sideload.refresh_record(
                entry,
                on_progress=_progress_to_stderr(entry["bundle_id"], json_mode=args.json),
                keep_signed=args.keep_signed,
                signed_dir=args.signed_dir,
            )
            results.append({
                "bundle_id": entry["bundle_id"],
                "status": "installed",
                "expires_at": res.get("expires_at"),
            })
        except Exception as exc:  # keep going; report per-app outcome
            results.append({"bundle_id": entry["bundle_id"], "status": "error", "error": str(exc)})

    summary = {"status": "done", "count": len(results), "refreshed": results}
    if args.json:
        _emit(args, summary)
    else:
        if not results:
            _note("Nothing to refresh.")
        for res in results:
            if res["status"] == "installed":
                _ok(f"Refreshed {res['bundle_id']}")
            else:
                _fail(f"Refresh failed - {res['bundle_id']}", res.get("error"))
    return 0


def _mb(count: int) -> str:
    return f"{count / (1 << 20):.1f} MB"


def _cmd_signed(args: argparse.Namespace) -> int:
    if args.clean:
        result = sideload.clean_signed(args.dir)
        if args.json:
            _emit(args, result)
        else:
            _ok(f"Removed {result['removed']} signed IPA(s), freed {_mb(result['bytes_freed'])}")
            _note(result["directory"])
        return 0

    report = sideload.signed_status(args.dir)
    if args.json:
        _emit(args, report)
        return 0
    print(f"  Signed IPAs kept: {_C_BOLD}{report['count']}{_C_RESET} {_C_DIM}({_mb(report['bytes'])}){_C_RESET}")
    print(f"      {_C_DIM}{report['directory']}{_C_RESET}")
    for item in report["files"]:
        print(f"      {item['name']}  {_C_DIM}{_mb(item['bytes'])}  {item['modified']}{_C_RESET}")
    return 0


def _cmd_forget(args: argparse.Namespace) -> int:
    removed = refresh.forget(args.bundle_id)
    if args.json:
        _emit(args, {"ok": removed, "bundle_id": args.bundle_id})
    else:
        _ok("Removed from registry") if removed else _note("Not in the registry.")
    return 0


def _cmd_slots(args: argparse.Namespace) -> int:
    """What one Apple ID's developer account holds. Reporting only; see revoke-cert."""
    report = account.overview(args.email)
    if args.json:
        _emit(args, report)
        return 0

    team = report["team"]
    print(f"  {_C_BOLD}Apple ID{_C_RESET} {_C_CYAN}{report['account']}{_C_RESET}")
    print(f"  {_C_BOLD}Team{_C_RESET} {team['name']} {_C_DIM}({team['id']}) - {team['type']}{_C_RESET}")

    certs = report["certificates"]
    print()
    print(f"  {_C_BOLD}Certificates ({len(certs)}){_C_RESET}")
    for cert in certs:
        # Whose it is decides whether revoking it is housekeeping or sabotage.
        if cert["in_use_here"]:
            owner = "iPASide (this machine - signing with it now)"
            mark = _CHECK_MARK.get("ok", "")
        elif cert["ours"]:
            owner = "iPASide (another machine)"
            mark = _CHECK_DIM
        else:
            owner = cert["machine"] or "unknown"
            mark = _CHECK_DIM
        print(f"    {mark} {cert['serial']}  {_C_DIM}expires {cert['expires']} · {cert['type']} · {owner}{_C_RESET}")

    print()
    # Deliberately not printed as "N/10": the ten is a ceiling on new registrations over a
    # rolling week, and this is how many exist. Shown as one fraction it reads as spare
    # capacity that may already be spent, which is exactly how somebody gets surprised.
    print(f"  {_C_BOLD}App IDs registered: {report['registered_app_ids']}{_C_RESET}")
    for app_id in report["app_ids"]:
        print(f"    {app_id['identifier']:<45} {_C_DIM}{app_id['name'] or ''}{_C_RESET}")
    print(
        f"      {_C_DIM}(Apple also allows about {report['weekly_app_id_limit']} *new* "
        "identifiers per 7 days, counted separately from the list above.)"
    )

    print()
    devices_ids = report["devices"]
    print(f"  {_C_BOLD}Devices ({len(devices_ids)}){_C_RESET}")
    for device_entry in devices_ids:
        print(f"    {device_entry['udid']}  {_C_DIM}{device_entry['name'] or ''}{_C_RESET}")
    return 0


def _cmd_delete_app_id(args: argparse.Namespace) -> int:
    result = account.delete_app_id(args.app_id, args.email)
    if args.json:
        _emit(args, result)
        return 0
    _ok(f"Deleted {result['identifier'] or result['deleted']}")
    # Said every time, because the obvious reading is the wrong one: the identifier is
    # free to re-use, but Apple's ~10 per week counts registrations, not what exists.
    _note(
        "That frees the identifier, but not one of this week's registrations - Apple "
        "counts those over a rolling seven days."
    )
    return 0


def _cmd_revoke_cert(args: argparse.Namespace) -> int:
    """Revoke a certificate, having said whose it was."""
    result = account.revoke_certificate(args.serial, args.email)
    if args.json:
        _emit(args, result)
        return 0
    _ok(f"Revoked {result['revoked']} ({result['machine'] or 'unknown machine'})")
    if result["invalidates_local_apps"]:
        _note(
            "That is the certificate iPASide signs with on this machine, so the apps it "
            "installed will stop opening. Sideload or refresh one to get a new one, and "
            "LiveContainer will be handed the replacement automatically."
        )
    elif not result["was_ours"]:
        _note(
            "That certificate belonged to another tool, so anything it signed will stop "
            "opening until that tool issues itself a new one."
        )
    return 0


def _cmd_inspect(args: argparse.Namespace) -> int:
    try:
        report = ipa.inspect(args.ipa)
    except ipa.IpaError as exc:
        # ipa.inspect phrases these; --json callers want the same text in a field.
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Inspect failed", exc)
        return 1
    if args.json:
        _emit(args, report)
        return 0
    print(f"  {_C_BOLD}{report['display_name']}{_C_RESET}  {_C_DIM}({report['bundle_id']}){_C_RESET}")
    print(f"      bundle: {report['app_bundle']}   executable: {report['executable']}")
    print(
        f"      version: {report['version']} (build {report['build']})   "
        f"min iOS: {report['minimum_os']}   built-with SDK: {report['platform_sdk']}"
    )
    print(
        f"      frameworks: {len(report['frameworks'])}   "
        f"app extensions: {len(report['extensions'])}   watch app: {report['has_watch_app']}"
    )
    print(
        f"      embedded provision: {report['has_embedded_provision']}   "
        f"FairPlay hint (SC_Info): {report['has_sc_info']}"
    )
    if report["extensions"]:
        print(f"      {_C_DIM}extensions (stripped for free accounts):{_C_RESET}")
        for ext in report["extensions"]:
            print(f"          - {ext}")
    return 0


def _cmd_prepare(args: argparse.Namespace) -> int:
    report = ipa.prepare(
        args.ipa,
        args.output,
        strip_extensions=args.strip_extensions,
        remove_device_restrictions=args.remove_device_restrictions,
        bundle_id=args.bundle_id,
        display_name=args.name,
        bundle_version=args.set_version,
        compresslevel=args.compress,
        keep_workdir=args.keep_dir,
    )
    if args.json:
        _emit(args, report)
        return 0
    _ok(f"Prepared {report['app_bundle']}")
    print(f"  removed: {', '.join(report['removed']) if report['removed'] else 'nothing'}")
    print(f"  Info.plist changes: {report['info_plist_changes'] or 'none'}")
    encrypted = report["encrypted"]
    print(f"  FairPlay encrypted: {encrypted}")
    if encrypted:
        print(f"      {_C_RED}WARNING{_C_RESET} the main binary is still encrypted; it must be decrypted")
        print(f"      before signing or iOS will kill it on launch.")
    print(f"  remaining frameworks: {len(report['remaining_frameworks'])}")
    if report["output"]:
        _ok(f"Output IPA {report['output']}")
    if report["workdir"]:
        _note(f"workdir kept: {report['workdir']}")
    return 0


def _read_credentials(args: argparse.Namespace) -> tuple[str | None, str | None]:
    """Resolve (email, password) from a creds file, env var, or prompt.

    The password is never taken from the command line (it would leak into shell
    history and process listings).
    """
    if args.creds_file:
        data = json.loads(Path(args.creds_file).read_text(encoding="utf-8"))
        return data.get("email"), data.get("password")
    email = args.email
    password = os.environ.get("IPASIDE_APPLE_PASSWORD")
    if email and not password:
        password = getpass.getpass("Apple ID password: ")
    return email, password


def _cmd_login(args: argparse.Namespace) -> int:
    if args.logout:
        # --logout alone signs every account out; with --email, only that one, so a
        # second Apple ID can be dropped without disturbing the rest.
        result = gsa.logout(args.email if args.email else None)
        if args.json:
            _emit(args, result)
        else:
            removed = ", ".join(result.get("removed") or []) or "nothing"
            _ok(f"Signed out: {removed}")
            if result.get("active"):
                _note(f"Active account is now {result['active']}.")
        return 0

    if args.use:
        result = gsa.use_account(args.use)
        if args.json:
            _emit(args, result)
        else:
            _ok(f"Now using {result['email']}")
        return 0

    if args.accounts:
        state = gsa.accounts()
        if args.json:
            _emit(args, state)
        elif not state["accounts"]:
            _note("No Apple IDs signed in.")
        else:
            for account in state["accounts"]:
                mark = f"{_C_GREEN}●{_C_RESET}" if account["active"] else f"{_C_DIM}○{_C_RESET}"
                team = account.get("team_id") or "-"
                print(f"  {mark} {account['email']:38s} {_C_DIM}team {team}{_C_RESET}")
            print(f"\n  {_C_DIM}{len(state['accounts'])} account(s){_C_RESET}")
        return 0

    if args.status:
        state = gsa.status()
        if args.json:
            _emit(args, state)
        elif state["authenticated"]:
            others = state.get("account_count", 1) - 1
            extra = f"{_C_DIM}, {others} other signed in{_C_RESET}" if others > 0 else ""
            print(f"  {_C_GREEN}✓{_C_RESET} signed in as {_C_BOLD}{state.get('email')}{_C_RESET} "
                  f"{_C_DIM}(adsid {state.get('adsid')}){_C_RESET} {extra}")
        else:
            _note("Not signed in.")
        return 0

    email, password = _read_credentials(args)
    if not email or not password:
        _fail("Missing credentials", "--creds-file, or --email plus IPASIDE_APPLE_PASSWORD")
        return 2

    try:
        if args.code:
            result = gsa.complete_2fa(email, password, args.code)
        else:
            result = gsa.begin_login(email, password)
    except gsa.GsaError as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Login failed", exc)
        return 1

    if args.json:
        _emit(args, result)
        return 0
    if result["status"] == "2fa_required":
        print(f"Two-factor authentication required (method: {result['method']}).")
        if result.get("notice"):
            print(result["notice"])
        elif result["method"] == "sms":
            print("A verification code was sent to your trusted phone number.")
        else:
            print("A verification code was sent to your trusted device(s).")
        print("Re-run the same command adding:  --code <the 6-digit code>")
    else:
        _ok(f"Authenticated. adsid: {result.get('adsid')}")
    return 0


def _cmd_version(args: argparse.Namespace) -> int:
    if args.json:
        _emit(args, {"version": __version__})
    else:
        print(f"iPASide Engine {__version__}")
    return 0


def _cmd_resolve_tweak(args: argparse.Namespace) -> int:
    """Resolve a .deb or .dylib to the injectable dylib(s) it provides."""
    try:
        dylibs = tweak.resolve(args.path)
    except Exception as exc:
        if args.json:
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            _fail("Resolve failed", exc)
        return 1
    if args.json:
        _emit(args, {"dylibs": dylibs})
    else:
        for item in dylibs:
            arches = ",".join(item["arches"]) or "?"
            print(f"  {_C_BOLD}{item['name']}{_C_RESET}  {_C_DIM}[{arches}]  {item['path']}{_C_RESET}")
    return 0


# --------------------------------------------------------------------------- #
# Persistent server mode
#
# `serve` keeps one warm engine process alive so the desktop app doesn't pay the
# Python-startup + import cost on every action. It speaks newline-delimited JSON
# over stdio: one request object per stdin line, and per line on stdout either a
# progress frame or a final result frame, both tagged with the request id.
#
# Unsolicited ``{"type":"event","name":"devices","data":[...]}`` frames are also
# written on this channel when usbmux reports Attached/Detached. They carry no
# request id. Stdout writes are locked because the Listen thread and a command
# can emit at the same time.
# --------------------------------------------------------------------------- #
class _StdoutChannel:
    """Thread-safe NDJSON writer for the serve stdout channel."""

    def __init__(self, stream: Any) -> None:
        self._stream = stream
        self._lock = threading.Lock()

    def write_frame(self, frame: dict[str, Any]) -> None:
        line = json.dumps(frame, default=_json_default) + "\n"
        with self._lock:
            self._stream.write(line)
            self._stream.flush()


class _ProgressWriter(io.TextIOBase):
    """A stderr stand-in that forwards each line written during a command as an
    id-tagged progress frame (commands stream progress via ``print(..., file=sys.stderr)``)."""

    def __init__(self, write_frame: Any, request_id: Any) -> None:
        self._write_frame = write_frame
        self._id = request_id
        self._pending = ""

    def write(self, text: str) -> int:
        self._pending += text
        while "\n" in self._pending:
            line, self._pending = self._pending.split("\n", 1)
            line = line.strip()
            if line:
                self._write_frame({"id": self._id, "type": "progress", "line": line})
        return len(text)

    def flush(self) -> None:  # pragma: no cover - nothing buffered downstream
        pass


def _run_request(request: dict[str, Any], write_frame: Any) -> None:
    """Run one command for the server: capture its JSON result, stream progress."""
    request_id = request.get("id")
    raw_args = list(request.get("args") or [])
    env = request.get("env") or {}

    saved_env: dict[str, str | None] = {}
    for key, value in env.items():
        if isinstance(value, str):
            saved_env[key] = os.environ.get(key)
            os.environ[key] = value

    out_buffer = io.StringIO()
    old_stdout, old_stderr = sys.stdout, sys.stderr
    ok, data, error = False, None, None
    try:
        namespace = build_parser().parse_args([*raw_args, "--json"])
        if namespace.command == "serve":
            raise ValueError("nested serve is not allowed")
        sys.stdout = out_buffer
        sys.stderr = _ProgressWriter(write_frame, request_id)
        ok = (dispatch(namespace) or 0) == 0
        text = out_buffer.getvalue().strip()
        if text:
            try:
                data = json.loads(text)
            except json.JSONDecodeError:
                data = None
        if not ok:
            error = (data.get("error") if isinstance(data, dict) else None) or text or "engine error"
    except SystemExit as exc:
        error = f"invalid request (argument error: {exc.code})"
    except Exception as exc:  # never let one bad command kill the server
        error = str(exc)
    finally:
        sys.stdout, sys.stderr = old_stdout, old_stderr
        for key, previous in saved_env.items():
            if previous is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = previous

    write_frame({"id": request_id, "type": "result", "ok": ok, "data": data, "error": error})


def _cmd_serve(args: argparse.Namespace) -> int:
    channel = _StdoutChannel(sys.stdout)
    reconfigure = getattr(sys.stdin, "reconfigure", None)
    if reconfigure is not None:
        try:
            reconfigure(encoding="utf-8")
        except (ValueError, OSError):
            pass
    stop = threading.Event()
    watcher = threading.Thread(
        target=device.run_usbmux_watch,
        args=(channel.write_frame, stop),
        name="usbmux-listen",
        daemon=True,
    )
    channel.write_frame({"type": "ready", "version": __version__})
    watcher.start()
    try:
        for raw_line in sys.stdin:
            raw_line = raw_line.strip()
            if not raw_line:
                continue
            try:
                request = json.loads(raw_line)
            except json.JSONDecodeError:
                continue
            if request.get("type") == "shutdown":
                break
            _run_request(request, channel.write_frame)
    finally:
        stop.set()
    return 0


def _pairing_udid(args: argparse.Namespace) -> str:
    """The device a pairing command is about.

    ``--udid`` is enough: the lockdown record and any stored pairing file live
    on this PC, so export and import must not refuse to run because the phone is
    unplugged. Creating Remote Pairing keys and placing the file still talk to
    the device, and those paths fail later if it cannot be reached.
    """
    if args.udid:
        return args.udid.strip()
    return device.resolve_serial(None)


def _print_pairing_status(report: dict[str, Any]) -> None:
    print(f"  {_C_BOLD}Pairing file{_C_RESET} {_C_DIM}for {report.get('udid') or '?'}{_C_RESET}")
    for line in _pairing_status_lines(report):
        print(f"      {_C_DIM}{line}{_C_RESET}")
    consumers = report.get("consumers") or []
    if report.get("device_reachable") and consumers:
        print(f"      {_C_BOLD}{len(consumers)} supported app(s){_C_RESET} {_C_DIM}installed:{_C_RESET}")
        for consumer in consumers:
            extra = " (needs Remote Pairing)" if consumer.get("needs_rppairing") else ""
            print(
                f"          {consumer.get('app_name') or consumer.get('name')}  "
                f"{consumer.get('bundle_id')}  {consumer.get('filename')}{_C_DIM}{extra}{_C_RESET}"
            )


def _pairing_status_lines(report: dict[str, Any]) -> list[str]:
    """The dim detail block under the pairing headline."""
    source = report.get("source") or "none"
    if source == "imported":
        lines = [f"Source: imported ({report.get('imported_path')})"]
    elif source == "lockdown":
        lines = ["Source: this PC's USB pairing record"]
    else:
        lines = ["Source: none"]
    lines.append(f"USB lockdown keys: {'yes' if report.get('has_lockdown') else 'no'}")
    lines.append(f"Remote Pairing keys: {'yes' if report.get('has_rppairing') else 'no'}")
    if report.get("error"):
        lines.append(str(report["error"]))
    elif report.get("note"):
        lines.append(str(report["note"]))
    if not report.get("device_reachable"):
        reason = report.get("device_error") or "the device could not be listed"
        lines.append(f"Installed apps: not listed ({reason})")
    elif not report.get("consumers"):
        lines.append(
            "No supported apps are installed. Sideload EscapeOS, SideStore, "
            "AltStore, LiveContainer, or StikDebug, then place the file."
        )
    return lines


def _print_pairing_delivery(result: dict[str, Any]) -> None:
    placed = result.get("placed") or []
    if not placed:
        _note(
            "No supported apps are installed on this iPhone. Sideload EscapeOS, "
            "SideStore, AltStore, LiveContainer, or StikDebug first."
        )
        return
    print(f"  {_C_BOLD}{len(placed)} app(s){_C_RESET} {_C_DIM}got the pairing file:{_C_RESET}")
    for item in placed:
        name = item.get("name") or item.get("bundle_id")
        mark = _CHECK_MARK.get("ok", "") if item.get("placed") else _CHECK_MARK.get("fail", "")
        if item.get("placed"):
            body = f"{name}: wrote {item.get('filename')} ({item.get('bytes')} bytes)"
        else:
            body = f"{name}: failed ({item.get('error')})"
        print(f"      {mark}  {body}")
        if item.get("warning"):
            print(f"          {_C_DIM}{item['warning']}{_C_RESET}")
    if result.get("note") and not result.get("has_rppairing"):
        _note(result["note"])


def _cmd_pairing(args: argparse.Namespace) -> int:
    udid = _pairing_udid(args)
    if args.pairing_create:
        result = pairing.create_remote_pairing(udid, udid, force=True)
        if args.json:
            _emit(args, result)
            return 0
        if result.get("created"):
            _ok(f"Created Remote Pairing keys for {udid}")
        else:
            _note(f"Remote Pairing keys already present for {udid}")
        if result.get("path"):
            print(f"      {_C_DIM}stored at {result['path']}{_C_RESET}")
        if result.get("note"):
            _note(result["note"])
        return 1 if result.get("error") or not result.get("has_rppairing") else 0
    if args.pairing_export:
        result = pairing.export_to(udid, args.pairing_export)
        if args.json:
            _emit(args, result)
            return 0
        _ok(f"Wrote {result['bytes']} bytes → {result['path']}")
        if result.get("note"):
            _note(result["note"])
        return 0
    if args.pairing_import:
        result = pairing.import_from(udid, args.pairing_import)
        if args.json:
            _emit(args, result)
            return 0
        _ok(f"Imported pairing file for {udid}")
        print(f"      {_C_DIM}stored at {result['path']}{_C_RESET}")
        if result.get("note"):
            _note(result["note"])
        return 0
    if args.deliver or args.pairing_app:
        apps_filter = [args.pairing_app] if args.pairing_app else None
        result = pairing.deliver_to_device(udid, udid, bundle_ids=apps_filter)
        if args.json:
            _emit(args, result)
            return 0
        _print_pairing_delivery(result)
        placed = result.get("placed") or []
        if not placed:
            return 0
        return 0 if all(item.get("placed") for item in placed) else 1

    report = pairing.status(udid, udid)
    if args.json:
        _emit(args, report)
        return 0
    _print_pairing_status(report)
    return 1 if report.get("error") else 0


_HANDLERS = {
    "doctor": _cmd_doctor,
    "devices": _cmd_devices,
    "device-info": _cmd_device_info,
    "apps": _cmd_apps,
    "app-icons": _cmd_app_icons,
    "developer-mode": _cmd_developer_mode,
    "anisette": _cmd_anisette,
    "apple-support": _cmd_apple_support,
    "login": _cmd_login,
    "teams": _cmd_teams,
    "provision": _cmd_provision,
    "sign": _cmd_sign,
    "sideload": _cmd_sideload,
    "pairing": _cmd_pairing,
    "livecontainer": _cmd_livecontainer,
    "jailbreak": _cmd_jailbreak,
    "installs": _cmd_installs,
    "refresh": _cmd_refresh,
    "signed": _cmd_signed,
    "forget": _cmd_forget,
    "slots": _cmd_slots,
    "delete-app-id": _cmd_delete_app_id,
    "revoke-cert": _cmd_revoke_cert,
    "inspect": _cmd_inspect,
    "prepare": _cmd_prepare,
    "install": _cmd_install,
    "uninstall": _cmd_uninstall,
    "version": _cmd_version,
    "resolve-tweak": _cmd_resolve_tweak,
    "serve": _cmd_serve,
}


def dispatch(args: argparse.Namespace) -> int:
    # --connection is applied through the environment rather than passed down,
    # because every device command reaches the same chokepoint (lockdown.create)
    # and threading a parameter through eight call chains to reach it would be a
    # lot of plumbing for one policy. The app sets the same variable per request,
    # which serve saves and restores around each command.
    connection = getattr(args, "connection", None)
    if connection:
        os.environ[lockdown.CONNECTION_ENV] = connection
    try:
        return _HANDLERS[args.command](args)
    finally:
        # Kill the pretty-progress timer even when a handler raises or exits early.
        _PROGRESS.stop()


def build_parser() -> argparse.ArgumentParser:
    # Shared parents so options are accepted before or after the subcommand.
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--json", action="store_true", help="emit machine-readable JSON output")

    # How to reach a device, separate from which one, because the two are not always
    # asked together: `refresh` reinstalls onto the device each record remembers, so it
    # takes no --udid, but the transport is a preference about this PC and applies to it
    # all the same.
    transport = argparse.ArgumentParser(add_help=False)
    transport.add_argument(
        "--connection",
        choices=("auto", "usb", "wifi"),
        default=None,
        help="transport to use (default: auto - prefer USB, fall back to Wi-Fi)",
    )

    # Every device-targeted command shares this, sideload included. Declaring --udid
    # on a subparser instead is what let sideload miss --connection when it was added.
    target = argparse.ArgumentParser(add_help=False, parents=[transport])
    target.add_argument(
        "--udid", default=None, help="target device UDID (default: the one connected device)"
    )

    # Shared by every command that signs an IPA (sideload + refresh), so the headless
    # auto-refresh run honours the same setting the UI sideloads with.
    signed_output = argparse.ArgumentParser(add_help=False)
    signed_output.add_argument(
        "--keep-signed", action=argparse.BooleanOptionalAction, default=False,
        help="keep the signed .ipa after installing instead of deleting it",
    )
    signed_output.add_argument(
        "--signed-dir", default=None,
        help="folder for the signed .ipa + signing scratch (default: the app's signed folder)",
    )

    parser = _RootParser(
        prog="ipaside",
        description="iPASide Engine - iOS device + Apple services core.",
        parents=[common],
    )
    parser.subcommands = [name for name in _HANDLERS if name != "serve"]
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("doctor", parents=[common], help="check the environment for sideloading readiness")
    sub.add_parser("devices", parents=[common], help="list connected iOS devices")
    sub.add_parser("device-info", parents=[common, target], help="show details for one device")
    sub.add_parser("version", parents=[common], help="print the engine version")
    sub.add_parser("serve", parents=[common], help="run a persistent engine server (JSON over stdio)")

    resolve_tweak_parser = sub.add_parser(
        "resolve-tweak", parents=[common], help="list the injectable dylib(s) in a .deb or .dylib"
    )
    resolve_tweak_parser.add_argument("path", help="path to a .deb or .dylib")

    apps_parser = sub.add_parser("apps", parents=[common, target], help="list installed apps")
    apps_parser.add_argument(
        "--type", default="User", choices=["User", "System", "Any"], help="app type filter"
    )

    icons_parser = sub.add_parser(
        "app-icons", parents=[common, target], help="home-screen icons for installed apps"
    )
    icons_parser.add_argument(
        "bundle_id", nargs="*", help="bundle ids to fetch (default: every app of --type)"
    )
    icons_parser.add_argument(
        "--type", default="User", choices=["User", "System", "Any"], help="app type filter"
    )

    dev_parser = sub.add_parser(
        "developer-mode", parents=[common, target], help="show or enable iOS Developer Mode"
    )
    dev_parser.add_argument("--enable", action="store_true", help="enable Developer Mode (reboots)")
    dev_parser.add_argument("--yes", action="store_true", help="confirm the reboot required by --enable")

    ani_parser = sub.add_parser(
        "anisette", parents=[common], help="show anisette status or generate headers"
    )
    ani_parser.add_argument(
        "--refresh", action="store_true", help="generate anisette headers (provisions on first use)"
    )

    apple_parser = sub.add_parser(
        "apple-support",
        parents=[common],
        help="show Apple's device-service status, or download/start what USB needs",
    )
    apple_parser.add_argument(
        "--download",
        action="store_true",
        help="download Apple's current iTunes installer and verify it is signed by Apple",
    )
    apple_parser.add_argument(
        "--start-service",
        action="store_true",
        help="start Apple Mobile Device Service (prompts for administrator rights)",
    )
    apple_parser.add_argument(
        "--dir",
        default=None,
        help="folder for the downloaded installer (default: the app's downloads folder)",
    )

    login_parser = sub.add_parser(
        "login", parents=[common], help="sign in to an Apple ID (GrandSlam SRP + anisette)"
    )
    login_parser.add_argument("--email", default=None, help="Apple ID email")
    login_parser.add_argument(
        "--creds-file", default=None, help="JSON file with {email, password}"
    )
    login_parser.add_argument("--code", default=None, help="2FA code (second step)")
    login_parser.add_argument("--status", action="store_true", help="show the account in use")
    login_parser.add_argument(
        "--accounts", action="store_true", help="list every signed-in Apple ID"
    )
    login_parser.add_argument(
        "--use", default=None, metavar="EMAIL",
        help="switch to an already signed-in Apple ID",
    )
    login_parser.add_argument(
        "--logout", action="store_true",
        help="sign out every account, or just --email if given",
    )

    sub.add_parser("teams", parents=[common], help="list Apple developer teams for the account")

    provision_parser = sub.add_parser(
        "provision",
        parents=[common],
        help="provision cert + device + App ID + profile for a bundle id",
    )
    provision_parser.add_argument("--bundle-id", required=True, help="app bundle identifier")
    provision_parser.add_argument(
        "--udid", default=None, help="device UDID (default: the one connected device)"
    )
    provision_parser.add_argument("--app-name", default=None, help="App ID display name")
    provision_parser.add_argument("--device-name", default=None, help="device display name")

    inspect_parser = sub.add_parser(
        "inspect", parents=[common], help="analyze an IPA without extracting it"
    )
    inspect_parser.add_argument("ipa", help="path to an .ipa file")

    sign_parser = sub.add_parser(
        "sign", parents=[common], help="sign an IPA with the provisioned identity (zsign)"
    )
    sign_parser.add_argument("ipa", help="path to the input .ipa file")
    sign_parser.add_argument("--output", "-o", required=True, help="path to the signed .ipa")
    sign_parser.add_argument("--bundle-id", default=None, help="override bundle id (default: provisioned)")
    sign_parser.add_argument(
        "--legacy-sha1",
        action="store_true",
        help="also add a SHA-1 signature, which iOS 10 and earlier need (sideload does this automatically)",
    )
    sign_parser.add_argument(
        "--remove-extensions", action=argparse.BooleanOptionalAction, default=True,
        help="remove app extensions + watch app (free-account requirement)",
    )
    sign_parser.add_argument(
        "--remove-device-restrictions", action=argparse.BooleanOptionalAction, default=True,
        help="remove UISupportedDevices",
    )

    sideload_parser = sub.add_parser(
        "sideload", parents=[common, target, signed_output],
        help="provision + sign + install an IPA in one step",
    )
    sideload_parser.add_argument(
        "ipa", nargs="?", default=None,
        help="path to the .ipa file (omit to pick one interactively)",
    )
    sideload_parser.add_argument("--bundle-id", default=None, help="override bundle id (default: the app's own)")
    sideload_parser.add_argument("--name", default=None, help="app display name (CFBundleDisplayName)")
    sideload_parser.add_argument("--set-version", default=None, help="override CFBundleShortVersionString")
    sideload_parser.add_argument("--dylib", action="append", default=None, help="inject a dylib/tweak (repeatable)")
    sideload_parser.add_argument("--weak-dylibs", action="store_true", help="inject dylibs as LC_LOAD_WEAK_DYLIB")
    sideload_parser.add_argument(
        "--allow-other-platform", action="store_true",
        help="attempt an .ipa built for tvOS/watchOS anyway (very unlikely to install)",
    )
    sideload_parser.add_argument("--inject-extensions", action="store_true", help="also inject dylibs into app extensions")
    sideload_parser.add_argument("--enable-file-sharing", action="store_true", help="enable Files app / iTunes file sharing")
    sideload_parser.add_argument(
        "--remove-extensions", action=argparse.BooleanOptionalAction, default=True,
        help="remove app extensions + watch app (free-account requirement)",
    )
    sideload_parser.add_argument(
        "--remove-device-restrictions", action=argparse.BooleanOptionalAction, default=True,
        help="remove UISupportedDevices",
    )

    pairing_parser = sub.add_parser(
        "pairing",
        parents=[common, target],
        help="export, import, create, or place this PC's pairing file on the iPhone",
    )
    pairing_actions = pairing_parser.add_mutually_exclusive_group()
    pairing_actions.add_argument(
        "--export",
        dest="pairing_export",
        default=None,
        metavar="PATH",
        help="write the pairing file to PATH (XML plist)",
    )
    pairing_actions.add_argument(
        "--import",
        dest="pairing_import",
        default=None,
        metavar="PATH",
        help="store a pairing plist from another tool (optional; iPASide can create one)",
    )
    pairing_actions.add_argument(
        "--create",
        dest="pairing_create",
        action="store_true",
        help="create Remote Pairing keys over USB (no other app required)",
    )
    pairing_actions.add_argument(
        "--deliver",
        action="store_true",
        help="write the pairing file into every supported app installed on the device",
    )
    pairing_parser.add_argument(
        "--app",
        dest="pairing_app",
        default=None,
        metavar="BUNDLE_ID",
        help="with --deliver, place only into this installed app",
    )

    lc_parser = sub.add_parser(
        "livecontainer", parents=[common, target, signed_output],
        help="install LiveContainer and hand it the signing certificate",
    )
    lc_parser.add_argument(
        "--setup", action="store_true",
        help="sign + install LiveContainer, then give it the certificate",
    )
    lc_parser.add_argument(
        "--download", action="store_true", help="download the latest LiveContainer IPA"
    )
    lc_parser.add_argument(
        "--ipa", default=None,
        help="LiveContainer .ipa to use (default: download the latest release)",
    )
    lc_parser.add_argument(
        "--dir", default=None,
        help="folder for the downloaded IPA (default: the app's downloads folder)",
    )
    lc_parser.add_argument(
        "--manual-certificate", action="store_true",
        help="leave the .p12 for LiveContainer's own Settings instead of importing it",
    )
    lc_parser.add_argument(
        "--variant", choices=livecontainer.VARIANTS, default=livecontainer.VARIANT_SIDESTORE,
        help="which build to fetch: 'sidestore' carries a store that can refresh on the "
             "phone itself, 'plain' is smaller and cannot (default: sidestore)",
    )
    lc_parser.add_argument(
        "--apps", action="store_true",
        help="list the apps running inside LiveContainer (these use no app slots)",
    )
    lc_parser.add_argument(
        "--add", default=None, metavar="IPA",
        help="put an .ipa inside LiveContainer instead of installing it on the phone",
    )
    lc_parser.add_argument(
        "--remove", default=None, metavar="BUNDLE_ID",
        help="remove an app from inside LiveContainer",
    )

    jb_parser = sub.add_parser(
        "jailbreak", parents=[common, target, signed_output],
        help="check jailbreak compatibility, or download / install Dopamine",
    )
    jb_parser.add_argument(
        "--install", action="store_true",
        help="sign + install Dopamine (downloads the latest IPA unless --ipa is given)",
    )
    jb_parser.add_argument(
        "--download", action="store_true", help="download the latest Dopamine IPA only",
    )
    jb_parser.add_argument(
        "--ipa", default=None,
        help="Dopamine .ipa to install (default: download the latest release)",
    )
    jb_parser.add_argument(
        "--dir", default=None,
        help="folder for the downloaded IPA (default: the app's downloads folder)",
    )

    sub.add_parser("installs", parents=[common], help="list apps iPASide has sideloaded (with expiry)")

    # transport, not target: a refresh reinstalls onto the device its record names, so
    # --udid would be meaningless, but the transport preference still has to be honoured
    # or the unattended daily run would go over the air after the user asked for USB.
    refresh_parser = sub.add_parser(
        "refresh", parents=[common, transport, signed_output],
        help="re-sign + reinstall sideloaded apps before they expire",
    )
    refresh_parser.add_argument("--bundle-id", default=None, help="refresh a single recorded app")
    refresh_parser.add_argument("--all", action="store_true", help="refresh every recorded app")
    refresh_parser.add_argument(
        "--within", type=float, default=2.0,
        help="with neither --all nor --bundle-id, refresh apps expiring within N days",
    )

    signed_parser = sub.add_parser(
        "signed", parents=[common], help="show or clean up the signed IPAs iPASide has kept"
    )
    signed_parser.add_argument(
        "--clean", action="store_true", help="delete the signed IPAs in the folder"
    )
    signed_parser.add_argument(
        "--dir", default=None,
        help="folder to report on or clean (default: the app's signed folder)",
    )

    forget_parser = sub.add_parser(
        "forget", parents=[common], help="remove an app from the sideload registry"
    )
    forget_parser.add_argument("bundle_id", help="installed bundle id to forget")

    slots_parser = sub.add_parser(
        "slots", parents=[common],
        help="show or tidy up an Apple ID's certificates, App IDs and devices",
    )
    slots_parser.add_argument(
        "--email", default=None,
        help="which signed-in Apple ID to report on (default: the active one)",
    )

    revoke_parser = sub.add_parser(
        "revoke-cert", parents=[common],
        help="revoke a development certificate (stops the apps it signed from opening)",
    )
    revoke_parser.add_argument("serial", help="serial number, from `slots`")
    revoke_parser.add_argument(
        "--email", default=None,
        help="which signed-in Apple ID owns it (default: the active one)",
    )

    delete_app_id_parser = sub.add_parser(
        "delete-app-id", parents=[common],
        help="delete a registered app identifier (frees the name, not the weekly quota)",
    )
    delete_app_id_parser.add_argument("app_id", help="the appIdId to delete (from `slots`)")
    delete_app_id_parser.add_argument(
        "--email", default=None,
        help="which signed-in Apple ID owns it (default: the active one)",
    )

    prepare_parser = sub.add_parser(
        "prepare", parents=[common], help="strip/patch an IPA for sideloading (before signing)"
    )
    prepare_parser.add_argument("ipa", help="path to an .ipa file")
    prepare_parser.add_argument(
        "--output", "-o", default=None, help="write a prepared .ipa (omit to only report)"
    )
    prepare_parser.add_argument(
        "--strip-extensions",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="remove app extensions + watch app (required for free Apple IDs)",
    )
    prepare_parser.add_argument(
        "--remove-device-restrictions",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="remove UISupportedDevices so the app installs on this device",
    )
    prepare_parser.add_argument("--bundle-id", default=None, help="new CFBundleIdentifier")
    prepare_parser.add_argument("--name", default=None, help="new CFBundleDisplayName")
    prepare_parser.add_argument("--set-version", default=None, help="new CFBundleShortVersionString")
    prepare_parser.add_argument(
        "--compress",
        type=int,
        default=0,
        help="zip level 0=store (fast, default; zsign re-zips) .. 9=smallest",
    )
    prepare_parser.add_argument(
        "--keep-dir", action="store_true", help="keep the extracted/prepared work directory"
    )

    install_parser = sub.add_parser(
        "install", parents=[common, target], help="install a signed IPA onto the device"
    )
    install_parser.add_argument("ipa", help="path to a signed .ipa file")

    uninstall_parser = sub.add_parser(
        "uninstall", parents=[common, target], help="uninstall an app by bundle id"
    )
    uninstall_parser.add_argument("bundle_id", help="bundle identifier to remove")

    return parser


def main(argv: list[str] | None = None) -> int:
    _force_utf8()
    _enable_fault_handler()
    argv = list(argv) if argv is not None else sys.argv[1:]
    # No command given: show the palette instead of argparse's error wall.
    # (-h/--help is still a help request and falls through to the parser.)
    if all(arg.startswith("-") and arg not in ("-h", "--help") for arg in argv):
        return _print_menu()
    args = build_parser().parse_args(argv)
    # --json may sit before the subcommand; argparse parents then parse it twice
    # and the subparser's default silently resets it to False.
    if "--json" in argv:
        args.json = True
    try:
        return dispatch(args)
    except EngineError as exc:
        # An EngineError is a situation, not a bug: the phone is unplugged, Apple
        # said no, the IPA is encrypted. Its message is written to be read, so print
        # that and nothing else. Anything not descended from EngineError keeps its
        # traceback, because then a stack is exactly what is wanted.
        if getattr(args, "json", False):
            _emit(args, {"status": "error", "error": str(exc)})
        else:
            print(f"error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:
        # A downstream consumer closed the pipe early (e.g. `| head`). Exit
        # quietly instead of dumping a traceback.
        try:
            sys.stdout.close()
        except OSError:
            pass
        sys.exit(0)
    except KeyboardInterrupt:
        sys.exit(130)
