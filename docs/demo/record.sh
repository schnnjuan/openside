#!/bin/sh
# OpenSide demo recording driver (run manually on the PC).
# Requires: kitty, wf-recorder, wtype, hyprctl, openscreen CLI in PATH.
set -eu

export DISPLAY=:1
export HOME=/home/schnnjuan/.demo-home
export OPENSIDE_ENGINE_PYTHON=/usr/bin/python3
export OPENSIDE_ENGINE_DIR=/home/schnnjuan/projects/openside/docs/demo/engine
export OPENSIDE_LANG=en

KITTY=/usr/bin/kitty
TUI=/home/schnnjuan/projects/openside/tools/openside-cli/target/debug/openside
OUT=/home/schnnjuan/demo-take.mp4

# 1. launch TUI
$KITTY --title "OpenSide Demo" $TUI >/tmp/kitty.log 2>&1 &
sleep 4

# 2. get window geometry
GEO=$(hyprctl clients -j | python3 -c "import sys,json; d=json.load(sys.stdin); c=[x for x in d if 'OpenSide Demo' in (x.get('title') or '')][0]; print(f\"{c['at'][0]},{c['at'][1]} {c['size'][0]}x{c['size'][1]}\")")
echo "geometry: $GEO"

# 3. record
wf-recorder -g "$GEO" -f "$OUT" >/tmp/wf.log 2>&1 &
WFPID=$!
sleep 2

# 4. drive keys (tmux-like timing via wtype)
focus() { sleep 0.3; }
# idle hero
sleep 2
# run doctor
wtype -k Return; sleep 2
# installs: 5 downs then enter
for i in 1 2 3 4 5; do wtype -k Down; sleep 0.15; done
wtype -k Return; sleep 2.5
# device-info: 11 downs then enter
for i in $(seq 1 11); do wtype -k Down; sleep 0.1; done
wtype -k Return; sleep 2
# inspect: 1 down then enter (opens file browser)
wtype -k Down; sleep 0.3; wtype -k Return; sleep 1
# in browser: navigate to Downloads (Down to Downloads, Enter), then timetracker.ipa (Down, Enter)
wtype -k Down; sleep 0.3; wtype -k Return; sleep 1
wtype -k Down; sleep 0.3; wtype -k Return; sleep 2
# sideload: select sideload (home then 4 downs) then enter? instead use version for theme hook
# version: home, then navigate
wtype -k Home; sleep 0.3
for i in $(seq 1 21); do wtype -k Down; sleep 0.05; done
wtype -k Return; sleep 2

# 5. stop recording
kill $WFPID 2>/dev/null
wait $WFPID 2>/dev/null || true
echo "recorded $OUT"
echo "next: openscreen createProject with videoPath=$OUT, then edit + export"
SH
chmod +x /home/schnnjuan/projects/openside/docs/demo/record.sh
echo "written"