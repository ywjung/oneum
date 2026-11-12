#!/bin/sh
# End-to-end smoke test of the packaged app: launches it against a fresh throw-away
# profile and a synthetic library, drives the real UI through the verification channel
# (src/ui/automation.rs) and checks the state it reports. Silent: app volume 0, no media
# session. Needs ffmpeg for the test library.
#
#   scripts/dev/smoke.sh            # debug build
#   scripts/dev/smoke.sh --release
set -eu
cd "$(dirname "$0")/../.."
MODE=${1:-}
WORK=$(mktemp -d /tmp/oneum-smoke.XXXXXX)
MUSIC="$WORK/music"
PROFILE="$WORK/profile"
trap 'pkill -f "dist/dev/OnEum.app/Contents/MacOS/OnEum" 2>/dev/null || true; rm -rf "$WORK"' EXIT

echo "== generating test library"
scripts/dev/gen-test-library.sh "$MUSIC" >/dev/null
echo "== launching"
# shellcheck disable=SC2086
scripts/dev/run.sh "$PROFILE" $MODE >/dev/null
ui() { scripts/dev/ui.sh "$PROFILE" "$@"; }
check() { # check <dump-file> <python expression over d> <description>
  python3 - "$PROFILE/$1" "$2" "$3" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
ok = bool(eval(sys.argv[2], {"d": d}))
print(("  ok   " if ok else "  FAIL ") + sys.argv[3])
if not ok:
    print("       state:", {k: d[k] for k in ("nav", "rows", "query", "state", "current", "position_ms", "queue_len", "queue_index", "shuffle", "repeat", "library_tracks", "toast")})
    sys.exit(1)
PY
}

# Transport button centres for a 1180x760 window without a side panel.
SHUFFLE="508 710"; PREV="547 710"; PLAY="590 710"; NEXT="633 710"; REPEAT="672 710"

echo "== first run"
# 'front' keeps the window above others without taking keyboard focus: a fully covered
# window is not painted and would not process the scripted input.
ui 'front' 'resize 1180 760' 'wait 400' 'volume 0' 'dump a.json'
check a.json 'd["library_tracks"] == 0 and d["state"] == "Idle"' "starts with an empty library, idle"

echo "== add a folder (Finder open path)"
ui "open $MUSIC" 'wait 2500' 'dump b.json'
check b.json 'd["library_tracks"] == 20 and not d["scan_running"]' "20 tracks scanned (1 unreadable file skipped)"
check b.json 'd["albums"] == 7' "albums grouped (mixed-tag album merged, same-title albums kept apart)"

echo "== search"
ui 'click 200 20' 'wait 200' 'click 200 20' 'text 이유' 'wait 400' 'dump c1.json' 'key Escape' 'wait 300' 'dump c2.json' \
   'click 200 20' 'text ㅇㅇㅇ' 'wait 400' 'dump c3.json' 'key Escape' 'wait 300' \
   'click 200 20' 'text beatles together' 'wait 400' 'dump c4.json' 'key Escape' 'wait 300'
check c1.json 'd["rows"] == 4 and d["text_focus"]' "Korean partial search finds 4 songs"
check c2.json 'd["rows"] == 20 and d["query"] == "" and not d["text_focus"]' "Escape clears the search"
check c3.json 'd["rows"] == 4' "choseong search"
check c4.json 'd["rows"] == 1 and d["first_rows"] == ["Come Together"]' "multi-word search"

echo "== playback"
ui 'dclick 400 93' 'wait 1500' 'dump p1.json'
check p1.json 'd["state"] == "Playing" and d["current"] == d["first_rows"][0] and d["queue_len"] == 20 and d["queue_index"] == 0' "double-click plays and queues the list"
check p1.json 'd["underruns"] == 0 and d["output_device"]' "audio reaches an output device without underruns"
ui 'key Space' 'wait 300' 'dump p2.json' 'key Space' 'wait 500' 'dump p3.json'
check p2.json 'd["state"] == "Paused"' "Space pauses"
check p3.json 'd["state"] == "Playing"' "Space resumes"
ui "click $NEXT" 'wait 900' 'dump p4.json'
check p4.json 'd["queue_index"] == 1 and d["state"] == "Playing"' "next button"
ui 'click 590 743' 'wait 900' 'dump p5.json'
check p5.json 'd["position_ms"] > 0.3 * d["duration_ms"]' "click on the seek bar seeks"
ui "click $PREV" 'wait 700' 'dump p6.json'
check p6.json 'd["queue_index"] == 1 and d["position_ms"] < 3000' "previous after 3 s restarts the track"
ui "click $PREV" 'wait 700' 'dump p7.json'
check p7.json 'd["queue_index"] == 0' "previous within 3 s goes to the previous track"

echo "== modes"
ui "click $SHUFFLE" 'wait 400' 'dump m1.json' "click $SHUFFLE" 'wait 400' 'dump m2.json' "click $REPEAT" 'wait 300' 'dump m3.json' "click $REPEAT" 'wait 300' 'dump m4.json' "click $REPEAT" 'wait 300' 'dump m5.json'
check m1.json 'd["shuffle"] and d["queue_index"] == 0' "shuffle on keeps the current track first"
check m2.json 'not d["shuffle"] and d["queue"][:3] == d["first_rows"][:3]' "shuffle off restores the order"
check m3.json 'd["repeat"] == "All"' "repeat all"
check m4.json 'd["repeat"] == "One"' "repeat one"
check m5.json 'd["repeat"] == "Off"' "repeat off"

echo "== views"
ui 'click 60 129' 'wait 500' 'dump v1.json' 'click 60 159' 'wait 500' 'dump v2.json' 'click 60 99' 'wait 300'
check v1.json 'd["nav"] == "Albums" and d["rows"] == 7' "albums view"
check v2.json 'd["nav"] == "Artists" and d["rows"] >= 5' "artists view"

echo "== favourites and queue panel"
ui 'click 228 157' 'wait 300' 'click 60 189' 'wait 400' 'dump f1.json' 'click 60 99' 'wait 300' 'click 1118 20' 'wait 400' 'dump f2.json' 'click 1118 20' 'wait 300'
check f1.json 'd["nav"] == "Favorites" and d["rows"] == 1' "heart marks a favourite"
check f2.json 'd["right_panel"] == "Queue"' "queue panel opens"

same() { # same <dump-a> <dump-b> <keys,comma-separated> <description>
  python3 - "$PROFILE/$1" "$PROFILE/$2" "$3" "$4" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
keys = sys.argv[3].split(",")
ok = all(a[k] == b[k] for k in keys)
print(("  ok   " if ok else "  FAIL ") + sys.argv[4])
if not ok:
    print("       before:", {k: a[k] for k in keys}); print("       after: ", {k: b[k] for k in keys})
    sys.exit(1)
PY
}
delta() { # delta <dump-a> <dump-b> <key> <expected difference> <description>
  python3 - "$PROFILE/$1" "$PROFILE/$2" "$3" "$4" "$5" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
ok = b[sys.argv[3]] - a[sys.argv[3]] == int(sys.argv[4])
print(("  ok   " if ok else "  FAIL ") + sys.argv[5])
if not ok:
    print("       ", sys.argv[3], a[sys.argv[3]], "->", b[sys.argv[3]])
    sys.exit(1)
PY
}

echo "== keyboard in the song list"
ui 'click 400 93' 'wait 200' 'key ArrowDown' 'key ArrowDown shift' 'key ArrowDown shift' 'wait 200' 'dump k1.json' 'key ArrowUp shift' 'wait 200' 'dump k2.json' \
   'click 200 20' 'wait 200' 'text 이유' 'wait 300' 'click 400 93' 'wait 300' 'dump k3.json' 'click 200 20' 'wait 200' 'key Enter' 'wait 700' 'dump k4.json' \
   'key Escape' 'wait 200' 'click 200 20' 'wait 100' 'key Escape' 'wait 300'
check k1.json 'd["selected"] == 3' "Shift+Down extends the selection"
check k2.json 'd["selected"] == 2' "Shift+Up shrinks it again"
same k3.json k4.json current,queue_len,queue_index "Return in the search field does not start the selected song"

echo "== song menu and queue keys"
ui 'click 400 93' 'wait 400' 'dump s0.json' 'menu song_play_next' 'wait 500' 'dump s1.json' 'menu toggle_queue' 'wait 500' 'click 1000 140' 'wait 200' 'key ArrowDown' 'wait 200' 'dump s2.json' \
   'key Delete' 'wait 600' 'dump s3.json' 'menu toggle_queue' 'wait 300'
delta s0.json s1.json queue_len 1 "Song ▸ Play Next queues the selected song"
check s2.json 'd["key_pane"] == "Queue" and d["queue_selected"] == 1' "the queue takes the keys after a click in it"
delta s1.json s3.json queue_len -1 "Delete removes the selected queue item"
check s3.json 'd["rows"] == 20 and d["playlists"] == []' "…and nothing else"

echo "== album play button"
ui 'click 60 129' 'wait 500' 'move 300 150' 'wait 200' 'click 348 184' 'wait 1200' 'dump al.json' 'click 60 99' 'wait 400'
check al.json 'd["nav"] == "Albums" and d["state"] == "Playing" and d["queue_len"] == 2' "the play button on a cover plays that album"

echo "== playlist, remove and undo"
ui 'click 400 93' 'wait 400' 'click 400 221 shift' 'wait 200' 'rclick 400 150' 'wait 400' 'click 460 270' 'wait 500' 'click 678 270' 'wait 500' 'key Enter' 'wait 700' 'dump pl1.json' \
   'click 400 132' 'wait 400' 'key Delete' 'wait 500' 'dump pl2.json' 'click 1103 664' 'wait 600' 'dump pl3.json' 'click 60 99' 'wait 300'
check pl1.json 'd["nav"].startswith("Playlist") and d["rows"] == 5 and d["playlists"][0][1] == 5' "a playlist is created from the selection"
same al.json pl1.json current,queue_len "Return in the name dialog does not start playback"
check pl2.json 'd["rows"] == 4 and d["toast_action"]' "Delete removes a row and offers Undo"
check pl3.json 'd["rows"] == 5 and not d["toast_action"]' "Undo puts it back"
same pl1.json pl3.json first_rows "…in the original order"

echo "== mute"
# 0.01 on the volume slider is far below audibility (cubic curve), so this stays silent.
ui 'volume 0.01' 'wait 300' 'menu toggle_mute' 'wait 300' 'dump mu1.json' 'menu toggle_mute' 'wait 300' 'dump mu2.json' 'volume 0' 'wait 200'
check mu1.json 'd["volume"] == 0' "mute"
check mu2.json 'abs(d["volume"] - 0.01) < 1e-6' "un-mute returns to the previous level"

check2() { # check2 <dump-a> <dump-b> <python expression over a and b> <description>
  python3 - "$PROFILE/$1" "$PROFILE/$2" "$3" "$4" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
ok = bool(eval(sys.argv[3], {"a": a, "b": b}))
print(("  ok   " if ok else "  FAIL ") + sys.argv[4])
if not ok:
    keys = ("nav", "first_rows", "playlists", "queue_len", "selected_titles")
    print("       before:", {k: a[k] for k in keys}); print("       after: ", {k: b[k] for k in keys})
    sys.exit(1)
PY
}

echo "== type-ahead, remaining time, drag and drop"
ui 'click 60 99' 'wait 400' 'click 400 93' 'wait 300' 'text here' 'wait 300' 'dump ta.json' 'wait 1200' \
   'click 810 743' 'wait 400' 'dump rt1.json' 'click 810 743' 'wait 400' 'dump rt2.json' \
   'dump dd0.json' 'drag 400 125 100 281' 'wait 700' 'dump dd1.json' 'drag 400 157 1118 20' 'wait 600' 'dump dd2.json' \
   'click 100 281' 'wait 500' 'dump dd3.json' 'drag 400 132 400 175' 'wait 700' 'dump dd4.json' \
   'menu playlist_rename' 'wait 600' 'key A cmd' 'text 드라이브' 'wait 200' 'key Enter' 'wait 600' 'dump pm1.json' \
   'menu playlist_delete' 'wait 500' 'dump pm2.json' 'key Escape' 'wait 400' 'dump pm3.json' 'click 60 99' 'wait 300'
check ta.json 'd["selected_titles"] == ["Here Comes the Sun"]' "typing letters jumps to the song"
check rt1.json 'd["show_remaining_time"]' "clicking the time label shows the time left"
check rt2.json 'not d["show_remaining_time"]' "…and back"
check2 dd0.json dd1.json 'b["playlists"][0][1] == a["playlists"][0][1] + 1 and "toast" in b and b["toast"]' "a song dragged onto a playlist is added to it"
check2 dd1.json dd2.json 'b["queue_len"] == a["queue_len"] + 1' "a song dragged onto the queue button is queued"
check2 dd3.json dd4.json 'b["first_rows"][:2] == a["first_rows"][1::-1] and b["first_rows"][2:] == a["first_rows"][2:]' "dragging a row inside a playlist reorders it"
check pm1.json 'd["playlists"][0][0] == "드라이브"' "File ▸ Rename Playlist renames the open playlist"
check pm2.json 'd["dialog_open"]' "File ▸ Delete Playlist asks first"
check pm3.json 'not d["dialog_open"] and len(d["playlists"]) == 1' "…and Escape keeps the playlist"

echo "== settings"
# Tab and control centres for the 620-point settings window in a 1180x760 window (Korean).
ui 'click 1081 20' 'wait 600' 'dump se1.json' 'click 510 176' 'wait 500' 'click 758 425' 'wait 500' 'dump se2.json' \
   'click 712 425' 'wait 400' 'click 332 176' 'wait 500' 'click 845 448' 'wait 500' 'dump se3.json' 'click 845 448' 'wait 400' \
   'click 590 176' 'wait 400' 'dump se4.json' 'key Escape' 'wait 400' 'dump se5.json' \
   'click 1081 20' 'wait 500' 'click 100 600' 'wait 400' 'dump se6.json'
check se1.json 'd["settings_open"]' "the gear opens Settings"
check se2.json 'd["settings_tab"] == "Playback" and d["replaygain"].startswith("Track")' "Playback tab: volume leveling mode"
check se3.json 'd["settings_tab"] == "General" and d["show_format_column"]' "General tab: the Format column switch"
check se4.json 'd["settings_tab"] == "Shortcuts"' "Shortcuts tab"
check se5.json 'not d["settings_open"]' "Escape closes Settings"
check se6.json 'not d["settings_open"] and d["nav"] == "Songs"' "a click outside closes it without acting on what is behind"

echo "== hidden window"
ui 'hide' 'wait 800' "open $MUSIC/tone_48k.flac" 'wait 1500' 'dump h1.json' 'activate' 'wait 700' 'dump h2.json'
check h1.json 'not d["window_visible"] and d["state"] == "Playing" and d["queue_len"] == 1' "a file opened while the window is hidden plays"
check h2.json 'd["window_visible"]' "the window comes back"

echo "== quit and restore"
ui 'key Space' 'wait 300' 'dump q0.json' 'quit'
sleep 2
# shellcheck disable=SC2086
scripts/dev/run.sh "$PROFILE" $MODE >/dev/null
ui 'dump q1.json' 'quit'
python3 - "$PROFILE/q0.json" "$PROFILE/q1.json" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
ok = (b["state"] == "Paused" and b["current"] == a["current"] and b["queue_len"] == a["queue_len"]
      and b["queue_index"] == a["queue_index"] and abs(b["position_ms"] - a["position_ms"]) < 1500
      and b["volume"] == 0 and b["library_tracks"] == 20)
print(("  ok   " if ok else "  FAIL ") + "restart restores queue, track, position and volume, paused")
if not ok:
    print("       before:", a["state"], a["current"], a["position_ms"], a["queue_index"]); print("       after: ", b["state"], b["current"], b["position_ms"], b["queue_index"], b["volume"])
    sys.exit(1)
PY
echo "SMOKE TEST PASSED"
