#!/bin/sh
# End-to-end smoke test of the iPhone app in the iOS Simulator: installs it afresh,
# puts a synthetic library into its Documents folder, drives the phone screens through
# the verification channel (src/ui/automation.rs) and checks the state the app reports.
# Silent: app volume 0, no media session. Needs ffmpeg for the test library and a
# booted simulator (xcrun simctl boot "iPhone 17").
#
#   scripts/dev/smoke-ios.sh
#   ONEUM_IOS_DEVICE   simulator to use (name or UDID); default: the booted one
#
# The app and its data on that simulator are removed first and again at the end.
set -eu
cd "$(dirname "$0")/../.."
DEVICE=${ONEUM_IOS_DEVICE:-booted}
BUNDLE=app.oneum.OnEum
WORK=$(mktemp -d /tmp/oneum-ios-smoke.XXXXXX)
MUSIC="$WORK/music"
PROFILE="$WORK/profile"
trap 'xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true; xcrun simctl uninstall "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT
export ONEUM_NO_MEDIA_SESSION=1

ui() { # ui <command>…: sends commands and waits for the files of any shot/dump
  outs=""
  for c in "$@"; do
    case "$c" in
      shot\ *|dump\ *)
        f="$PROFILE/${c#* }"; rm -f "$f"; outs="$outs $f"
        printf '%s %s\n' "${c%% *}" "$f" >> "$PROFILE/script.txt" ;;
      *) printf '%s\n' "$c" >> "$PROFILE/script.txt" ;;
    esac
  done
  for f in $outs; do
    i=0
    while [ ! -f "$f" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i + 1)); done
    [ -f "$f" ] || { echo "timed out waiting for $f" >&2; exit 1; }
  done
}
check() { # check <dump-file> <python expression over d> <description>
  python3 - "$PROFILE/$1" "$2" "$3" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
ok = bool(eval(sys.argv[2], {"d": d, "p": d.get("phone", {})}))
print(("  ok   " if ok else "  FAIL ") + sys.argv[3])
if not ok:
    print("       phone:", d.get("phone"))
    print("       state:", {k: d.get(k) for k in ("state", "current", "position_ms", "queue_len", "queue_index", "library_tracks", "albums", "volume", "underruns", "dialog_open", "scan_running")})
    sys.exit(1)
PY
}
start() { # (re)starts the installed app with the verification channel
  xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
  sleep 1
  : > "$PROFILE/script.txt"
  SIMCTL_CHILD_ONEUM_SCRIPT="$PROFILE/script.txt" SIMCTL_CHILD_ONEUM_NO_MEDIA_SESSION=1 \
    xcrun simctl launch "$DEVICE" "$BUNDLE" >/dev/null
  sleep 5
}
container() { xcrun simctl get_app_container "$DEVICE" "$BUNDLE" data; }

echo "== generating test library"
scripts/dev/gen-test-library.sh "$MUSIC" >/dev/null
echo "== installing afresh"
xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
xcrun simctl uninstall "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
scripts/ios-sim.sh "$PROFILE" >/dev/null
sleep 5

echo "== first run"
ui 'volume 0' 'wait 500' 'dump a.json'
check a.json 'd["library_tracks"] == 0 and d["state"] == "Idle"' "starts with an empty library, idle"
check a.json 'p["tab"] == "Library" and p["page"] is None' "the phone screens are shown, on the library tab"
check a.json 'd["screen"]["full"][3] > d["screen"]["full"][2] > 300' "the whole screen is drawn on, upright"
check a.json 'd["screen"]["safe"][1] > 0 and d["screen"]["safe"][3] < d["screen"]["full"][3]' "the status bar and the home indicator are kept clear"
# Where things are on this screen (the constants of src/ui/phone.rs).
eval "$(python3 - "$PROFILE/a.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))["screen"]
w, top, bottom = s["full"][2], s["safe"][1], s["safe"][3]
print(f"W={w:.0f} TOP={top:.0f} BOTTOM={bottom:.0f}")
print(f"HEAD={top + 25:.0f} TABS={bottom - 27:.0f} MINI={bottom - 54 - 31:.0f}")
for i, name in enumerate(["LIBRARY", "SEARCH", "PLAYLISTS", "SETTINGS"]):
    print(f"TAB_{name}={w * (i + 0.5) / 4:.0f}")
for i in range(8):
    print(f"MENU{i}={top + 50 + 54 * i + 27:.0f}")
print(f"SONG0={top + 178:.0f} SONG1={top + 240:.0f}")
step = (w - 56) / 5
controls = top + 50 + 8 + min(w - 56, (bottom - top - 50) - 308) + 16
print(f"PLAY_X={28 + step * 2.5:.0f} NEXT_X={28 + step * 3.5:.0f} PLAY_Y={controls + 156:.0f}")
print(f"RIGHT={w - 28:.0f}")
PY
)"

echo "== music in the Documents folder"
cp -R "$MUSIC"/* "$(container)/Documents/"
start
ui 'volume 0' 'wait 1500' 'dump b.json'
check b.json 'd["library_tracks"] == 20 and not d["scan_running"]' "20 tracks read from the Documents folder (1 unreadable file skipped)"
check b.json 'd["albums"] == 7' "albums grouped"

echo "== lists"
ui "click 200 $MENU0" 'wait 500' 'dump c1.json' "click 26 $HEAD" 'wait 400' 'dump c2.json' \
   "click 200 $MENU1" 'wait 600' 'dump c3.json' "click 100 $((TOP + 150))" 'wait 600' 'dump c4.json' \
   "click 26 $HEAD" 'wait 300' "click 26 $HEAD" 'wait 300' 'dump c5.json'
check c1.json 'p["page"] == "List(Songs)"' "the song list opens"
check c2.json 'p["page"] is None' "the back button returns"
check c3.json 'p["page"] == "List(Albums)"' "the albums open"
check c4.json 'p["page"].startswith("List(Album(") and p["depth"][0] == 2' "an album opens above them"
check c5.json 'p["page"] is None and p["depth"][0] == 0' "back twice is the first page again"

echo "== playback"
ui "click 200 $MENU0" 'wait 500' "click 150 $SONG0" 'wait 1500' 'dump p1.json'
check p1.json 'd["state"] == "Playing" and d["queue_len"] == 20 and d["queue_index"] == 0' "a tap plays the song and queues the list"
check p1.json 'd["volume"] == 0 and d["underruns"] == 0 and d["output_device"]' "audio reaches the output without underruns, silently"
ui 'wait 1200' 'dump p2.json'
check p2.json 'd["position_ms"] > 1500' "the position advances"
ui "click 150 $MINI" 'wait 600' 'dump p3.json'
check p3.json 'p["player_open"] and not p["queue_open"]' "the bar of the playing song opens the player"
ui "click $PLAY_X $PLAY_Y" 'wait 500' 'dump p4.json' "click $PLAY_X $PLAY_Y" 'wait 700' 'dump p5.json'
check p4.json 'd["state"] == "Paused"' "the play button pauses"
check p5.json 'd["state"] == "Playing"' "and resumes"
ui "click $NEXT_X $PLAY_Y" 'wait 1000' 'dump p6.json'
check p6.json 'd["queue_index"] == 1 and d["state"] == "Playing"' "next"
ui "click $RIGHT $HEAD" 'wait 500' 'dump p7.json' "click 150 $((TOP + 50 + 34 + 31))" 'wait 1000' 'dump p8.json'
check p7.json 'p["queue_open"]' "the queue opens in the player"
check p8.json 'd["queue_index"] == 0 and d["state"] == "Playing"' "a tap in the queue plays that entry"
ui "click 28 $HEAD" 'wait 400' 'dump p9.json'
check p9.json 'not p["player_open"]' "the player closes"

echo "== search"
ui "click $TAB_SEARCH $TABS" 'wait 500' 'text 꽃' 'wait 600' 'dump s1.json' "click 150 $((TOP + 50 + 4 + 44 + 8 + 32 + 8 + 31))" 'wait 1200' 'dump s2.json'
check s1.json 'p["tab"] == "Search" and p["search_rows"] == 4' "a Korean search finds 4 songs"
check s2.json 'd["state"] == "Playing" and d["queue_len"] == 4' "a tap on a result plays what was found"

echo "== playlists"
ui "click $TAB_PLAYLISTS $TABS" 'wait 500' "click $RIGHT $HEAD" 'wait 600' 'dump l1.json' 'text  하나' 'wait 300' 'key Enter' 'wait 900' 'dump l2.json'
check l1.json 'p["tab"] == "Playlists" and d["dialog_open"]' "the plus asks for a playlist's name"
check l2.json 'not d["dialog_open"] and p["page"].startswith("List(Playlist(")' "the new playlist opens"

echo "== settings"
ui "click $TAB_SETTINGS $TABS" 'wait 500' "click 200 $MENU1" 'wait 600' 'dump t1.json' "click 26 $HEAD" 'wait 300' "click 200 $MENU5" 'wait 600' 'dump t2.json'
check t1.json 'p["page"] == "Settings(Playback)"' "the playback settings open"
check t2.json 'p["page"] == "Settings(About)"' "the about page opens"

echo "== restart"
ui 'wait 300' 'dump r0.json'
start
ui 'wait 800' 'dump r1.json'
check r1.json 'd["library_tracks"] == 20 and d["state"] == "Paused" and d["volume"] == 0' "a restart restores the library and the session, paused and silent"
python3 - "$PROFILE/r0.json" "$PROFILE/r1.json" <<'PY'
import json, sys
before, after = (json.load(open(f))["current"] for f in sys.argv[1:3])
ok = bool(before) and before == after
print(("  ok   " if ok else "  FAIL ") + "with the song that was on")
sys.exit(0 if ok else 1)
PY

echo "== the app's container moves (a reinstall gives it another path)"
BEFORE=$(container)
scripts/ios-sim.sh "$PROFILE" >/dev/null
sleep 6
AFTER=$(container)
ui 'volume 0' 'wait 1500' 'dump m1.json'
[ "$BEFORE" != "$AFTER" ] && echo "  ok   the container has another path" || echo "  note the simulator kept the container's path: the move was not exercised"
check m1.json 'd["library_tracks"] == 20 and not d["scan_running"]' "the library followed: 20 tracks"
ROOTS=$(sqlite3 "$AFTER/Library/Application Support/OnEum/library.db" 'select count(*) from roots; select count(*) from tracks where available = 0;' | tr '\n' ' ')
[ "$ROOTS" = "1 0 " ] && echo "  ok   one music folder, every track available" || { echo "  FAIL roots/unavailable: $ROOTS"; exit 1; }

echo "SMOKE TEST PASSED (iPhone Simulator)"
