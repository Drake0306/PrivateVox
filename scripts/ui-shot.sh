#!/bin/sh
# Screenshot the PrivateVox panel on a given tab, for visual checks during development (macOS only).
#   scripts/ui-shot.sh <History|Models|Polish|Network|Settings|Setup> <out.png> [--page-down N] [--click X,Y]
# Needs the app running, Accessibility for the terminal (System Events) and Screen Recording for screencapture.
# --click X,Y clicks at an offset from the panel window's top-left corner (points) before capturing.
set -e
TAB=$1; OUT=$2; shift 2
PD=0; CLICK=""
while [ $# -gt 0 ]; do
  case $1 in
    --page-down) PD=$2; shift 2 ;;
    --click) CLICK=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
case $TAB in
  History) TX=60 ;; Models) TX=137 ;; Polish) TX=212 ;; Network) TX=289 ;; Settings) TX=374 ;; Setup) TX=451 ;;
  *) echo "unknown tab $TAB" >&2; exit 2 ;;
esac
# The pill and the panel are both windows called "PrivateVox"; the panel is the wide one. Open it through the tray if missing.
panel_frame() {
  # "{x1, y1, x2, y2}, {w1, h1, w2, h2}" for N windows -> the frame of the first window wider than 600 pt.
  osascript -e 'tell application "System Events" to tell process "privatevox" to get {position, size} of every window' 2>/dev/null \
    | tr -d ' ' | awk -F, '{ n = NF / 4; for (i = 1; i <= n; i++) { w = $(2*n + 2*i - 1); if (w + 0 > 600) { printf "%s,%s,%s,%s\n", $(2*i - 1), $(2*i), w, $(2*n + 2*i); exit } } }'
}
# Always go through the tray: it shows the panel when it was closed and brings it forward.
osascript >/dev/null 2>&1 <<'EOF'
tell application "System Events"
  tell process "privatevox"
    click menu bar item 1 of menu bar 2
    delay 0.4
    click menu item "Open history" of menu 1 of menu bar item 1 of menu bar 2
  end tell
end tell
EOF
sleep 1
FRAME=$(panel_frame)
[ -n "$FRAME" ] || { echo "no panel window" >&2; exit 1; }
osascript -e 'tell application "System Events" to tell process "privatevox" to set frontmost to true' >/dev/null 2>&1
WX=$(echo "$FRAME" | cut -d, -f1); WY=$(echo "$FRAME" | cut -d, -f2); WW=$(echo "$FRAME" | cut -d, -f3); WH=$(echo "$FRAME" | cut -d, -f4)
osascript -e "tell application \"System Events\" to click at {$((WX + TX)), $((WY + 155))}" >/dev/null
sleep 0.6
if [ "$PD" -gt 0 ]; then
  # Focus the page (empty space right of the tab bar) so Page Down scrolls the content rather than the tab button.
  osascript -e "tell application \"System Events\" to click at {$((WX + WW - 60)), $((WY + 155))}" >/dev/null
  i=0; while [ $i -lt "$PD" ]; do osascript -e 'tell application "System Events" to key code 121' >/dev/null; i=$((i + 1)); done
  sleep 0.5
fi
if [ -n "$CLICK" ]; then
  CX=$(echo "$CLICK" | cut -d, -f1); CY=$(echo "$CLICK" | cut -d, -f2)
  osascript -e "tell application \"System Events\" to click at {$((WX + CX)), $((WY + CY))}" >/dev/null
  sleep 0.6
fi
TMP=$(mktemp -t uishot).png
screencapture -x "$TMP"
# Retina: points × 2. sips takes the crop as height width, then offsetY offsetX.
sips -c $((WH * 2)) $((WW * 2)) --cropOffset $((WY * 2)) $((WX * 2)) "$TMP" --out "$OUT" >/dev/null 2>&1
rm -f "$TMP"
echo "$OUT"
