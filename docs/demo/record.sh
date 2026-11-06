#!/bin/sh
# OpenSide demo recording driver.
# Run from workspace 3 (switch there first). Leaves PC untouched for ~12s.
set -eu

export DISPLAY=:1
export HOME=/home/schnnjuan/.demo-home
export OPENSIDE_ENGINE_PYTHON=/usr/bin/python3
export OPENSIDE_ENGINE_DIR=/home/schnnjuan/projects/openside/docs/demo/engine
export OPENSIDE_LANG=en

KITTY=/usr/bin/kitty
TUI=/home/schnnjuan/projects/openside/tools/openside-cli/target/debug/openside
OUT=/home/schnnjuan/demo-take.mp4

echo "Switch to workspace 3 now. Recording starts in 3s..."
sleep 3

$KITTY --title "OpenSide Demo" "$TUI" >/tmp/kitty.log 2>&1 &
sleep 4

GEO=$(hyprctl clients -j | python3 -c "import sys,json; d=json.load(sys.stdin); c=[x for x in d if 'OpenSide Demo' in (x.get('title') or '')][0]; print(f\"{c['at'][0]},{c['at'][1]} {c['size'][0]}x{c['size'][1]}\")")
echo "geometry: $GEO"

wf-recorder -g "$GEO" -f "$OUT" >/tmp/wf.log 2>&1 &
WFPID=$!
sleep 2

# idle hero
sleep 2

# open sideload picker: doctor(0)->devices(1)->device-info(2)->[header]->sideload(4) = 4 downs
for i in 1 2 3 4; do wtype -k Down; sleep 0.12; done
wtype -k Return
sleep 0.6

# type the ipa path directly (char by char)
for ch in '~' '/' 'U' 's' 'e' 'r' 's' '/' 'd' 'e' 'm' 'o' '/' 'D' 'o' 'w' 'n' 'l' 'o' 'a' 'd' 's' '/' 't' 'i' 'm' 'e' 't' 'r' 'a' 'c' 'k' 'e' 'r' '.' 'i' 'p' 'a'; do
    wtype "$ch"
    sleep 0.04
done
sleep 0.3
wtype -k Return
# progress phases + result (~4s)
sleep 4.5

# stop recording
kill $WFPID 2>/dev/null
wait $WFPID 2>/dev/null || true
echo "recorded $OUT"
echo "review the mp4, then tell the agent to continue"
SH
chmod +x /home/schnnjuan/projects/openside/docs/demo/record.sh
echo "written"