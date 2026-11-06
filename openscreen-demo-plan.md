# OpenSide demo — handover (next session)

## Goal
Produce MP4 (1080p, 60fps, muted) product demos of OpenSide TUI for openside.schnnjuan.com, showing the real app with fictional device/ipa data. No real device, no real filesystem beyond a demo sandbox.

## What's already done (this session)
- Demo engine fixture: `docs/demo/engine/ipaside_engine/` (isolated: grep confirmed zero pymobiledevice/fs/subprocess access).
  - Serves NDJSON like real engine; canned data: iPhone 17 Pro Max "OpenSide Demo iPhone", timetracker.ipa (com.demo.timetracker, 1.0.0, 24.8 MB), installs with expiry colors, doctor all ✓.
- Smoke boot verified: `OPENSIDE_ENGINE_PYTHON=/usr/bin/python3 OPENSIDE_ENGINE_DIR=.../docs/demo/engine` → TUI boots, engine ready.
- Demo sandbox HOME: `/home/schnnjuan/.demo-home` with `~/Downloads/timetracker.ipa` (empty placeholder) for the browse flow.
- OpenScreen 2.2.2 installed once via pacman to /tmp/osx (wiped between sessions — reinstall with `gh release download --repo getopenscreen/openscreen -p Openscreen-Linux-*.pacman` then `bsdtar -xf`).

## What's NOT done (needs OpenScreen MCP, available only next session)
Recording, editing, export. The MCP tools are not loaded in this session.

## Next session setup
1. Reinstall OpenScreen to a durable path, e.g. `/home/schnnjuan/.local/bin/openscreen` (extract pacman there).
2. Add MCP server to opencode.json: command ["/home/.../openscreen","mcp"], env DISPLAY=:1.
3. Restart session so tools load. Then read this file and continue from step 4 of the openscreen-demo skill.

## Storyboard (cursor-driven, ~19s)
Terminal: kitty 120x36, maki theme, bg #012D2D, monospace, zoom 100%, caret only.
Keys via tmux send-keys (deterministic). Cursor choreographed via setPointerTrack (fractions of window, easing, no fake clicks).

| Beat | Time | Action | On screen |
|---|---|---|---|
| 1 | 0-2s | idle | wordmark + "detected OpenSide Demo iPhone" |
| 2 | 2-5s | Commands: open picker, type /Users/demo/Downloads/timetracker.ipa, ⏎ | metadata: com.demo.timetracker · 1.0.0 · 24.8 MB |
| 3 | 5-7s | initiate install, confirm | prompt |
| 4 | 7-12s | output pane streams | validation → prep → sign progress → ✓ installed |
| 5 | 12-15s | back to commands | ✓ on device |
| 6 | 15-19s | idle + zoom out | clean end |

Short cut (~8s): beats 1, 4, ✓, end.

## Export
MP4 H.264 1080p 60fps muted only. No GIF (MP4 wins on weight/quality/fps; GIF only if README asks later).

## Constraints (permanent)
- Never run sideload/install/uninstall/refresh/device-info/devices on the real iPhone. Demo uses fixture only.
- APP_NAME stays iPASide; engine untouched.
- All on-screen text in English.

## Files
- Fixture: docs/demo/engine/ipaside_engine/
- Sandbox HOME: /home/schnnjuan/.demo-home
- Launch: env HOME=/home/schnnjuan/.demo-home OPENSIDE_ENGINE_PYTHON=/usr/bin/python3 OPENSIDE_ENGINE_DIR=/home/schnnjuan/projects/openside/docs/demo/engine OPENSIDE_LANG=en ./tools/openside-cli/target/debug/openside
