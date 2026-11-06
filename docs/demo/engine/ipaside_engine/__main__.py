"""Engine for OpenSide recordings.

Isolated: no device access, no filesystem reads, no ipa parsing. Returns canned
data that looks like a real OpenSide run so the TUI can be recorded without
touching any real hardware.
"""
from __future__ import annotations
import argparse, json, sys, time

VERSION = "1.2.14"

DEVICES = [{
    "serial": "00008140-001A2B3C4D5E6F70",
    "connection_type": "usb",
    "device_id": "00008140-001A2B3C4D5E6F70",
}]
DEVICE_INFO = {
    "name": "iPhone 17 Pro Max",
    "model": "iPhone 17 Pro Max",
    "ios_version": "18.2.1",
    "udid": "00008140-001A2B3C4D5E6F70",
    "connection_type": "usb",
    "pairing": "ok",
    "developer_mode": True,
    "storage_free": "412.3 GB",
    "battery": "87%",
}
INSTALLS = [
    {"name": "retro shooter", "bundle_id": "com.indie.retroshooter", "expired": True, "days_left": 0},
    {"name": "timetracker", "bundle_id": "com.productivity.timetracker", "expired": False, "days_left": 4},
    {"name": "notes pro", "bundle_id": "com.notes.notespro", "expired": False, "days_left": 22},
]
INSPECT = {
    "filename": "timetracker.ipa",
    "bundle_id": "com.productivity.timetracker",
    "version": "1.0.0",
    "size": "24.8 MB",
    "min_ios": "17.0",
    "team": "Personal Team (A1B2C3D4E5)",
}
DOCTOR_CHECKS = [
    {"name": "engine", "status": "ok", "detail": "python 3.12, engine 1.2.14"},
    {"name": "anisette", "status": "ok", "detail": "remote provisioner reachable"},
    {"name": "pairing", "status": "ok", "detail": "iPhone 17 Pro Max paired"},
    {"name": "developer mode", "status": "ok", "detail": "enabled"},
    {"name": "usbmuxd", "status": "ok", "detail": "listening on /var/run/usbmuxd"},
]
APPS = [
    {"name": "Timetracker", "bundle_id": "com.productivity.timetracker", "version": "1.0.0"},
    {"name": "Notes Pro", "bundle_id": "com.notes.notespro", "version": "2.4.1"},
]
ACCOUNTS = [{"email": "alex@icloud.com", "team_id": "A1B2C3D4E5", "active": True}]
ANISEETTE = {"status": "ok", "provisioner": "remote", "headers": "fresh"}
SIDeload_RESULT = {
    "refreshed": [{"bundle_id": "com.productivity.timetracker", "status": "installed", "expires_at": "2026-12-01"}],
    "count": 1,
}

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
        phases = [
            "Contacting Apple\u2026",
            "Provisioning signing assets\u2026",
            "Signing the app\u2026",
            "Uploading to iPhone\u2026",
            "Installing\u2026",
            "Finishing setup\u2026",
        ]
        for p in phases:
            _progress(rid, p)
            time.sleep(0.55)
        _result(rid, True, SIDeload_RESULT)
    elif cmd == "refresh":
        _progress(rid, "Refreshing\u2026")
        time.sleep(0.6)
        _result(rid, True, SIDeload_RESULT)
    elif cmd == "anisette":
        time.sleep(0.2)
        _result(rid, True, ANISEETTE)
    elif cmd == "developer-mode":
        time.sleep(0.2)
        _result(rid, True, {"developer_mode": True, "status": "enabled"})
    elif cmd == "pairing":
        time.sleep(0.2)
        _result(rid, True, {"status": "paired", "file": "/var/lib/lockdown/..."})
    elif cmd == "jailbreak":
        time.sleep(0.2)
        _result(rid, True, {"advice": "no jailbreak detected; standard sideload path"})
    elif cmd == "teams":
        time.sleep(0.2)
        _result(rid, True, {"teams": [{"teamId": "A1B2C3D4E5", "name": "Personal Team"}]})
    elif cmd == "slots":
        time.sleep(0.2)
        _result(rid, True, {"certs": 1, "app_ids": 3, "devices": 1})
    elif cmd == "accounts" or cmd == "login":
        time.sleep(0.2)
        _result(rid, True, {"accounts": ACCOUNTS, "authenticated": True})
    elif cmd == "signed":
        time.sleep(0.2)
        _result(rid, True, [{"name": "timetracker.ipa", "path": "~/Downloads/timetracker.ipa"}])
    elif cmd == "apps":
        time.sleep(0.3)
        _result(rid, True, APPS)
    else:
        _result(rid, True, {})

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
    args = ap.parse_args()
    if args.command in (None, "serve"):
        serve()

if __name__ == "__main__":
    main()
