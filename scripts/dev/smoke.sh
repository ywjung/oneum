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
ui 'resize 1180 760' 'wait 400' 'volume 0' 'dump a.json'
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
