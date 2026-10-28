#!/bin/sh
# Click an item in PrivateVox's menu bar menu (macOS, for development checks).
#   scripts/tray-menu.sh list              prints the menu items and the Model submenu
#   scripts/tray-menu.sh "Show pill"       clicks a top-level item
#   scripts/tray-menu.sh Model large-v3    clicks an item of a submenu
# The first click after a start is sometimes swallowed, so each call retries a few times.
ITEM=$1; SUB=$2
n=0
while [ $n -lt 4 ]; do
  if [ "$ITEM" = "list" ]; then
    ACTION='set names to name of every menu item of m
      set sub to name of every menu item of menu 1 of menu item "Model" of m
      key code 53
      return {names, sub}'
  elif [ -n "$SUB" ]; then
    ACTION="click menu item \"$SUB\" of menu 1 of menu item \"$ITEM\" of m
      return \"clicked\""
  else
    ACTION="click menu item \"$ITEM\" of m
      return \"clicked\""
  fi
  OUT=$(osascript 2>&1 -e 'tell application "System Events"' -e 'tell process "privatevox"' -e 'click menu bar item 1 of menu bar 2' -e 'delay 0.8' -e 'if not (exists menu 1 of menu bar item 1 of menu bar 2) then error "menu did not open"' -e 'set m to menu 1 of menu bar item 1 of menu bar 2' -e "$ACTION" -e 'end tell' -e 'end tell')
  case "$OUT" in
    *"execution error"*) n=$((n + 1)); osascript -e 'tell application "System Events" to key code 53' >/dev/null 2>&1; sleep 1 ;;
    *) echo "$OUT"; exit 0 ;;
  esac
done
echo "$OUT" >&2; exit 1
