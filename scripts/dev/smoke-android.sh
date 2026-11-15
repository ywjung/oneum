#!/bin/sh
# End-to-end smoke test of the Android app on an emulator: installs it afresh, puts a
# synthetic library into a folder of the emulator's shared storage, drives the phone
# screens through the verification channel (src/ui/automation.rs) and checks the state
# the app and the system report. Silent: app volume 0, and the emulator's media volume
# is set to 0. Needs ffmpeg for the test library, the Android SDK with an NDK
# (scripts/android-env.sh) and a running emulator:
#
#   QEMU_AUDIO_DRV=none ~/Library/Android/sdk/emulator/emulator -avd <name> -no-window &
#   scripts/dev/smoke-android.sh
#
#   ONEUM_ANDROID_SERIAL   the emulator to use (as `adb devices` names it); default: the only one
#
# The app and its data on that emulator are removed first and again at the end, and so
# is the folder the test puts there. It refuses to run on a real phone (which has an
# app and music of its owner's).
set -eu
cd "$(dirname "$0")/../.."
. scripts/android-env.sh
ADB="$ANDROID_SDK/platform-tools/adb"
[ -n "${ONEUM_ANDROID_SERIAL:-}" ] && ADB="$ADB -s $ONEUM_ANDROID_SERIAL"
PKG=app.oneum.android
REMOTE=/sdcard/Download/oneum-smoke
VERIFY=/data/user/0/$PKG/files/verify
WORK=$(mktemp -d /tmp/oneum-android-smoke.XXXXXX)
MUSIC="$WORK/music"
OUT="$WORK/out"
mkdir -p "$OUT"
# shellcheck disable=SC2086
[ "$($ADB shell getprop ro.kernel.qemu | tr -d '\r')" = 1 ] || [ "$($ADB shell getprop ro.boot.qemu | tr -d '\r')" = 1 ] \
  || { echo "this is not an emulator: the smoke test only runs on one" >&2; exit 1; }
cleanup() {
  $ADB shell am force-stop $PKG >/dev/null 2>&1 || true
  $ADB uninstall $PKG >/dev/null 2>&1 || true
  $ADB shell rm -rf "$REMOTE" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

ui() { # ui <command>…: sends commands and fetches the files of any shot/dump
  outs=""
  for c in "$@"; do
    case "$c" in
      shot\ *|dump\ *)
        f="${c#* }"; outs="$outs $f"
        $ADB shell "run-as $PKG rm -f files/verify/$f" </dev/null
        printf '%s %s\n' "${c%% *}" "$VERIFY/$f" | $ADB shell "run-as $PKG sh -c 'cat >> files/verify/script.txt'" ;;
      *) printf '%s\n' "$c" | $ADB shell "run-as $PKG sh -c 'cat >> files/verify/script.txt'" ;;
    esac
  done
  for f in $outs; do
    i=0
    while ! $ADB shell "run-as $PKG test -f files/verify/$f" </dev/null 2>/dev/null && [ $i -lt 300 ]; do
      sleep 0.2; i=$((i + 1))
    done
    sleep 0.3
    $ADB exec-out run-as $PKG cat "files/verify/$f" > "$OUT/$f" </dev/null
    [ -s "$OUT/$f" ] || { echo "timed out waiting for $f" >&2; exit 1; }
  done
}
check() { # check <dump-file> <python expression over d> <description>
  python3 - "$OUT/$1" "$2" "$3" <<'PY'
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
say() { # say <condition as a shell test result> <description>
  if [ "$1" = 0 ]; then echo "  ok   $2"; else echo "  FAIL $2"; exit 1; fi
}
start() { # (re)starts the installed app with the verification channel
  $ADB shell am force-stop $PKG
  $ADB shell "run-as $PKG sh -c 'mkdir -p files/verify && : > files/verify/script.txt'"
  $ADB shell am start -n $PKG/.MainActivity \
    --es ONEUM_SCRIPT "$VERIFY/script.txt" --es ONEUM_MUSIC_DIR "$REMOTE" >/dev/null
  sleep 6
}
session() { # what the system's media session says of the app: "PLAYING" / "PAUSED" / ""
  $ADB shell dumpsys media_session | grep -A14 "package=$PKG" \
    | sed -n 's/.*state=PlaybackState {state=\([A-Z_]*\).*/\1/p' | head -1
}

echo "== generating test library"
scripts/dev/gen-test-library.sh "$MUSIC" >/dev/null
echo "== building and installing afresh"
APK=$(scripts/android-build.sh --automation | tail -1)
$ADB shell am force-stop $PKG >/dev/null 2>&1 || true
$ADB uninstall $PKG >/dev/null 2>&1 || true
$ADB install "$APK" >/dev/null
$ADB shell cmd audio set-volume 3 0 >/dev/null 2>&1 || true
$ADB shell rm -rf "$REMOTE"
$ADB shell mkdir -p "$REMOTE"
$ADB push "$MUSIC/." "$REMOTE/" >/dev/null
# Notifications are allowed beforehand, so that the system's question does not come up
# over the app in the middle of the test.
$ADB shell pm grant $PKG android.permission.POST_NOTIFICATIONS >/dev/null 2>&1 || true

echo "== first run, without the right to read files"
start
ui 'volume 0' 'wait 500' 'dump a.json'
check a.json 'd["library_tracks"] == 0 and d["state"] == "Idle"' "starts with an empty library, idle"
check a.json 'p["tab"] == "Library" and p["page"] is None' "the phone screens are shown, on the library tab"
check a.json 'd["screen"]["full"][3] > d["screen"]["full"][2] > 300' "the whole screen is drawn on, upright"
check a.json 'd["screen"]["safe"][1] > 0 and d["screen"]["safe"][3] < d["screen"]["full"][3]' "the status bar and the navigation bar are kept clear"
# Where things are on this screen (the constants of src/ui/phone.rs).
eval "$(python3 - "$OUT/a.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))["screen"]
w, top, bottom, full = s["full"][2], s["safe"][1], s["safe"][3], s["full"][3]
print(f"W={w:.0f} TOP={top:.0f} BOTTOM={bottom:.0f}")
print(f"HEAD={top + 25:.0f} TABS={bottom - 27:.0f} MINI={bottom - 54 - 31:.0f}")
for i, name in enumerate(["LIBRARY", "SEARCH", "PLAYLISTS", "SETTINGS"]):
    print(f"TAB_{name}={w * (i + 0.5) / 4:.0f}")
for i in range(8):
    print(f"MENU{i}={top + 50 + 54 * i + 27:.0f}")
print(f"SONG0={top + 173:.0f} SONG1={top + 235:.0f} SONG2={top + 297:.0f}")
foot = full - 8 - (full - bottom)
for i in range(4):
    print(f"SHEET_UP{i}={foot - 26 - 52 * i:.0f}")
step = (w - 56) / 5
body = bottom - top - 50
side = max(min(w - 56, body - 308), 120)
spare = max(body - 308 - side, 0)
controls = top + 50 + 8 + side + 16 + 0.6 * spare
print(f"PLAY_X={28 + step * 2.5:.0f} NEXT_X={28 + step * 3.5:.0f} PLAY_Y={controls + 156:.0f}")
print(f"RIGHT={w - 28:.0f} VIEW_METERS={w - 70:.0f}")
PY
)"

echo "== the right to read files is given"
$ADB shell appops set $PKG MANAGE_EXTERNAL_STORAGE allow
start
ui 'volume 0' 'wait 2500' 'dump b.json'
check b.json 'd["library_tracks"] == 20 and not d["scan_running"]' "20 tracks read from the music folder (1 unreadable file skipped)"
check b.json 'd["albums"] == 7' "albums grouped"

echo "== lists and the system's Back"
ui "tap 200 $MENU0" 'wait 700' 'dump c1.json'
check c1.json 'p["page"] == "List(Songs)" and not p["moving"]' "the song list opens"
$ADB shell input keyevent KEYCODE_BACK
ui 'wait 700' 'dump c2.json'
check c2.json 'p["page"] is None' "Back returns to the first page"
ui "tap 200 $MENU0" 'wait 600' "tap $((W - 30)) $SONG0" 'wait 700' 'dump c3.json'
check c3.json 'p["sheet"] is not None and p["sheet"].startswith("Track")' "the more button of a song puts up its sheet"
$ADB shell input keyevent KEYCODE_BACK
ui 'wait 700' 'dump c4.json'
check c4.json 'p["sheet"] is None and p["page"] == "List(Songs)"' "Back puts the sheet away and leaves the page"
ui "tap $TAB_PLAYLISTS $TABS" 'wait 500' 'dump c5.json'
check c5.json 'p["tab"] == "Playlists"' "another tab"
$ADB shell input keyevent KEYCODE_BACK
ui 'wait 600' 'dump c6.json'
check c6.json 'p["tab"] == "Library"' "Back returns to the library tab"

echo "== playback"
ui "tap 150 $SONG0" 'wait 2500' 'dump p1.json'
check p1.json 'd["state"] == "Playing" and d["queue_len"] == 20 and d["queue_index"] == 0' "a tap plays the song and queues the list"
check p1.json 'd["volume"] == 0 and d["underruns"] == 0 and d["output_device"]' "audio reaches the output without underruns, silently"
ui 'wait 1200' 'dump p2.json'
check p2.json 'd["position_ms"] > 1500' "the position advances"
[ "$(session)" = PLAYING ]; say $? "the system's media session says it is playing"
$ADB shell dumpsys activity services $PKG | grep -q "isForeground=true"; say $? "the playback service is in the foreground"
ui "tap 150 $MINI" 'wait 700' 'dump p3.json'
check p3.json 'p["player_open"]' "the bar of the playing song opens the player"
ui "tap $PLAY_X $PLAY_Y" 'wait 600' 'dump p4.json' "tap $PLAY_X $PLAY_Y" 'wait 800' 'dump p5.json'
check p4.json 'd["state"] == "Paused"' "the play button pauses"
check p5.json 'd["state"] == "Playing"' "and resumes"
ui "tap $NEXT_X $PLAY_Y" 'wait 1200' 'dump p6.json'
check p6.json 'd["queue_index"] == 1 and d["state"] == "Playing"' "next"
ui "tap $VIEW_METERS $HEAD" 'wait 1500' 'dump v1.json'
check v1.json 'p["player_view"] == "Meters" and d["meter"]["position"][0] > 0' "the VU meters follow what is playing"
$ADB shell input keyevent KEYCODE_BACK
ui 'wait 700' 'dump p7.json'
check p7.json 'not p["player_open"] and d["state"] == "Playing"' "Back puts the player away and the music plays on"

echo "== buttons of the system (a headset, the lock screen)"
$ADB shell cmd media_session dispatch pause >/dev/null
ui 'wait 1200' 'dump m1.json'
check m1.json 'd["state"] == "Paused"' "a pause from outside pauses"
[ "$(session)" = PAUSED ]; say $? "and the media session says so"
$ADB shell cmd media_session dispatch play >/dev/null
ui 'wait 1200' 'dump m2.json'
check m2.json 'd["state"] == "Playing"' "a play from outside plays"
$ADB shell cmd media_session dispatch next >/dev/null
ui 'wait 1500' 'dump m3.json'
check m3.json 'd["queue_index"] == 2 and d["state"] == "Playing"' "next from outside"

echo "== media keys while the app is on the screen (wired earphones, a keyboard)"
# These come to the window in front, not to the media session.
$ADB shell input keyevent KEYCODE_MEDIA_PLAY_PAUSE
ui 'wait 1500' 'dump mk1.json'
check mk1.json 'd["state"] == "Paused"' "the play/pause key pauses"
$ADB shell input keyevent KEYCODE_MEDIA_PLAY
ui 'wait 1200' 'dump mk2.json'
check mk2.json 'd["state"] == "Playing"' "the play key plays"
$ADB shell input keyevent KEYCODE_MEDIA_NEXT
ui 'wait 1500' 'dump mk3.json'
check mk3.json 'd["queue_index"] == 3 and d["state"] == "Playing"' "the next key goes on"

echo "== a keyboard that is plugged in or paired"
# Its keys are the app's shortcuts while no field is typed into, and the input method
# must not come up for them (it did for Ctrl alone).
$ADB shell input keyevent KEYCODE_SPACE
ui 'wait 1200' 'dump kb1.json'
check kb1.json 'd["state"] == "Paused"' "Space pauses"
$ADB shell input keyevent KEYCODE_SPACE
ui 'wait 1200' 'dump kb2.json'
check kb2.json 'd["state"] == "Playing"' "and plays"
$ADB shell input keycombination KEYCODE_CTRL_LEFT KEYCODE_DPAD_RIGHT
ui 'wait 1500' 'dump kb3.json'
check kb3.json 'd["queue_index"] == 4 and d["state"] == "Playing"' "Ctrl+Right goes to the next song"
$ADB shell input keycombination KEYCODE_CTRL_LEFT KEYCODE_U
ui 'wait 900' 'dump kb4.json'
check kb4.json 'p["player_open"] and p["player_view"] == "Queue"' "Ctrl+U shows the queue"
$ADB shell input keyevent KEYCODE_ESCAPE
ui 'wait 900' 'dump kb5.json'
check kb5.json 'not p["player_open"] and d["state"] == "Playing"' "Escape goes back"
[ "$($ADB shell dumpsys input_method | grep -c 'mInputShown=true')" = 0 ]
say $? "the on-screen keyboard stayed away"

echo "== a call"
$ADB emu gsm call 0101234567 >/dev/null
ui 'wait 3000' 'dump k1.json'
check k1.json 'd["state"] == "Paused"' "the music pauses when a call comes in"
# It is to play on after the call, also from behind the screen: a service that left the
# foreground now could not come back to it there.
$ADB shell dumpsys activity services $PKG | grep -q 'isForeground=true'
say $? "and the playback service keeps the foreground for the call"
$ADB emu gsm cancel 0101234567 >/dev/null
ui 'wait 3000' 'dump k2.json'
check k2.json 'd["state"] == "Playing"' "and plays on when the call is over"

echo "== search"
ui "tap $TAB_SEARCH $TABS" 'wait 500' "tap 150 $((TOP + 72))" 'wait 600' 'text 꽃' 'wait 700' 'dump s1.json'
check s1.json 'p["tab"] == "Search" and p["search_rows"] == 4' "a Korean search finds 4 songs"
$ADB shell input keyevent KEYCODE_BACK
ui 'wait 700' 'dump s2.json'
check s2.json 'not d["text_focus"] and p["tab"] == "Search"' "Back gives the keyboard up first"

echo "== off screen and back"
$ADB shell input keyevent KEYCODE_HOME
sleep 5
[ "$(session)" = PLAYING ]; say $? "the music plays on with the app off screen"
$ADB shell am start -n $PKG/.MainActivity >/dev/null 2>&1
sleep 2
ui 'wait 600' 'dump h1.json'
check h1.json 'd["state"] == "Playing" and d["underruns"] == 0' "and the app comes back as it was"

echo "== restart"
ui "tap $TAB_LIBRARY $TABS" 'wait 400' 'dump r0.json'
start
ui 'wait 800' 'dump r1.json'
check r1.json 'd["library_tracks"] == 20 and d["state"] == "Paused" and d["volume"] == 0' "a restart restores the library and the session, paused and silent"
python3 - "$OUT/r0.json" "$OUT/r1.json" <<'PY'
import json, sys
before, after = (json.load(open(f))["current"] for f in sys.argv[1:3])
ok = bool(before) and before == after
print(("  ok   " if ok else "  FAIL ") + "with the song that was on")
sys.exit(0 if ok else 1)
PY

echo "SMOKE TEST PASSED (Android emulator)"
