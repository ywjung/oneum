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
#   ONEUM_GRAPHICS         gl, vulkan or glow: what every start of the app draws through
#                          (default: the app's own choice, Vulkan first; glow is the
#                          UI framework's OpenGL renderer, without wgpu)
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
GRAPHICS=
[ -z "${ONEUM_GRAPHICS:-}" ] || GRAPHICS="--es ONEUM_GRAPHICS $ONEUM_GRAPHICS"
DLNA_PID=
. scripts/dev/smoke-webdav.sh
. scripts/dev/smoke-lyrics.sh
. scripts/dev/smoke-room.sh
cleanup() {
  [ -z "$DLNA_PID" ] || kill "$DLNA_PID" 2>/dev/null || true
  for p in $DAV_PIDS; do kill "$p" 2>/dev/null || true; done
  $ADB shell am force-stop $PKG >/dev/null 2>&1 || true
  $ADB uninstall $PKG >/dev/null 2>&1 || true
  $ADB shell rm -rf "$REMOTE" "$REMOTE-room.wav" >/dev/null 2>&1 || true
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
holds() { # holds <description> <command…>: runs the command and says whether it holds
  # (As the test of an "if": a command that fails on a line of its own ends the script
  # under "set -e" before anything is said of it.)
  what=$1; shift
  if "$@"; then say 0 "$what"; else say 1 "$what"; fi
}
start() { # (re)starts the installed app with the verification channel
  $ADB shell am force-stop $PKG
  $ADB shell "run-as $PKG sh -c 'mkdir -p files/verify && : > files/verify/script.txt'"
  # (A paused service keeps the foreground for ten minutes; six seconds in this run,
  # so that its leaving can be seen. Only a debuggable build takes this.)
  # shellcheck disable=SC2086
  $ADB shell am start -n $PKG/.MainActivity \
    --es ONEUM_SCRIPT "$VERIFY/script.txt" --es ONEUM_MUSIC_DIR "$REMOTE" \
    --es ONEUM_PAUSED_FOREGROUND_MS 6000 $GRAPHICS >/dev/null
  sleep 6
}
session() { # what the system's media session says of the app: "PLAYING" / "PAUSED" / ""
  $ADB shell dumpsys media_session | grep -A14 "package=$PKG" \
    | sed -n 's/.*state=PlaybackState {state=\([A-Z_]*\).*/\1/p' | head -1
}
session_is() { [ "$(session)" = "$1" ]; }
in_foreground() { $ADB shell dumpsys activity services $PKG | grep -q 'isForeground=true'; }
not_in_foreground() { ! in_foreground; }
keyboard_away() { [ "$($ADB shell dumpsys input_method | grep -c 'mInputShown=true')" = 0 ]; }

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
# With an interface asked for, the app says that it draws through that one (else a run
# "on OpenGL ES" could pass on Vulkan). The adapter ends "(<interface>, <kind>)", or
# "; glow)" under the UI framework's OpenGL renderer.
case "${ONEUM_GRAPHICS:-}" in
  glow) check a.json 'd["adapter"].endswith("; glow)")' "the screen is drawn by the OpenGL renderer that was asked for (glow)" ;;
  gl) check a.json 'd["adapter"].rsplit("(", 1)[1].startswith("Gl,")' "the screen is drawn through OpenGL ES, as asked" ;;
  vulkan) check a.json 'd["adapter"].rsplit("(", 1)[1].startswith("Vulkan,")' "the screen is drawn through Vulkan, as asked" ;;
esac
check a.json 'd["library_tracks"] == 0 and d["state"] == "Idle"' "starts with an empty library, idle"
check a.json 'p["tab"] == "Library" and p["page"] is None' "the phone screens are shown, on the library tab"
check a.json 'd["screen"]["full"][3] > d["screen"]["full"][2] > 300' "the whole screen is drawn on, upright"
check a.json 'd["adapter"] and int(d["memory"].split()[0].split("=")[1]) > 100 and " atlas=4096x" in d["memory"]' "the app tells what it draws with and what it holds of memory (the glyphs in a picture 4096 wide)"
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
print(f"RIGHT={w - 28:.0f} VIEW_METERS={w - 70:.0f} VIEW_LYRICS={w - 112:.0f}")
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
holds "the system's media session says it is playing" session_is PLAYING
holds "the playback service is in the foreground" in_foreground
ui "tap 150 $MINI" 'wait 700' 'dump p3.json'
check p3.json 'p["player_open"]' "the bar of the playing song opens the player"
ui "tap $PLAY_X $PLAY_Y" 'wait 600' 'dump p4.json' "tap $PLAY_X $PLAY_Y" 'wait 800' 'dump p5.json'
check p4.json 'd["state"] == "Paused"' "the play button pauses"
check p5.json 'd["state"] == "Playing"' "and resumes"
ui "tap $NEXT_X $PLAY_Y" 'wait 1200' 'dump p6.json'
check p6.json 'd["queue_index"] == 1 and d["state"] == "Playing"' "next"
ui "tap $VIEW_METERS $HEAD" 'wait 1500' 'dump v1.json'
check v1.json 'p["player_view"] == "Meters" and d["meter"]["position"][0] > 0' "the VU meters follow what is playing"
ui "tap $VIEW_LYRICS $HEAD" 'wait 800' 'dump v2.json'
check v2.json 'p["player_view"] == "Lyrics"' "the lyrics take the cover's place"
# Lyrics from the internet, while the lyrics of a song without any are in view. (The
# emulator reaches this computer at 10.0.2.2.)
lyrics_checks 10.0.2.2 0.0.0.0 "$OUT"
# (Beside the music folder, not in it: the library would take it for a song.)
room_file "$WORK/room.wav"
$ADB push "$WORK/room.wav" "$REMOTE-room.wav" >/dev/null
room_checks "$REMOTE-room.wav"
$ADB shell input keyevent KEYCODE_BACK
ui 'wait 700' 'dump p7.json'
check p7.json 'not p["player_open"] and d["state"] == "Playing"' "Back puts the player away and the music plays on"

echo "== buttons of the system (a headset, the lock screen)"
$ADB shell cmd media_session dispatch pause >/dev/null
ui 'wait 1200' 'dump m1.json'
check m1.json 'd["state"] == "Paused"' "a pause from outside pauses"
holds "and the media session says so" session_is PAUSED
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
holds "the on-screen keyboard stayed away" keyboard_away

echo "== a call"
$ADB emu gsm call 0101234567 >/dev/null
ui 'wait 3000' 'dump k1.json'
check k1.json 'd["state"] == "Paused"' "the music pauses when a call comes in"
# It is to play on after the call, also from behind the screen: a service that left the
# foreground now could not come back to it there.
holds "and the playback service keeps the foreground for the call" in_foreground
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
holds "the music plays on with the app off screen" session_is PLAYING
$ADB shell am start -n $PKG/.MainActivity >/dev/null 2>&1
sleep 2
ui 'wait 600' 'dump h1.json'
check h1.json 'd["state"] == "Playing" and d["underruns"] == 0' "and the app comes back as it was"

echo "== paused off screen"
# The paused service keeps the foreground for a while: one that leaves it at once is
# stopped by the system about a minute later, and the notification loses its buttons.
$ADB shell input keyevent KEYCODE_HOME
sleep 2
$ADB shell cmd media_session dispatch pause >/dev/null
sleep 1.5
holds "a pause from outside pauses with the app off screen" session_is PAUSED
holds "and the playback service keeps the foreground at first" in_foreground
sleep 7
holds "and leaves it when its time is over" not_in_foreground
$ADB shell cmd media_session dispatch play >/dev/null
sleep 2
holds "a play from outside plays again" session_is PLAYING
holds "from the foreground" in_foreground
$ADB shell am start -n $PKG/.MainActivity >/dev/null 2>&1
sleep 2

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

echo "== a media server of the network"
# A stand-in for a DLNA server shows the test library from this computer, which the
# emulator reaches at 10.0.2.2. The app is given its address: nothing is asked of the
# real network.
python3 scripts/dev/fake-dlna.py "$MUSIC" --host 0.0.0.0 > "$WORK/dlna.out" 2>&1 &
DLNA_PID=$!
i=0
until grep -q '^listening ' "$WORK/dlna.out" 2>/dev/null; do
  sleep 0.2; i=$((i + 1))
  [ $i -lt 50 ] || { echo "the stand-in media server did not start" >&2; exit 1; }
done
DLNA=$(sed -n 's/^listening http:\/\/0\.0\.0\.0/http:\/\/10.0.2.2/p' "$WORK/dlna.out")
entry() { # entry <dump-file> <title>: the place of that entry in the open folder
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["network"]["titles"].index(sys.argv[2]))' "$OUT/$1" "$2"
}
ui 'volume 0' "net add $DLNA" 'wait 2500' 'dump n1.json'
check n1.json 'd["network"]["server"] == "OnEum test server" and d["network"]["entries"] == 7 and d["network"]["folders"] == 5' "a media server added by its address opens: 5 folders and 2 songs"
check n1.json 'd["network"]["saved"] == ["OnEum test server"]' "and is kept in the list of servers"
ui "net open $(entry n1.json 아이유)" 'wait 1200' 'net open 0' 'wait 1200' 'dump n3.json'
check n3.json 'd["network"]["depth"] == 2 and d["network"]["playable"] == 4' "the album in a folder of it has 4 songs that can be played"
ui 'net open 1' 'wait 3000' 'dump n4.json' 'wait 1500' 'dump n5.json'
check n4.json 'd["state"] == "Playing" and d["current"] == "02 꽃" and d["queue_len"] == 4 and d["queue_index"] == 1' "a song of the server plays, with its folder as the queue"
check n4.json 'd["current_path"].startswith("http://10.0.2.2:")' "read from the server's address"
check n5.json 'd["position_ms"] > 3500 and d["underruns"] == 0 and d["library_tracks"] == 20' "the music goes on, and the library is as it was"
# What was added is a setting, and settings are written to their file. (On Android none
# were, once: the app there is told no window size, and saving hung on having one.)
start
ui 'wait 800' 'dump n6.json'
check n6.json 'd["network"]["saved"] == ["OnEum test server"] and d["current"] == "02 꽃"' "after a restart the server is still listed, and its song is the one that was on"
kill "$DLNA_PID" 2>/dev/null || true
DLNA_PID=

echo "== a network folder (WebDAV)"
# (The emulator reaches this computer at 10.0.2.2.)
ui 'volume 0' 'wait 300'
webdav_checks 10.0.2.2 0.0.0.0
check w9.json 'd["network"]["secret_selftest"].startswith("ok")' "the phone's key store keeps, gives back, replaces and forgets a password"

echo "== a song removed from the library stays out"
# Removed through the song's sheet, as it is done by hand. Its file stays where it is,
# and the next scan of its folder (every start begins with one) must not bring the song
# back: it did (reported from an iPhone, 2026-11-15), because only the library's row
# went. "Bring Back Removed Songs" is the way back.
ui "tap $TAB_LIBRARY $TABS" 'wait 500' "tap 200 $MENU0" 'wait 700' 'dump x0.json'
SONGS=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["library_tracks"])' "$OUT/x0.json")
ui "tap $((W - 30)) $SONG0" 'wait 700' "tap 200 $SHEET_UP0" 'wait 700' 'key Enter' 'wait 900' 'dump x1.json'
check x1.json "d['library_tracks'] == $SONGS - 1 and d['removed_songs'] == 1" "the song's sheet removes it from the library"
start
ui 'volume 0' 'wait 3000' 'dump x2.json'
check x2.json "d['library_tracks'] == $SONGS - 1 and not d['scan_running'] and d['removed_songs'] == 1" "after a restart and its scan the song is still out"
ui 'restore removed' 'wait 3000' 'dump x3.json'
check x3.json "d['library_tracks'] == $SONGS and d['removed_songs'] == 0" "\"Bring Back Removed Songs\" takes it in again"

# (Helpers of the sections that follow.)
pid() { $ADB shell pidof $PKG 2>/dev/null | tr -d '\r'; }
log_has() { $ADB shell "run-as $PKG grep -q '$1' files/logs/oneum.log" </dev/null; }
panics() { $ADB shell "run-as $PKG grep -c 'panic:' files/logs/oneum.log" </dev/null | tr -d '\r'; }
opened_again() { [ -n "$after" ] && [ "$after" != "$before" ]; }
no_new_panic() { [ "$(panics)" = "$panics_before" ]; }
app_gone() { [ -z "$(pid)" ]; }
echo "== the graphics device is lost"
# A driver can take away the device the screen is drawn with at any time (on a phone
# one did, 2026-11-15), and the app then ended at the next frame. The step takes the
# device away as a driver does (under the UI framework's OpenGL renderer, which has no
# such device, it says the loss as the app does of a driver that ran out of memory).
# The app saves what it keeps, opens again in a new process and plays on; that run has
# no verification channel (it was not started with one), so it is asked about through
# the system and its log.
ui "tap 200 $MENU0" 'wait 700' "tap 150 $SONG0" 'wait 2500' 'dump g1.json'
check g1.json 'd["state"] == "Playing" and d["volume"] == 0' "a song of the library plays"
check g1.json 'd["graphics_errors"] == 0' "what the screen is drawn with has told of no error in all of this run"
# The home screen widget: the system knows it, and it is told the song that plays (what
# a launcher shows of it was looked at by hand: no launcher is driven here).
widget_known() { $ADB shell dumpsys appwidget </dev/null | grep -q "$PKG/$PKG.NowPlayingWidget"; }
widget_has_song() { $ADB shell "run-as $PKG cat shared_prefs/widget.xml" </dev/null | grep -q '<string name="title">..*</string>'; }
holds "the home screen widget is known to the system" widget_known
holds "and is told the song that plays" widget_has_song
# A section repeated, stars and the library's data: what the desk's smoke test shows
# of these in full, by the same commands; here, that they work on a phone.
ui 'menu repeat_section' 'wait 1500' 'menu repeat_section' 'wait 300' 'dump n1.json' 'wait 3500' 'dump n2.json' \
   'menu repeat_section' 'wait 300' 'dump n3.json'
check n1.json 'd["section"] is not None and d["section"][1] is not None and d["state"] == "Playing"' "a section of the song is marked, from one press to the next"
# (Going back to the beginning of the section is a seek: for a moment of every round
# the state says so.)
check n2.json 'd["state"] in ("Playing", "Seeking") and d["section"][0] - 300 <= d["position_ms"] <= d["section"][1] + 500' "left alone, the song plays inside the section, over and over"
check n3.json 'd["section"] is None and d["state"] in ("Playing", "Seeking")' "the third press gives the section up"
ui 'menu rate_current 4' 'wait 600' 'dump n4.json' "menu data_export $VERIFY/data.json" 'wait 1500' 'dump n5.json' \
   'menu rate_current 0' 'wait 600' 'dump n6.json' "menu data_import $VERIFY/data.json" 'wait 1500' 'dump n7.json'
check n4.json 'd["current_rating"] == 4' "the song that is playing is given four stars"
$ADB exec-out run-as $PKG cat files/verify/data.json > "$OUT/data.json" </dev/null
if python3 - "$OUT/data.json" "$OUT/n5.json" <<'PY'
import json, sys
text = open(sys.argv[1]).read()
d = json.loads(text)
state = json.load(open(sys.argv[2]))
assert d["format"] == "oneum-library-data" and d["version"] == 1
assert len(d["songs"]) == state["library_tracks"], (len(d["songs"]), state["library_tracks"])
assert [song.get("rating", 0) for song in d["songs"]].count(4) == 1
assert "/storage/" not in text and "/data/" not in text, "the file tells where the music lies"
PY
then echo "  ok   the library's data is written: every song, the stars, and no place on the device"
else echo "  FAIL the library data file is not as expected"; exit 1
fi
check n6.json 'd["current_rating"] == 0' "the stars are taken away"
check n7.json 'd["current_rating"] == 4' "…and the data, taken in again, gives them back: its songs are found here"
ui 'menu crossfade 4' 'wait 400' 'dump n8.json' 'menu crossfade 0' 'wait 400' 'dump n9.json'
check n8.json 'd["crossfade_secs"] == 4 and d["state"] == "Playing"' "crossfade is set to four seconds while a song plays"
check n9.json 'd["crossfade_secs"] == 0' "…and switched off again"
# (A change of speed is a seek to where the song is: for a moment the state says so.)
ui 'menu speed 150' 'wait 900' 'dump n14.json' 'menu speed 100' 'wait 900' 'dump n15.json'
check n14.json 'd["speed_percent"] == 150 and d["state"] in ("Playing", "Seeking")' "the playback speed is set to 1.5× while a song plays"
check n15.json 'd["speed_percent"] == 100 and d["state"] in ("Playing", "Seeking")' "…and back to the song's own"
ui 'menu shuffle_by_rating' 'wait 300' 'dump n12.json' 'menu shuffle_by_rating' 'wait 300' 'dump n13.json'
check n12.json 'd["shuffle_by_rating"]' "shuffling by stars is switched on"
check n13.json 'not d["shuffle_by_rating"]' "…and off again"
ui 'menu correction_edit_new' 'wait 300' 'menu correction_edit_band 3200 5 2.5' 'wait 400' 'dump n10.json' \
   'menu correction_edit_save' 'wait 400' 'dump n11.json' 'menu correction_off' 'wait 300'
check n10.json 'd["correction_editing"][1] == 2 and d["correction"]["applied"] == d["correction_editing"][0]' "a correction profile is made band by band, and heard while it is edited"
check n11.json 'd["correction_editing"] is None and d["correction"]["profiles"][-1][1] == 2 and d["correction"]["selected"] == d["correction"]["profiles"][-1][0]' "saved, it is one of the user's own profiles and selected"
# Where a year begins for the listener is a matter of the device's time zone.
if python3 - "$OUT/n11.json" "$($ADB shell date +%z </dev/null | tr -d '\r')" <<'PY'
import json, sys
told = json.load(open(sys.argv[1]))["utc_offset_secs"]
zone = sys.argv[2]
device = (1 if zone[0] == "+" else -1) * (int(zone[1:3]) * 3600 + int(zone[3:5]) * 60)
assert told == device, (told, zone)
PY
then echo "  ok   the app knows how far the device's clock is from UTC"
else echo "  FAIL the app's offset from UTC is not the device's"; exit 1
fi
lose=device
if [ "${ONEUM_GRAPHICS:-}" = glow ]; then
  # That no error was told means something only if the driver is really asked: OpenGL
  # is given a call that it refuses, and the refusal is to stand in the log.
  ui 'fail gl' 'wait 1000' 'dump g2.json'
  holds "OpenGL is asked what it refused: a call it refuses stands in the log" log_has 'graphics: OpenGL: invalid enum (0x0500)'
  check g2.json 'd["graphics_errors"] == 1' "and is counted, once"
  # A driver that gives up under that renderer does so when a frame is to be shown:
  # the surface is taken away, the framework cannot show the frame and stops, and the
  # app is to open again all the same.
  lose=surface
fi
before=$(pid)
panics_before=$(panics)
ui "fail $lose"
i=0; after=$(pid)
while { [ -z "$after" ] || [ "$after" = "$before" ]; } && [ $i -lt 75 ]; do
  sleep 0.2; i=$((i + 1)); after=$(pid)
done
sleep 4
holds "the app opened again in a new process" opened_again
holds "its log says that it started anew, and that music was playing" log_has 'graphics: started anew after the device was lost playing=true'
if [ "${ONEUM_GRAPHICS:-}" = glow ]; then
  # That renderer is a choice of its own, with no other interface to turn to: the note
  # of the loss names none (the run that follows goes by the settings).
  names_no_interface() { $ADB shell "run-as $PKG cat files/data/relaunch.txt" </dev/null | tr -d '\r' | grep -q ' -$'; }
  holds "the loss names no interface to turn from" names_no_interface
else
  # While the settings leave the choice to the app, the new run draws through the other
  # interface than the one the device was lost on.
  other=gl; [ "${ONEUM_GRAPHICS:-}" != gl ] || other=vulkan
  holds "the new run draws through the other interface ($other)" log_has "graphics: this interface is tried first interface=.$other."
  holds "which has shown that it works" log_has 'graphics: the interface on trial works'
fi
holds "the music goes on: the system's media session says it is playing" session_is PLAYING
holds "nothing ended in a panic" no_new_panic
# The same again within a minute: what failed twice in a row is not tried a third time.
start
ui 'volume 0' 'wait 500' 'fail device'
sleep 6
holds "a second loss right after closes the app instead of opening it once more" app_gone
holds "and the log says why" log_has 'graphics: lost again right after a new start'
if [ "${ONEUM_GRAPHICS:-}" = glow ]; then
  holds "the surface was taken away for the first loss" log_has 'graphics: the surface was taken away by a verification run destroyed=true'
  holds "which ended the framework's event loop: the frame could not be shown" log_has 'event loop failed'
fi

echo "== a crash of the native side"
# What the system kept of it (the signal, where the thread was) is written beside the
# log at the next start, for Settings → About: nothing else of such a crash can be
# read on a phone. The crash here is a signal sent to the app.
exits_have() { $ADB shell "run-as $PKG cat files/logs/exits.txt" </dev/null | grep -q "$1"; }
start
$ADB shell "run-as $PKG kill -SEGV $(pid)" </dev/null
sleep 4
start
holds "the crash is in the list of how earlier runs ended" exits_have 'crash (native)'
holds "with the signal" exits_have "^$(printf '\t')SIGSEGV"
holds "and where the thread was" exits_have "^$(printf '\t')#00 "

echo "== glow chosen in the settings"
# The way a user reaches the UI framework's OpenGL renderer: the settings name it (what
# the fourth choice of Settings → About writes), and the start that follows draws with
# it on trial. (ONEUM_GRAPHICS, which the rest of a run may go by, passes the settings
# and the trial by: this start is made without it.)
$ADB shell am force-stop $PKG
$ADB exec-out run-as $PKG cat files/data/settings.json > "$OUT/settings.json" </dev/null
python3 - "$OUT/settings.json" <<'PY'
import json, sys
settings = json.load(open(sys.argv[1]))
settings["graphics"] = "Glow"
json.dump(settings, open(sys.argv[1], "w"))
PY
$ADB shell "run-as $PKG sh -c 'cat > files/data/settings.json'" < "$OUT/settings.json"
no_trial_left() { ! $ADB shell "run-as $PKG test -f files/data/graphics-trying.txt" </dev/null; }
GRAPHICS=
start
ui 'volume 0' 'wait 500' 'dump s1.json'
check s1.json 'd["adapter"].endswith("; glow)")' "the app draws with the renderer the settings name"
holds "which was tried first, as a choice of the settings is" log_has 'graphics: this interface is tried first interface=.glow.'
holds "and has shown that it works: its trial is over" no_trial_left
check s1.json 'd["library_tracks"] > 0 and d["graphics_errors"] == 0' "the library is there, and nothing is told of an error"

echo "SMOKE TEST PASSED (Android emulator)"
