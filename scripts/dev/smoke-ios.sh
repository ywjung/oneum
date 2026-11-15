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
DLNA_PID=
trap '[ -z "$DLNA_PID" ] || kill "$DLNA_PID" 2>/dev/null || true; xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true; xcrun simctl uninstall "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true; rm -rf "$WORK"' EXIT
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
# A song list: Play and Shuffle, the line with "Sort" and "Select", then the rows.
print(f"SONG0={top + 173:.0f} SONG1={top + 235:.0f} SONG2={top + 297:.0f}")
print(f"PILLS={top + 83:.0f} TOOLS={top + 123:.0f} SORT_X={w - 78:.0f} SELECT_X={w - 30:.0f}")
# Lines of a sheet, counted from its last one (52 points each; the choices of the sort
# sheet 50), above the room kept for the home indicator.
full = s["full"][3]
foot = full - 8 - (full - bottom)
for i in range(8):
    print(f"SHEET_UP{i}={foot - 26 - 52 * i:.0f}")
for i in range(10):
    print(f"CHOICE_UP{i}={foot - 25 - 50 * i:.0f}")
# The handle a row is carried by, left of its "more" button.
print(f"GRIP_X={w - 77:.0f}")
step = (w - 56) / 5
# The player: the cover is as large as the screen has room for, and what is left over
# is shared out above and below it.
body = bottom - top - 50
side = max(min(w - 56, body - 308), 120)
spare = max(body - 308 - side, 0)
controls = top + 50 + 8 + side + 16 + 0.6 * spare
print(f"PLAY_X={28 + step * 2.5:.0f} NEXT_X={28 + step * 3.5:.0f} PLAY_Y={controls + 156:.0f}")
print(f"COVER_Y={top + 50 + 8 + 0.3 * spare + side / 2:.0f}")
# The "more" button beside the title of the song that plays, and in the sheet it puts
# up (eleven lines under a heading, more than the sheet shows at once on most phones:
# then it is as tall as it may be and its lines are counted from the top) the line
# that shows the song in the song list, the eighth.
print(f"MORE_X={w - 42:.0f} MORE_Y={controls + 26:.0f}")
shown = min(52 + 52 * 11, max((bottom - top) * 0.72, 200))
sheet_top = full - (26 + shown + 8 + (full - bottom))
print(f"IN_SONGS={sheet_top + 26 + 52 + 26 + 52 * 7:.0f}")
# The queue: the words at the right of its first line, and its rows.
print(f"QUEUE_TOOLS={top + 67:.0f} QUEUE_SELECT={w - 31:.0f} QUEUE_CHOSEN={w - 97:.0f}")
print(f"QUEUE2={top + 127 + 124:.0f} QUEUE3={top + 127 + 186:.0f}")
# The first switch of the playback settings.
print(f"SWITCH_X={w - 57:.0f} SWITCH0={top + 146:.0f}")
# The buttons at the right of a bar: the last one, and two of the views of the player.
print(f"RIGHT={w - 28:.0f} VIEW_METERS={w - 70:.0f} VIEW_LYRICS={w - 112:.0f}")
PY
)"

echo "== music in the Documents folder"
cp -R "$MUSIC"/* "$(container)/Documents/"
start
ui 'volume 0' 'wait 1500' 'dump b.json'
check b.json 'd["library_tracks"] == 20 and not d["scan_running"]' "20 tracks read from the Documents folder (1 unreadable file skipped)"
check b.json 'd["albums"] == 7' "albums grouped"

echo "== lists"
# (A page takes a third of a second to slide in or out.)
ui "click 200 $MENU0" 'wait 600' 'dump c1.json' "click 26 $HEAD" 'wait 600' 'dump c2.json' \
   "click 200 $MENU1" 'wait 1500' 'dump c3.json' 'wait 1000' 'dump c3b.json' \
   "click 100 $((TOP + 150))" 'wait 600' 'dump c4.json' \
   "click 26 $HEAD" 'wait 600' "click 26 $HEAD" 'wait 600' 'dump c5.json'
check c1.json 'p["page"] == "List(Songs)" and not p["moving"]' "the song list opens"
check c2.json 'p["page"] is None' "the back button returns"
check c3.json 'p["page"] == "List(Albums)"' "the albums open"
python3 - "$PROFILE/c3.json" "$PROFILE/c3b.json" <<'PY'
import json, sys
a, b = (json.load(open(f))["art"] for f in sys.argv[1:3])
ok = a["textures"] > 0 and b["pending"] == 0 and a["uploads"] == b["uploads"]
print(("  ok   " if ok else "  FAIL ") + "the covers of the grid are loaded once and kept")
if not ok:
    print("       art:", a, b)
sys.exit(0 if ok else 1)
PY
check c4.json 'p["page"].startswith("List(Album(") and p["depth"][0] == 2' "an album opens above them"
check c5.json 'p["page"] is None and p["depth"][0] == 0' "back twice is the first page again"

echo "== touch"
ui "tap 200 $MENU0" 'wait 600' "swipe 4 $((TOP + 300)) 260 $((TOP + 304)) 12" 'wait 700' 'dump g1.json'
check g1.json 'p["page"] is None and not p["moving"]' "a finger pulling from the left edge goes back"
ui "tap 200 $MENU0" 'wait 600' "tap $((W - 30)) $SONG0" 'wait 600' 'dump g2.json' \
   "tap 200 $SHEET_UP1" 'wait 700' 'dump g2b.json' \
   "tap 200 $((TOP + 20))" 'wait 600' 'dump g3.json' "tap 26 $HEAD" 'wait 600' 'dump g4.json'
check g2.json 'p["sheet"] is not None and p["sheet"].startswith("Track")' "the more button of a song puts up its sheet"
check g2b.json 'p["sheet"] is not None and p["sheet"].startswith("Info(")' "its line before the last shows what is known about the song"
check g3.json 'p["sheet"] is None' "a tap outside the sheet puts it away"
check g4.json 'p["page"] is None' "and the page under it answers again"

echo "== order, several songs at once, pins"
# "Title" is the ninth choice from the bottom of the sort sheet; a second tap turns it round.
ui "tap 200 $MENU0" 'wait 600' 'dump o0.json' "tap $SORT_X $TOOLS" 'wait 600' 'dump o1.json' \
   "tap 200 $CHOICE_UP8" 'wait 700' 'dump o2.json' \
   "tap $SORT_X $TOOLS" 'wait 600' "tap 200 $CHOICE_UP8" 'wait 700' 'dump o3.json' \
   "tap $SORT_X $TOOLS" 'wait 600' "tap 200 $CHOICE_UP9" 'wait 700' 'dump o4.json'
check o1.json 'p["sheet"] == "Sort(Songs)"' "the songs can be put in another order"
python3 - "$PROFILE/o0.json" "$PROFILE/o2.json" "$PROFILE/o3.json" "$PROFILE/o4.json" <<'PY'
import json, sys
first, by_title, turned, back = (json.load(open(f))["first_rows"] for f in sys.argv[1:5])
key = lambda title: title.casefold()
ok = (
    by_title != first
    and by_title[0] == min(by_title, key=key) and turned[0] == max(turned, key=key)
    and back == first
)
print(("  ok   " if ok else "  FAIL ") + "by title, the other way round, and back to the order they came in")
if not ok:
    print("      ", first[:3], by_title[:3], turned[:3], back[:3])
sys.exit(0 if ok else 1)
PY
ui "tap $SELECT_X $TOOLS" 'wait 500' "tap 200 $SONG0" "tap 200 $SONG2" 'wait 400' 'dump o5.json' \
   "tap 110 $PILLS" 'wait 600' 'dump o6.json' "tap 200 $SHEET_UP3" 'wait 800' 'dump o7.json'
check o5.json 'p["selecting"] == "Songs" and d["selected"] == 2' "two songs are chosen with a tap each"
check o6.json 'p["sheet"] == "Selection(Songs)"' "what can be done with them comes up"
check o7.json 'p["selecting"] is None and p["sheet"] is None and d["queue_len"] == 2' "they are added to the queue together, and choosing ends"
# An album is pinned from the sheet a long press brings up (its last line), shows at the
# top of the library, and is unpinned the same way: the rows below are where they were.
ui "tap 26 $HEAD" 'wait 600' "tap 200 $MENU1" 'wait 900' "hold 100 $((TOP + 150)) 900" 'wait 700' 'dump n1.json' \
   "tap 200 $SHEET_UP0" 'wait 700' "tap 26 $HEAD" 'wait 700' 'dump n2.json' \
   "tap 200 $((TOP + 50 + 30 + 27))" 'wait 700' 'dump n3.json' \
   "tap 26 $HEAD" 'wait 600' "hold 200 $((TOP + 50 + 30 + 27)) 900" 'wait 700' "tap 200 $SHEET_UP0" 'wait 700' 'dump n4.json'
check n1.json 'p["sheet"] is not None and p["sheet"].startswith("Album(")' "a long press on an album offers what can be done with it"
check n2.json 'd["pinned"] == 1 and p["page"] is None' "the album is pinned"
check n3.json 'p["page"].startswith("List(Album(")' "the pin at the top of the library leads to it"
check n4.json 'd["pinned"] == 0 and p["sheet"] is None' "and it is unpinned again"
# The magnifier at the top of a list searches in that list; again, and the search is over.
ui "tap 200 $MENU1" 'wait 900' "tap $RIGHT $HEAD" 'wait 500' 'text abbey' 'wait 800' 'dump k1.json' \
   "tap $RIGHT $HEAD" 'wait 600' 'dump k2.json' "tap 26 $HEAD" 'wait 600'
check k1.json 'p["searching"] == "Albums" and d["rows"] == 1' "a search inside the albums leaves the one that matches"
check k2.json 'p["searching"] is None and d["rows"] == 7' "and all of them are back when it is put away"

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
# The third entry is carried to the top by its handle.
ui "finger down $GRIP_X $((TOP + 239))" "finger move $GRIP_X $((TOP + 220))" "finger move $GRIP_X $((TOP + 150))" \
   "finger move $GRIP_X $((TOP + 92))" 'wait 200' 'dump q1.json' "finger up $GRIP_X $((TOP + 92))" 'wait 700' 'dump q2.json'
check q1.json 'p["carrying"] == [2, 0]' "a queue entry is taken hold of by its handle"
python3 - "$PROFILE/p8.json" "$PROFILE/q2.json" <<'PY'
import json, sys
before, after = (json.load(open(f)) for f in sys.argv[1:3])
a, b = before["queue"], after["queue"]
ok = b[0] == a[2] and b[1:3] == a[0:2] and b[3:] == a[3:] and after["state"] == "Playing" and after["phone"]["carrying"] is None
print(("  ok   " if ok else "  FAIL ") + "and let go of at the top: it plays third no more but first, the song that is on plays on")
if not ok:
    print("      ", a[:4], b[:4])
sys.exit(0 if ok else 1)
PY
# Two entries that are not playing are chosen and taken out together.
ui "tap $QUEUE_SELECT $QUEUE_TOOLS" 'wait 500' "tap 200 $QUEUE2" "tap 200 $QUEUE3" 'wait 400' 'dump qs1.json' \
   "tap $QUEUE_CHOSEN $QUEUE_TOOLS" 'wait 700' 'dump qs2.json' "tap 200 $SHEET_UP0" 'wait 900' 'dump qs3.json'
check qs1.json 'p["queue_selecting"] and d["queue_selected"] == 2' "two entries of the queue are chosen"
check qs2.json 'p["sheet"] == "QueueSelection"' "what can be done with them comes up"
python3 - "$PROFILE/q2.json" "$PROFILE/qs3.json" <<'PY'
import json, sys
before, after = (json.load(open(f)) for f in sys.argv[1:3])
ok = (
    after["queue_len"] == before["queue_len"] - 2
    and after["queue"][:2] == before["queue"][:2] and after["queue"][2] == before["queue"][4]
    and after["current"] == before["current"] and after["state"] == "Playing"
    and not after["phone"]["queue_selecting"] and after["queue_selected"] == 0
)
print(("  ok   " if ok else "  FAIL ") + "they leave the queue together, the song that is on plays on, and choosing ends")
if not ok:
    print("      ", before["queue_len"], before["queue"][:5], after["queue_len"], after["queue"][:5], after["phone"])
sys.exit(0 if ok else 1)
PY
ui "tap $VIEW_METERS $HEAD" 'wait 1200' 'dump v1.json' "tap $VIEW_LYRICS $HEAD" 'wait 600' 'dump v2.json' \
   "tap $VIEW_LYRICS $HEAD" 'wait 500' 'dump v3.json'
check v1.json 'p["player_view"] == "Meters" and d["meter"]["position"][0] > 0' "the VU meters follow what is playing"
check v2.json 'p["player_view"] == "Lyrics"' "the lyrics take the cover's place"
check v3.json 'p["player_view"] == "Cover"' "a second tap brings the cover back"
ui "click 28 $HEAD" 'wait 600' 'dump p9.json'
check p9.json 'not p["player_open"]' "the player closes"
ui "tap 150 $MINI" 'wait 700' "swipe 200 $COVER_Y 200 $((COVER_Y + 300)) 12" 'wait 700' 'dump p10.json'
check p10.json 'not p["player_open"] and d["state"] == "Playing"' "pulled down by its cover, the player goes away and the music plays on"
# From the first page of the library: the song that plays is shown in the song list.
ui "tap 26 $HEAD" 'wait 600' 'dump w0.json' "tap 150 $MINI" 'wait 700' "tap $MORE_X $MORE_Y" 'wait 700' 'dump w1.json' \
   "tap 200 $IN_SONGS" 'wait 900' 'dump w2.json'
check w0.json 'p["page"] is None' "back on the first page of the library"
check w1.json 'p["player_open"] and p["sheet"] is not None and p["sheet"].startswith("Track")' "the more button of the player puts up the song's sheet"
check w2.json 'not p["player_open"] and p["sheet"] is None and p["page"] == "List(Songs)" and d["state"] == "Playing"' "its line for the song list puts the player away and opens the songs"

echo "== search"
ui "click $TAB_SEARCH $TABS" 'wait 500' 'dump s0.json' "tap 150 $((TOP + 72))" 'wait 500' 'text 꽃' 'wait 600' 'dump s1.json' "click 150 $((TOP + 50 + 4 + 44 + 8 + 32 + 8 + 31))" 'wait 1200' 'dump s2.json'
check s0.json 'p["tab"] == "Search" and not d["text_focus"]' "the search tab opens without the keyboard"
check s1.json 'p["tab"] == "Search" and p["search_rows"] == 4' "a tap in the field and a Korean search find 4 songs"
check s2.json 'd["state"] == "Playing" and d["queue_len"] == 4' "a tap on a result plays what was found"
# The search is kept as a smart playlist (the plus in the field), under the name offered.
ui "tap $((W - 82)) $((TOP + 72))" 'wait 700' 'dump s3.json' 'key Enter' 'wait 1200' 'dump s4.json'
check s3.json 'd["dialog_open"]' "the plus in the search field asks for a smart playlist's name"
check s4.json 'len(d["smart_playlists"]) == 1 and p["page"] == "List(Smart(\"꽃\"))" and p["tab"] == "Playlists"' "the search is kept as a smart playlist and opens"

echo "== playlists"
ui "click $TAB_PLAYLISTS $TABS" 'wait 500' "click $RIGHT $HEAD" 'wait 600' 'dump l1.json' 'text  하나' 'wait 300' 'key Enter' 'wait 900' 'dump l2.json'
check l1.json 'p["tab"] == "Playlists" and d["dialog_open"]' "the plus asks for a playlist's name"
check l2.json 'not d["dialog_open"] and p["page"].startswith("List(Playlist(")' "the new playlist opens"

echo "== settings"
ui "click $TAB_SETTINGS $TABS" 'wait 500' "click 200 $MENU1" 'wait 600' 'dump t1.json' "click 26 $HEAD" 'wait 600' "click 200 $MENU5" 'wait 600' 'dump t2.json'
check t1.json 'p["page"] == "Settings(Playback)"' "the playback settings open"
check t2.json 'p["page"] == "Settings(About)"' "the about page opens"

echo "== output at the song's sample rate"
# The first switch of the playback settings; then a 48 kHz file and a 44.1 kHz one.
ui "click 26 $HEAD" 'wait 600' "click 200 $MENU1" 'wait 600' 'dump h0.json' "tap $SWITCH_X $SWITCH0" 'wait 500' 'dump h1.json' \
   "click $TAB_SEARCH $TABS" 'wait 600' "tap $((W - 42)) $((TOP + 72))" 'wait 400' "tap 150 $((TOP + 72))" 'wait 400' 'text 48k' 'wait 700' \
   "click 150 $((TOP + 50 + 4 + 44 + 8 + 32 + 8 + 31))" 'wait 2000' 'dump h2.json' \
   "tap $((W - 42)) $((TOP + 72))" 'wait 400' "tap 150 $((TOP + 72))" 'wait 400' 'text 꽃' 'wait 700' \
   "click 150 $((TOP + 50 + 4 + 44 + 8 + 32 + 8 + 31))" 'wait 2000' 'dump h3.json'
check h0.json 'not d["output_options"]["match_sample_rate"]' "output at the song's sample rate is off to begin with"
check h1.json 'd["output_options"]["match_sample_rate"]' "its switch turns it on"
check h2.json 'd["state"] == "Playing" and d["output_rate"] == 48000 and d["underruns"] == 0' "a 48 kHz song goes out at 48 kHz"
check h3.json 'd["state"] == "Playing" and d["output_rate"] == 44100 and d["underruns"] == 0' "and a 44.1 kHz song at 44.1 kHz again"

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

echo "== music put into the folder while the app runs"
# The folder is looked into every half minute while the app is in front.
cp "$MUSIC/tone_48k.flac" "$AFTER/Documents/dropped.flac"
i=0
while [ $i -lt 12 ]; do
  sleep 5
  ui 'dump f1.json'
  python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1]))["library_tracks"] == 21 else 1)' "$PROFILE/f1.json" && break
  i=$((i + 1))
done
check f1.json 'd["library_tracks"] == 21' "a file put into the Documents folder is found without being asked for"

echo "== a media server of the network"
# A stand-in for a DLNA server shows the test library from this computer (which the
# Simulator shares its network with). The app is given its address: nothing is asked
# of the real network.
python3 scripts/dev/fake-dlna.py "$MUSIC" > "$WORK/dlna.out" 2>&1 &
DLNA_PID=$!
i=0
until grep -q '^listening ' "$WORK/dlna.out" 2>/dev/null; do
  sleep 0.2; i=$((i + 1))
  [ $i -lt 50 ] || { echo "the stand-in media server did not start" >&2; exit 1; }
done
DLNA=$(sed -n 's/^listening //p' "$WORK/dlna.out")
entry() { # entry <dump-file> <title>: the place of that entry in the open folder
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["network"]["titles"].index(sys.argv[2]))' "$PROFILE/$1" "$2"
}
ui "net add $DLNA" 'wait 2500' 'dump n1.json'
check n1.json 'd["network"]["server"] == "OnEum test server" and d["network"]["entries"] == 7 and d["network"]["folders"] == 5' "a media server added by its address opens: 5 folders and 2 songs"
check n1.json 'd["network"]["saved"] == ["OnEum test server"]' "and is kept in the list of servers"
ui "net open $(entry n1.json 아이유)" 'wait 1200' 'net open 0' 'wait 1200' 'dump n3.json'
check n3.json 'd["network"]["depth"] == 2 and d["network"]["playable"] == 4' "the album in a folder of it has 4 songs that can be played"
ui 'net open 1' 'wait 3000' 'dump n4.json' 'wait 1500' 'dump n5.json'
check n4.json 'd["state"] == "Playing" and d["current"] == "02 꽃" and d["queue_len"] == 4 and d["queue_index"] == 1' "a song of the server plays, with its folder as the queue"
check n4.json 'd["current_path"].startswith("http://127.0.0.1:")' "read from the server's address"
check n5.json 'd["position_ms"] > 3500 and d["underruns"] == 0 and d["library_tracks"] == 21' "the music goes on, and the library is as it was"
# What was added is a setting, and settings are written to their file. (On Android none
# were, once: the app there is told no window size, and saving hung on having one.)
start
ui 'wait 800' 'dump n6.json'
check n6.json 'd["network"]["saved"] == ["OnEum test server"] and d["current"] == "02 꽃"' "after a restart the server is still listed, and its song is the one that was on"
kill "$DLNA_PID" 2>/dev/null || true
DLNA_PID=

echo "== copying the record"
# What "Copy the Record" under Settings → About does: on a phone the app puts the text
# on the system's pasteboard itself (the UI library has none there). Read back from the
# simulator's own pasteboard. Left out while the Simulator app is open: that shares the
# simulator's pasteboard with the Mac's, and the test would replace what the user copied.
if pgrep -x Simulator >/dev/null; then
  echo "  skip the Simulator app is open (its pasteboard is the Mac's)"
else
  ui 'copy report' 'wait 800' 'dump k1.json'
  xcrun simctl pbpaste "$DEVICE" > "$WORK/pasted.txt"
  for part in '^--- the app ---$' '^OnEum [0-9.]* (ios)$' '^--- the log ---$' '^--- memory now ---$'; do
    if grep -q "$part" "$WORK/pasted.txt"; then
      echo "  ok   the record on the pasteboard has: $part"
    else
      echo "  FAIL the record on the pasteboard has: $part"; head -5 "$WORK/pasted.txt"; exit 1
    fi
  done
fi

echo "SMOKE TEST PASSED (iPhone Simulator)"
