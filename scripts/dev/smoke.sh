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
DLNA_PID=
trap 'pkill -f "dist/dev/OnEum.app/Contents/MacOS/OnEum" 2>/dev/null || true; [ -z "$DLNA_PID" ] || kill "$DLNA_PID" 2>/dev/null || true; rm -rf "$WORK"' EXIT

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
    print("       audio:", {k: d.get(k) for k in ("volume", "meter", "spectrum_peak_hz", "output_rate", "replaygain_mode", "peak_limited_frames", "underruns")})
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
ui 'click 200 20' 'text artist:beatles' 'wait 400' 'dump c5.json' 'key Escape' 'wait 300' \
   'click 200 20' 'text year:2019-2020' 'wait 400' 'dump c6.json' 'key Escape' 'wait 300' \
   'click 200 20' 'text 장르:가요 format:mp3' 'wait 400' 'dump c7.json' 'key Escape' 'wait 300'
check c5.json 'd["rows"] == 6' "field search: artist:"
check c6.json 'd["rows"] == 4' "field search: a range of years"
check c7.json 'd["rows"] == 2' "field search: genre in Korean and format together"

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

echo "== skip within a song, stop after the song"
# Controls ▸ Skip Ahead/Back, then "Stop After Current Song": taken back by another
# song, and carried out on the second song (26 s long, skipped ahead to its last seconds).
ui 'menu seek_forward' 'wait 800' 'dump sk1.json' 'menu seek_back' 'wait 800' 'dump sk2.json' \
   'menu stop_after_current' 'wait 300' 'dump sk3.json' "click $NEXT" 'wait 900' 'dump sk4.json' \
   'menu stop_after_current' 'wait 200' 'menu seek_forward' 'wait 300' 'menu seek_forward' 'wait 300' \
   'menu seek_forward' 'wait 300' 'menu seek_forward' 'wait 400' 'dump sk5.json' 'wait 6500' 'dump sk6.json' \
   'key Space' 'wait 800' 'dump sk7.json'
check sk1.json 'd["position_ms"] >= 5000' "skip ahead moves 5 s on"
check sk2.json 'd["position_ms"] < 4000' "skip back moves 5 s back"
check sk3.json 'd["stop_after_current"] and d["state"] == "Playing"' "stop after the current song is marked"
check sk4.json 'd["queue_index"] == 1 and not d["stop_after_current"]' "…and another song takes the mark away"
check sk5.json 'd["stop_after_current"] and d["position_ms"] >= 20000' "marked again, near the end of the song"
check sk6.json 'd["state"] == "Paused" and d["queue_index"] == 2 and d["position_ms"] == 0 and not d["stop_after_current"]' "playback stops at the end of that song with the next one ready"
check sk7.json 'd["state"] == "Playing" and d["queue_index"] == 2' "Space goes on from there"
ui 'menu sleep_15' 'wait 400' 'dump sl1.json' 'menu sleep_off' 'wait 300' 'dump sl2.json'
check sl1.json 'd["sleep_in_s"] is not None and 890 <= d["sleep_in_s"] <= 900 and d["state"] == "Playing"' "the sleep timer counts down from 15 minutes"
check sl2.json 'd["sleep_in_s"] is None and d["state"] == "Playing"' "…and can be switched off"

echo "== the new shortcuts, as key presses handed to the menu bar"
# Key codes: 124/123 = right/left arrow, 47 = period. The last press (Command-period,
# without Shift) is a control: no menu item may take it.
ui 'menukey 124 cmd alt' 'wait 800' 'dump mk1.json' 'menukey 123 cmd alt' 'wait 800' 'dump mk2.json' \
   'menukey 47 cmd shift' 'wait 300' 'dump mk3.json' 'menukey 47 cmd shift' 'wait 300' 'menukey 47 cmd' 'wait 200' 'dump mk4.json'
check mk1.json 'd["position_ms"] >= 5000' "Option-Command-Right skips ahead"
check mk2.json 'd["position_ms"] < 4500' "Option-Command-Left skips back"
check mk3.json 'd["stop_after_current"]' "Shift-Command-period marks the song as the last one"
check mk4.json 'not d["stop_after_current"] and d["menu_keys"] == [True, True, True, True, False]' "…and takes the mark back; Command-period alone is nobody's shortcut"

echo "== a section of the song, repeated"
# Controls ▸ Mark Section to Repeat (A–B): the first time marks where the section
# begins, the second where it ends. Left alone for more than twice its length, the
# song still stands inside it. The third time gives it up.
ui 'menu repeat_section' 'wait 1500' 'dump ab1.json' 'menu repeat_section' 'wait 300' 'dump ab2.json' \
   'wait 3500' 'dump ab3.json' 'menu repeat_section' 'wait 300' 'dump ab4.json'
check ab1.json 'd["section"] is not None and d["section"][1] is None and d["state"] == "Playing"' "the beginning of a section is marked"
check ab2.json 'd["section"][1] is not None and 1000 <= d["section"][1] - d["section"][0] <= 2500' "…and its end, about a second and a half on"
check ab3.json 'd["state"] == "Playing" and d["section"][0] - 300 <= d["position_ms"] <= d["section"][1] + 500' "left alone, the song plays inside the section, over and over"
check ab4.json 'd["section"] is None and d["state"] == "Playing"' "the third time gives the section up"

echo "== stars"
# A rating for the song that is playing, kept in the app's database only: the song is
# found by it, and the rating is taken away again.
ui 'menu rate_current 4' 'wait 500' 'dump rt1.json' 'click 200 20' 'text rating:>=4' 'wait 400' 'dump rt2.json' 'key Escape' 'wait 300' \
   'menu rate_current 0' 'wait 500' 'dump rt3.json'
check rt1.json 'd["current_rating"] == 4' "the song that is playing is given four stars"
check rt2.json 'd["rows"] == 1 and d["first_rows"] == [d["current"]]' "…and is found by them (rating:>=4)"
check rt3.json 'd["current_rating"] == 0 and d["rows"] == 20 and d["query"] == ""' "the stars are taken away again"

echo "== modes"
ui "click $SHUFFLE" 'wait 400' 'dump m1.json' "click $SHUFFLE" 'wait 400' 'dump m2.json' "click $REPEAT" 'wait 300' 'dump m3.json' "click $REPEAT" 'wait 300' 'dump m4.json' "click $REPEAT" 'wait 300' 'dump m5.json'
check m1.json 'd["shuffle"] and d["queue_index"] == 0' "shuffle on keeps the current track first"
check m2.json 'not d["shuffle"] and d["queue"][:3] == d["first_rows"][:3]' "shuffle off restores the order"
check m3.json 'd["repeat"] == "All"' "repeat all"
check m4.json 'd["repeat"] == "One"' "repeat one"
check m5.json 'd["repeat"] == "Off"' "repeat off"

echo "== views"
ui 'click 60 129' 'wait 500' 'dump v1.json' 'click 60 159' 'wait 500' 'dump v2.json' \
   'click 60 189' 'wait 500' 'dump v3.json' 'click 400 131' 'wait 500' 'dump v4.json' 'click 60 99' 'wait 300'
check v1.json 'd["nav"] == "Albums" and d["rows"] == 7' "albums view"
check v2.json 'd["nav"] == "Artists" and d["rows"] >= 5' "artists view"
check v3.json 'd["nav"] == "Genres" and d["first_rows"] == ["Electronic", "Pop", "Rock", "가요"]' "genres view lists the genres of the tags"
check v4.json 'd["nav"].startswith("Genre(") and d["rows"] == 4' "a genre opens its songs"

echo "== favourites and queue panel"
ui 'click 228 157' 'wait 300' 'click 60 219' 'wait 400' 'dump f1.json' 'click 60 99' 'wait 300' 'click 1118 20' 'wait 400' 'dump f2.json' 'click 1118 20' 'wait 300'
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
   'dump dd0.json' 'drag 400 125 100 399' 'wait 700' 'dump dd1.json' 'drag 400 157 1118 20' 'wait 600' 'dump dd2.json' \
   'click 100 399' 'wait 500' 'dump dd3.json' 'drag 400 132 400 175' 'wait 700' 'dump dd4.json' \
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
# On the Playback tab only the memory playback switch is flipped: the other two output
# options reconfigure the real output device. Volume leveling is below the fold.
ui 'click 1081 20' 'wait 600' 'dump se1.json' 'click 510 176' 'wait 500' 'click 846 535' 'wait 400' 'dump se2a.json' \
   'click 846 535' 'wait 400' 'dump se2b.json' 'scroll 590 400 -264' 'wait 500' 'click 758 419' 'wait 500' 'dump se2.json' \
   'click 703 419' 'wait 400' 'click 332 176' 'wait 500' 'click 845 448' 'wait 500' 'dump se3.json' 'click 845 448' 'wait 400' \
   'click 590 176' 'wait 400' 'dump se4.json' 'key Escape' 'wait 400' 'dump se5.json' \
   'menu user_guide' 'wait 500' 'dump se7.json' 'key Escape' 'wait 400' 'dump se8.json' \
   'click 1081 20' 'wait 500' 'click 100 600' 'wait 400' 'dump se6.json'
check se1.json 'd["settings_open"]' "the gear opens Settings"
check se2a.json 'd["settings_tab"] == "Playback" and d["output_options"] == {"match_sample_rate": False, "exclusive": False, "memory_playback": True}' "Playback tab: the memory playback switch"
check se2b.json 'not d["output_options"]["memory_playback"]' "…and off again"
check se2.json 'd["settings_tab"] == "Playback" and d["replaygain"].startswith("Track")' "Playback tab: volume leveling mode"
check se3.json 'd["settings_tab"] == "General" and d["show_format_column"]' "General tab: the Format column switch"
check se4.json 'd["settings_tab"] == "Shortcuts"' "Shortcuts tab"
check se5.json 'not d["settings_open"]' "Escape closes Settings"
check se7.json 'd["guide_open"] and not d["settings_open"]' "Help ▸ User Guide opens the guide inside the app"
check se8.json 'not d["guide_open"] and d["nav"] == "Songs"' "Escape closes the guide"
check se6.json 'not d["settings_open"] and d["nav"] == "Songs"' "a click outside closes it without acting on what is behind"

echo "== listen again"
# The four lists behind "Listen Again": what was played, what came in, what is played
# most, what has not been heard for long (nothing yet in a library this new).
ui 'click 60 249' 'wait 500' 'dump la0.json' 'click 328 105' 'wait 500' 'dump la1.json' \
   'click 503 105' 'wait 500' 'dump la2.json' 'click 255 105' 'wait 400' 'dump la3.json' 'click 60 99' 'wait 400' 'dump la4.json'
check la0.json 'd["nav"] == "Recent" and d["rediscover"] == "Played" and d["rows"] > 0' "Listen Again opens on what was played"
check la1.json 'd["rediscover"] == "Added" and d["rows"] == 20' "…Recently Added lists the whole new library"
check la2.json 'd["rediscover"] == "Forgotten" and d["rows"] == 0' "…Not Heard Lately is empty in a library this new"
check la3.json 'd["rediscover"] == "Played" and d["rows"] > 0' "…and back"
check la4.json 'd["nav"] == "Songs" and d["rows"] == 20' "back to all songs"

echo "== equalizer, crossfeed, radio"
# The Playback tab: scrolled to its end for the radio switch, and 263 points back up from
# there for the equalizer row and the crossfeed strengths (the correction card lies between:
# when the explanation under it gains or loses a line, 13 points, this number moves with it).
# Then the equalizer sheet: preset list, on/off switch, reset.
ui 'click 1081 20' 'wait 600' 'click 510 176' 'wait 500' 'scroll 590 400 -2000' 'wait 500' 'scroll 590 400 263' 'wait 500' \
   'click 779 413' 'wait 400' 'dump sh1.json' 'click 671 413' 'wait 300' 'scroll 590 400 -2000' 'wait 500' 'click 846 526' 'wait 400' 'dump sh2.json' \
   'click 846 526' 'wait 400' 'dump sh3.json' 'scroll 590 400 263' 'wait 500' 'click 843 301' 'wait 600' 'dump sh4.json' \
   'click 435 183' 'wait 500' 'click 342 435' 'wait 500' 'dump sh5.json' 'click 855 133' 'wait 400' 'dump sh6.json' \
   'click 883 183' 'wait 400' 'dump sh7.json' 'key Escape' 'wait 400' 'dump sh8.json'
check sh1.json 'd["crossfeed"] == "Medium"' "Playback tab: headphone crossfeed strength"
check sh2.json 'd["crossfeed"] == "Off" and d["radio"]' "…off again; the radio switch"
check sh3.json 'not d["radio"]' "…and off again"
check sh4.json 'd["eq_open"] and not d["settings_open"]' "the equalizer row opens the equalizer"
check sh5.json 'd["eq"]["preset"] == "rock" and d["eq"]["active"] and d["eq"]["player_gains_db"][0] == 4.5' "choosing a preset sets the bands and switches the equalizer on"
check sh6.json 'not d["eq"]["active"] and d["eq"]["preset"] == "rock"' "the switch turns it off and keeps the values"
check sh7.json 'd["eq"]["preset"] == "flat" and not d["eq"]["active"]' "Reset returns every band to 0 dB"
check sh8.json 'not d["eq_open"] and d["nav"] == "Songs"' "Escape closes the equalizer"

echo "== list columns, a search kept as a smart playlist"
# The column menu opens on a right-click on a column title; it stays open until Escape.
# The + in the search field appears while All Songs is being searched.
ui 'rclick 256 62' 'wait 400' 'click 292 100' 'wait 300' 'click 292 160' 'wait 300' 'key Escape' 'wait 300' 'dump co1.json' \
   'rclick 256 62' 'wait 400' 'click 292 100' 'wait 300' 'click 292 160' 'wait 300' 'key Escape' 'wait 300' 'dump co2.json' \
   'click 200 20' 'wait 200' 'click 200 20' 'text year:2019-2020' 'wait 400' 'click 365 24' 'wait 500' \
   'key a cmd' 'text Late' 'key Enter' 'wait 600' 'dump sm1.json' \
   'click 60 99' 'wait 300' 'click 200 20' 'wait 100' 'key Escape' 'wait 300' 'dump sm2.json'
check co1.json 'd["columns"]["year"] and d["columns"]["plays"] and not d["columns"]["genre"]' "the column menu switches the year and play count columns on"
check co2.json 'not d["columns"]["year"] and not d["columns"]["plays"]' "…and off again"
check sm1.json 'd["nav"] == "Smart(\"Late\")" and d["rows"] == 4 and d["smart_playlists"] == [["Late", "year:2019-2020"]]' "a search is kept as a smart playlist and opens with its songs"
check sm2.json 'd["nav"] == "Songs" and d["rows"] == 20 and d["query"] == ""' "back to all songs"

echo "== headphone correction"
# A correction file as AutoEq writes them, and one with a filter type that is not
# implemented: the first is imported and applied, the second is refused as a whole.
printf 'Preamp: -6.2 dB\nFilter 1: ON LSC Fc 105 Hz Gain 5.5 dB Q 0.70\nFilter 2: ON PK Fc 2900 Hz Gain 4.1 dB Q 2.10\n' > "$WORK/Test Phones ParametricEQ.txt"
printf 'Filter 1: ON LP Fc 100 Hz\n' > "$WORK/unsupported.txt"
ui "menu correction_import $WORK/unsupported.txt" 'wait 300' 'dump hc0.json' \
   "menu correction_import $WORK/Test Phones ParametricEQ.txt" 'wait 500' 'dump hc1.json' \
   'menu correction_off' 'wait 400' 'dump hc2.json' \
   'menu correction_select Sennheiser HD 800 S' 'wait 400' 'dump hc3.json' \
   'menu correction_select No Such Headphones' 'wait 300' 'dump hc4.json' \
   'menu correction_off' 'wait 300' 'dump hc5.json'
check hc0.json 'd["correction"] == {"selected": None, "applied": None, "profiles": [], "builtin": 35}' "a file with a filter that is not supported is not imported"
check hc1.json 'd["correction"] == {"selected": "Test Phones", "applied": "Test Phones", "profiles": [["Test Phones", 2]], "builtin": 35}' "a correction file is imported, named after the file, and applied"
check hc2.json 'd["correction"]["applied"] is None and d["correction"]["profiles"] == [["Test Phones", 2]]' "…and can be switched off, the profile kept"
check hc3.json 'd["correction"]["selected"] == "Sennheiser HD 800 S" and d["correction"]["applied"] == "Sennheiser HD 800 S" and d["correction"]["profiles"] == [["Test Phones", 2]]' "a built-in headphone profile is applied without being copied into the settings"
check hc4.json 'd["correction"]["applied"] == "Sennheiser HD 800 S"' "a name that is no profile changes nothing"
check hc5.json 'd["correction"]["applied"] is None' "…and the built-in profile can be switched off"

echo "== loudness of untagged songs"
# 18 of the 20 songs have no ReplayGain tags. Measured values are forgotten again right
# away: the VU check below relies on the tone being played as it is.
ui 'menu loudness_start' 'wait 300'
i=0
while :; do
  ui 'wait 500' 'dump lo1.json'
  python3 -c "import json,sys; sys.exit(1 if json.load(open(sys.argv[1]))['loudness']['running'] else 0)" "$PROFILE/lo1.json" && break
  i=$((i + 1)); [ $i -lt 240 ] || break
done
ui 'menu loudness_forget' 'wait 500' 'dump lo2.json'
check lo1.json 'd["loudness"] == {"running": False, "total": 18, "done": 18, "failed": 0, "measured": 18, "waiting": 0}' "the songs without leveling tags are measured"
check lo2.json 'd["loudness"]["measured"] == 0 and d["loudness"]["waiting"] == 18' "…and what was measured can be forgotten"

echo "== a media server of the network"
# A stand-in for a DLNA server shows the test library from this computer. The app is
# given its address: nothing is asked of the real network.
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
ui 'dump n0.json' "net add $DLNA" 'wait 2000' 'dump n1.json'
SONGS=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["library_tracks"])' "$PROFILE/n0.json")
check n1.json 'd["network"]["server"] == "OnEum test server" and d["network"]["entries"] == 7 and d["network"]["folders"] == 5' "a media server added by its address opens: 5 folders and 2 songs"
check n1.json 'd["network"]["saved"] == ["OnEum test server"]' "and is kept in the list of servers"
ui "net open $(entry n1.json 아이유)" 'wait 1200' 'dump n2.json' 'net open 0' 'wait 1200' 'dump n3.json'
check n2.json 'd["network"]["depth"] == 1 and d["network"]["titles"] == ["꽃갈피"]' "a folder of the server opens"
check n3.json 'd["network"]["depth"] == 2 and d["network"]["playable"] == 4' "the album in it has 4 songs that can be played"
ui 'net open 1' 'wait 3000' 'dump n4.json' 'wait 1500' 'dump n5.json' 'menu seek_forward' 'wait 600' 'dump n6.json'
check n4.json 'd["state"] == "Playing" and d["current"] == "02 꽃" and d["queue_len"] == 4 and d["queue_index"] == 1' "a song of the server plays, with its folder as the queue"
check n4.json 'd["current_path"].startswith("http://127.0.0.1:")' "read from the server's address"
check n5.json 'd["position_ms"] > 3500 and d["underruns"] == 0' "the music goes on without a break in the sound"
check n6.json 'd["position_ms"] > 8500 and d["state"] == "Playing"' "skipping ahead works in it"
check n6.json "d[\"library_tracks\"] == $SONGS" "the server's songs are not put into the library"
ui 'net up' 'wait 500' 'net up' 'wait 500' 'net up' 'wait 300' 'net forget 0' 'wait 300' 'dump n7.json'
check n7.json 'd["network"]["server"] is None and d["network"]["saved"] == []' "back out of the server, and it can be taken off the list"
kill "$DLNA_PID" 2>/dev/null || true
DLNA_PID=

echo "== hidden window"
ui 'hide' 'wait 800' "open $MUSIC/tone_48k.flac" 'wait 1500' 'dump h1.json' 'activate' 'wait 700' 'dump h2.json' \
   'wait 700' 'dump h3.json'
check h1.json 'not d["window_visible"] and d["state"] == "Playing" and d["queue_len"] == 1' "a file opened while the window is hidden plays"
check h2.json 'd["window_visible"]' "the window comes back"
# macOS does not report a window as visible again while the screen is locked, and a window
# that is not visible is not painted: everything after this point needs an unlocked screen.
python3 - "$PROFILE/h2.json" "$PROFILE/h3.json" <<'PY'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:3])
ok = b["perf"]["frames"] > a["perf"]["frames"]
print(("  ok   " if ok else "  FAIL ") + "…and is painted again")
if not ok:
    print("       no frame was drawn after the window came back: is the screen locked or asleep?")
    sys.exit(1)
PY

echo "== VU meter"
# tone_48k.flac is a sine peaking at -15 dBFS (ffmpeg's mono-to-stereo up-mix takes 3 dB off
# the -12 dBFS tone), i.e. -7 VU. The app volume is 0 all along: the meter shows the music,
# not the volume.
ui 'menu toggle_meter' 'wait 1500' 'dump m1.json' 'key Space' 'wait 1500' 'dump m2.json' \
   'key Space' 'wait 500' 'menu toggle_meter' 'wait 300' 'dump m3.json'
check m1.json 'd["meter_visible"] and d["volume"] == 0 and all(v is not None and abs(v + 7.0) < 0.5 for v in d["meter"]["vu"])' "the VU meter reads -7 VU for a -15 dBFS tone, at volume 0"
check m1.json 'd["spectrum_peak_hz"] is not None and 0.8 < d["spectrum_peak_hz"] / 880.0 < 1.25' "the spectrum display peaks at the tone's 880 Hz"
check m2.json 'd["state"] == "Paused" and max(d["meter"]["position"]) < 0.01' "paused: the needles fall back to rest"
check m3.json 'not d["meter_visible"] and d["state"] == "Playing"' "the meter deck closes again"

echo "== file check, pins, folders, statistics"
ui 'menu check_start' 'wait 2000' 'dump fc1.json'
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  python3 -c 'import json,sys; sys.exit(0 if not json.load(open(sys.argv[1]))["check"]["running"] else 1)' "$PROFILE/fc1.json" && break
  ui 'wait 2000' 'dump fc1.json'
done
ui 'menu check_show_damaged' 'wait 500' 'dump fc2.json' 'key Escape' 'wait 300'
check fc1.json 'not d["check"]["running"] and d["check"]["checked"] == 20 and d["check"]["with_dr"] >= 15 and d["check"]["verified"] >= 6' "the file check reads every file (FLAC MD5, DR)"
check fc2.json 'd["query"] == "check:damaged" and d["rows"] == d["check"]["damaged"]' "…and lists the damaged ones"
ui 'menu go_first_folder' 'wait 500' 'dump fo1.json' 'menu go_stats' 'wait 1500' 'dump st1.json' \
   'menu pin_first_album' 'wait 300' 'dump pn1.json'
check fo1.json 'd["nav"].startswith("Folder") and d["rows"] == 20 and d["subfolders"] == 5' "the folder view: every song under the music folder, its five folders as ways in"
check st1.json 'd["nav"] == "Stats" and d["stats"] is not None and d["stats"][0] >= 1 and d["stats"][2] == 20' "statistics: the plays of this run and the whole library"
check pn1.json 'd["pinned"] == 1' "an album is pinned to the sidebar"

echo "== library data, to a file and back"
# The library's own data (plays, favourites, playlists, saved searches) is written to a
# file that names songs from their music folder on, not by a place on this Mac. A file
# that knows more (as one from another device does) is merged in; twice changes nothing.
ui "menu data_export $WORK/data.json" 'wait 1000' 'dump ld1.json'
if python3 - "$WORK/data.json" "$WORK" "$WORK/data-more.json" <<'PY'
import json, sys
text = open(sys.argv[1]).read()
d = json.loads(text)
assert d["format"] == "oneum-library-data" and d["version"] == 1
assert len(d["songs"]) == 20, len(d["songs"])
assert all(song["file"].startswith("music/") for song in d["songs"]), d["songs"][0]
assert sys.argv[2] not in text, "the file tells where the music lies"
assert sum(song.get("plays", 0) for song in d["songs"]) >= 1 and d["plays"], "no plays"
assert [p["name"] for p in d["playlists"]] == ["드라이브"], d["playlists"]
assert d["searches"] == [{"name": "Late", "query": "year:2019-2020"}], d["searches"]
# What another device might know besides.
d["playlists"] += [{"name": "From Elsewhere", "songs": [0, 1, 2]}, {"name": "드라이브", "songs": [3]}]
d["searches"].append({"name": "Loud", "query": "dr:<8"})
json.dump(d, open(sys.argv[3], "w"))
PY
then echo "  ok   the file holds the 20 songs by folder and name, the plays, the playlist and the saved search"
else echo "  FAIL the library data file is not as expected"; exit 1
fi
ui "menu data_import $WORK/data-more.json" 'wait 1200' 'dump ld2.json' \
   "menu data_import $WORK/data-more.json" 'wait 1200' 'dump ld3.json' \
   "menu data_import $WORK/unsupported.txt" 'wait 600' 'dump ld4.json'
check ld1.json '"20" in (d["toast"] or "")' "the export says how much it wrote"
check ld2.json 'sorted(p[0] for p in d["playlists"]) == sorted(["드라이브", "드라이브 (2)", "From Elsewhere"])' "a file from elsewhere is merged in: its new playlist, and one whose name was taken, marked"
check ld2.json 'd["smart_playlists"] == [["Late", "year:2019-2020"], ["Loud", "dr:<8"]]' "…and its saved search"
check ld3.json 'len(d["playlists"]) == 3 and len(d["smart_playlists"]) == 2' "taking the same file in again changes nothing more"
check ld4.json 'len(d["playlists"]) == 3 and d["toast"] is not None and d["library_tracks"] == 20' "a file that is not library data is refused, and said so"

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
