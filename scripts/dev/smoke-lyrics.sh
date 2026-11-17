# Sourced by the smoke tests: the checks of lyrics from the internet, which are the same
# on every system. They use the caller's `ui` and `check` and its $WORK, and add the
# stand-in's process id to DAV_PIDS for the caller's cleanup.
#
#   lyrics_checks <host the app reaches this computer at> <host to listen on> <folder of the dumps>
#
# To be called while a song of the library plays and its lyrics are in view (Song Info
# on a computer, the lyrics view of the player on a phone); the song's file has no
# lyrics. A stand-in for the lyrics service runs here (scripts/dev/fake-lyrics.py) and
# the app is given its address: nothing is asked of the real service. (A verification
# run knows no lyrics service until it is given one.)
lyrics_checks() {
  ly_host=$1
  ly_listen=$2
  ly_dumps=$3
  rm -f "$WORK/lyrics.log"
  python3 scripts/dev/fake-lyrics.py --host "$ly_listen" --log "$WORK/lyrics.log" > "$WORK/lyrics.out" 2>&1 &
  ly_pid=$!
  DAV_PIDS="$DAV_PIDS $ly_pid"
  i=0
  until grep -q '^listening ' "$WORK/lyrics.out" 2>/dev/null; do
    sleep 0.2; i=$((i + 1))
    [ $i -lt 50 ] || { echo "the stand-in lyrics service did not start" >&2; exit 1; }
  done
  ly=$(sed -n "s/^listening \(http:\/\/\)[^:]*:/\1$ly_host:/p" "$WORK/lyrics.out")
  asked() { # asked <python expression over reqs (the requests so far) and d (a dump)> <dump-file> <description>
    python3 - "$WORK/lyrics.log" "$1" "$ly_dumps/$2" "$3" <<'PY'
import json, os, sys
reqs = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")] if os.path.exists(sys.argv[1]) else []
d = json.load(open(sys.argv[3]))
ok = bool(eval(sys.argv[2], {"reqs": reqs, "d": d}))
print(("  ok   " if ok else "  FAIL ") + sys.argv[4])
if not ok:
    print("       asked:", reqs)
    print("       lyrics:", d.get("lyrics"), "current:", d.get("current"))
    sys.exit(1)
PY
  }

  ui "menu lyrics_service $ly" 'wait 1500' 'dump ly1.json'
  check ly1.json 'not d["lyrics"]["online"] and d["lyrics"]["chars"] is None and not d["lyrics"]["found_online"]' "the song's file has no lyrics, and lyrics from the internet are off"
  asked 'reqs == []' ly1.json "while they are off, nothing is asked of the lyrics service"
  ui 'menu online_lyrics' 'wait 3000' 'dump ly2.json'
  check ly2.json 'd["lyrics"]["online"] and d["lyrics"]["found_online"] and d["lyrics"]["timed_lines"] == 6 and not d["lyrics"]["seeking"]' "switched on, the lyrics of the song in view are found: 6 lines that follow the song"
  asked 'len(reqs) >= 1 and reqs[0]["path"] == "/api/get" and sorted(reqs[0]["query"]) == ["album_name", "artist_name", "duration", "track_name"] and reqs[0]["query"]["track_name"] == d["current"] and reqs[0]["query"]["duration"].isdigit()' ly2.json "the service was asked for that song by its title, artist, album and length"
  asked 'reqs[0]["headers"] == ["accept-encoding", "connection", "host", "user-agent"]' ly2.json "and was told nothing else: no account, nothing of the device"
  ui 'menu online_lyrics' 'wait 1500' 'dump ly3.json' 'menu online_lyrics' 'wait 2500' 'dump ly4.json'
  check ly3.json 'not d["lyrics"]["online"] and d["lyrics"]["chars"] is None and not d["lyrics"]["found_online"]' "switched off, the lyrics that were found go"
  check ly4.json 'd["lyrics"]["found_online"] and d["lyrics"]["timed_lines"] == 6' "switched on again, they are back"
  asked 'len(reqs) == len({r["query"]["track_name"] for r in reqs})' ly4.json "from what was kept on the device: no song was asked for twice"
  ui 'menu online_lyrics' 'wait 600'
  kill "$ly_pid" 2>/dev/null || true
}
