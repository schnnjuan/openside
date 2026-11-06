"""Serve loop mimicking ipaside_engine serve, returning canned demo data."""
from __future__ import annotations
import argparse, json, sys, threading, time

VERSION = "1.2.14"

DEVICES = [{
    "serial": "00008140-001A2B3C4D5E6F70",
    "connection_type": "usb",
    "device_id": "demo-iphone-17-pro-max",
}]
DEVICE_INFO = {
    "name": "OpenSide Demo iPhone",
    "model": "iPhone 17 Pro Max",
    "ios_version": "19.0 (demo)",
    "udid": "00008140-001A2B3C4D5E6F70",
    "connection_type": "usb",
    "pairing": "ok",
    "developer_mode": True,
    "storage_free": "412.3 GB",
}
INSTALLS = [
    {"name": "retro shooter", "bundle_id": "com.demo.retroshooter", "expired": True, "days_left": 0},
    {"name": "timetracker", "bundle_id": "com.demo.timetracker", "expired": False, "days_left": 4},
    {"name": "notes pro", "bundle_id": "com.demo.notespro", "expired": False, "days_left": 22},
]
INSPECT = {
    "filename": "timetracker.ipa",
    "bundle_id": "com.demo.timetracker",
    "version": "1.0.0",
    "size": "24.8 MB",
    "min_ios": "17.0",
    "team": "Demo Team (1234567890)",
}
DOCTOR_CHECKS = [
    {"name": "engine", "status": "ok", "detail": "python 3.12, engine 1.2.14"},
    {"name": "anisette", "status": "ok", "detail": "remote provisioner reachable"},
    {"name": "pairing", "status": "ok", "detail": "OpenSide Demo iPhone paired"},
    {"name": "developer mode", "status": "ok", "detail": "enabled"},
]
APPS = [
    {"name": "Timetracker", "bundle_id": "com.demo.timetracker", "version": "1.0.0"},
]

def _emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def _progress(rid, line):
    _emit({"id": rid, "type": "progress", "line": line})

def _result(rid, ok, data=None, error=None):
    _emit({"id": rid, "type": "result", "ok": ok, "data": data, "error": error})

def handle(request):
    rid = request.get("id")
    args = request.get("args") or []
    cmd = args[0] if args else ""
    # simulate latency per command for cinematic pacing
    if cmd == "doctor":
        time.sleep(0.4)
        _result(rid, True, {"overall": "ok", "checks": DOCTOR_CHECKS})
    elif cmd == "devices":
        time.sleep(0.3)
        _result(rid, True, DEVICES)
    elif cmd == "device-info":
        time.sleep(0.4)
        _result(rid, True, DEVICE_INFO)
    elif cmd == "installs":
        time.sleep(0.4)
        _result(rid, True, INSTALLS)
    elif cmd == "apps":
        time.sleep(0.3)
        _result(rid, True, APPS)
    elif cmd == "inspect":
        time.sleep(0.5)
        _result(rid, True, INSPECT)
    elif cmd == "version":
        time.sleep(0.2)
        _result(rid, True, {"version": VERSION})
    elif cmd == "sideload":
        ipa = args[-1] if len(args) > 1 else "timetracker.ipa"
        phases = [
            "validating ipa…",
            "reading metadata…",
            "signing with demo team…",
            "uploading to device…",
            "installing…",
            "verifying installation…",
        ]
        for p in phases:
            _progress(rid, p)
            time.sleep(0.55)
        _result(rid, True, {
            "refreshed": [{"bundle_id": "com.demo.timetracker", "status": "installed", "expires_at": "2026-11-20"}],
            "count": 1,
        })
    elif cmd == "login":
        _result(rid, True, {"status": "ok", "authenticated": False})
    elif cmd == "anisette":
        _result(rid, True, {"status": "ok"})
    else:
        _result(rid, True, {"note": f"demo: {cmd}"})

def serve():
    _emit({"type": "ready", "version": VERSION})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        if req.get("type") == "shutdown":
            break
        handle(req)

def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="command")
    sub.add_parser("serve")
    # allow `doctor --json` style direct invocation too
    for c in ("doctor", "devices", "device-info", "installs", "apps", "inspect", "version"):
        sp = sub.add_parser(c)
        sp.add_argument("--json", action="store_true")
    args = ap.parse_args()
    if args.command == "serve" or args.command is None:
        serve()
        return
    # direct (non-serve) invocation: print JSON to stdout
    cmd = args.command
    fake_req = {"id": 1, "args": [cmd]}
    # reuse handle but capture stdout
    import io
    buf = io.StringIO()
    old = sys.stdout
    sys.stdout = buf
    handle(fake_req)
    sys.stdout = old
    # handle already emitted result line; extract data
    out = buf.getvalue().strip().splitlines()
    if out:
        last = json.loads(out[-1])
        print(json.dumps(last.get("data")))

if __name__ == "__main__":
    main()
