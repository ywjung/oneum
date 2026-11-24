#!/bin/sh
# Makes the pictures of the Mac app for the Mac App Store, in Korean and in English: the
# albums, an album while it plays (the meters), the lyrics with the signal path, the
# equalizer, a search and the output settings. They are what the app's window shows,
# 1280 × 800 points on a display that draws two pixels a point: 2560 × 1600, one of the
# sizes the store takes. They go to dist/appstore/mac/ko and dist/appstore/mac/en, not
# into the repository.
#
#   scripts/dev/mac-store-shots.sh
#   ONEUM_DEMO_LIBRARY   a folder that scripts/dev/gen-demo-library.py made, where there
#                        is one (making it takes some minutes; default: target/demo-library,
#                        made when it is not there and kept)
#   ONEUM_SHOTS_OUTPUT   a part of the name of the output device the pictures are to
#                        name (default: the system's; the app's own choice, the system's
#                        is left alone)
#
# The music is made up (gen-demo-library.py), and so are the lyrics (those of
# scripts/dev/appstore-shots.sh): nothing in the pictures is anybody's. Needs ffmpeg
# and Pillow, a display that draws two pixels a point with room for the window, an
# unlocked screen and a mouse that is left alone while it runs.
#
# Silent: the app's volume is 0 whenever something plays. Most pictures show the volume,
# so they are taken with the song paused and the volume at full for that time; the
# meters need the song to play and are taken at volume 0.
#
# The app is the automation build in dist/dev with a throw-away profile: an OnEum that
# runs from elsewhere and its data are left alone.
set -eu
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"
OUT=dist/appstore/mac
WORK=$(mktemp -d /tmp/oneum-mac-shots.XXXXXX)
PROFILE=""
APP="dist/dev/OnEum.app/Contents/MacOS/OnEum"

cleanup() {
  # (Whatever happened, nothing is left playing aloud.)
  [ -z "$PROFILE" ] || printf 'volume 0\nquit\n' >> "$PROFILE/script.txt" 2>/dev/null || true
  sleep 1
  pkill -f "$APP" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

ui() { # ui <command>…: sends commands and waits until the app has carried them out
  # (It does them in order: when the file of a last "dump" is there, all are done.)
  outs=""
  for c in "$@" 'dump done.json'; do
    case "$c" in
      dump\ *)
        f="$PROFILE/${c#* }"; rm -f "$f"; outs="$outs $f"
        printf 'dump %s\n' "$f" >> "$PROFILE/script.txt" ;;
      *) printf '%s\n' "$c" >> "$PROFILE/script.txt" ;;
    esac
  done
  for f in $outs; do
    i=0
    while [ ! -f "$f" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i + 1)); done
    [ -f "$f" ] || { echo "timed out waiting for $f" >&2; exit 1; }
  done
}
value() { # value <python expression over d (the state)> [more arguments, as a]
  ui 'dump state.json'
  expression=$1
  shift
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
a = sys.argv[3:]
print(eval(sys.argv[2]))' "$PROFILE/state.json" "$expression" "$@"
}
must() { # must <python expression> <what it says>: stops where the app is not there
  [ "$(value "$1")" = True ] || { echo "not as expected: $2" >&2; exit 1; }
}
until_true() { # until_true <python expression> <what it says>: waits up to a minute
  i=0
  while [ "$(value "$1")" != True ]; do
    i=$((i + 1))
    [ $i -lt 60 ] || { echo "waited in vain: $2" >&2; exit 1; }
    sleep 1
  done
}
shot() { # shot <name>: what the window shows (the app draws it into the file itself)
  mkdir -p "$OUT/$LANGUAGE"
  target="$PWD/$OUT/$LANGUAGE/$1.png"
  rm -f "$target"
  ui "shot $target" 'wait 400'
  i=0
  while [ ! -f "$target" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
  [ -f "$target" ] || { echo "no picture $target" >&2; exit 1; }
  echo "  $LANGUAGE/$1.png"
}

echo "== the music"
LIBRARY=${ONEUM_DEMO_LIBRARY:-target/demo-library}
if [ ! -d "$LIBRARY" ] || [ -z "$(ls "$LIBRARY" 2>/dev/null)" ]; then
  mkdir -p "$LIBRARY"
  python3 scripts/dev/gen-demo-library.py "$LIBRARY"
fi
# A copy, with made-up lyrics beside one song of each language (the ones of the iPhone
# pictures: times, so that a line is lit).
MUSIC="$WORK/Music"
cp -R "$LIBRARY" "$MUSIC"
lyrics() { # lyrics <variable of appstore-shots.sh that names the song> <file>
  sed -n "/$1.lrc\" <<'EOF'/,/^EOF/p" scripts/dev/appstore-shots.sh | sed '1d;$d' > "$2"
  [ "$(wc -l < "$2" | tr -d ' ')" = 12 ] || { echo "no lyrics for $2" >&2; exit 1; }
}
for song in "달빛 사중주/온음계/01 온음" "Atlas Minor/Deep Field/01 Glass Hour"; do
  [ -f "$MUSIC/$song.flac" ] || { echo "$LIBRARY is not a demo library: no $song.flac" >&2; exit 1; }
done
lyrics KO_SONG "$MUSIC/달빛 사중주/온음계/01 온음.lrc"
lyrics EN_SONG "$MUSIC/Atlas Minor/Deep Field/01 Glass Hour.lrc"
SONGS=$(find "$MUSIC" -type f \( -name '*.flac' -o -name '*.mp3' \) | wc -l | tr -d ' ')

pictures() { # pictures <language> <its name in the settings> <album> <song> <search>
  LANGUAGE=$1
  echo "== $LANGUAGE"
  # A profile of its own for each language: the dark look and the language are set
  # before the app starts, everything else is as a first start has it.
  PROFILE="$WORK/profile-$LANGUAGE"
  mkdir -p "$PROFILE/data"
  printf '{"language":"%s","theme":"Dark"}\n' "$2" > "$PROFILE/data/settings.json"
  scripts/dev/run.sh "$PROFILE" --release >/dev/null
  # ('front' keeps the window above others without taking the keyboard: a window that
  # is covered is not drawn.)
  ui 'front' 'resize 1280 800' 'wait 600' 'volume 0' 'wait 300'
  must 'd["volume"] == 0' "the app's volume is 0"
  must 'd["window_size"] == [1280.0, 800.0] and d["pixels_per_point"] == 2.0' \
    "a window of 1280 x 800 points at two pixels a point (the display is too small, or draws one pixel a point)"
  must 'd["dark"] and d["right_panel"] == "Hidden" and not d["meter_visible"]' "a first start, dark"
  if [ -n "${ONEUM_SHOTS_OUTPUT:-}" ]; then
    ui "menu output_device $ONEUM_SHOTS_OUTPUT" 'wait 500'
  fi
  ui "open $MUSIC" 'wait 1500'
  until_true "d['library_tracks'] == $SONGS and not d['scan_running']" "the $SONGS songs are read"
  until_true 'd["toast"] is None' "the note about the scan has gone"

  # Where things are, in a window of this size (points): the sidebar's rows, the search
  # field, the albums six in a row, the first song of an album, the right panel.
  ui 'click 53 129' 'wait 1200'
  must 'd["nav"] == "Albums"' "the albums are open"
  COLUMN=$(value 'd["first_rows"].index(a[0])' "$3")
  [ "$COLUMN" -lt 6 ] || { echo "the album $3 is not in the first row" >&2; exit 1; }
  ui "click $((298 + 172 * COLUMN)) 134" 'wait 1200'
  must 'd["nav"].startswith("Album(")' "the album is open"
  ui 'dclick 500 244' 'wait 1500'
  [ "$(value 'd["state"] == "Playing" and d["volume"] == 0 and d["queue_index"] == 0 and d["current"] == a[0]' "$4")" = True ] \
    || { echo "not as expected: the album's first song plays, at volume 0" >&2; exit 1; }
  if [ -n "${ONEUM_SHOTS_OUTPUT:-}" ]; then
    [ "$(value 'a[0] in (d["output_device"] or "")' "$ONEUM_SHOTS_OUTPUT")" = True ] \
      || { echo "not as expected: the sound goes to the device named $ONEUM_SHOTS_OUTPUT" >&2; exit 1; }
  fi

  # The album while it plays: the song's details beside it, the meters under it.
  ui 'menu toggle_info' 'wait 600' 'menu toggle_meter' 'wait 3000'
  must 'd["right_panel"] == "Info" and d["meter_visible"] and d["state"] == "Playing" and d["volume"] == 0' \
    "the details and the meters, with the song playing at volume 0"
  shot 02-playing
  ui 'menu toggle_meter' 'wait 400'

  # The lyrics. Their fourth line is sung from 0:22 to 0:28: the song is held there,
  # and only while it is held is the volume at full. The details are scrolled to where
  # the signal path and the lyrics are (the file's path, further down, stays out).
  until_true 'd["position_ms"] >= 23500' "the song is 23 seconds in"
  ui 'menu play_pause' 'wait 700'
  must 'd["state"] == "Paused" and not d["meter_visible"]' "the song is paused"
  ui 'volume 1' 'wait 300' 'scroll 1130 400 3000' 'wait 400' 'scroll 1130 400 -556' 'wait 800'
  must 'd["lyrics"]["timed_lines"] == 12 and d["lyrics"]["current_line"] == 3' "the fourth line of the lyrics is lit"
  shot 03-lyrics

  # The albums, with the queue beside them.
  ui 'click 53 129' 'wait 800' 'menu toggle_queue' 'wait 800'
  must 'd["nav"] == "Albums" and d["right_panel"] == "Queue" and d["state"] == "Paused"' "the albums and the queue"
  shot 01-albums

  # The equalizer, switched on, with one of its presets (the ninth of the list).
  ui 'menu equalizer' 'wait 900'
  must 'd["eq_open"]' "the equalizer is open"
  [ "$(value 'd["eq"]["enabled"]')" = True ] || ui 'click 904 153' 'wait 500'
  ui 'click 485 203' 'wait 800' 'click 397 487' 'wait 900'
  must 'd["eq"]["enabled"] and d["eq"]["preset"] == "jazz"' "the equalizer is on, with the jazz preset"
  shot 04-equalizer
  ui 'click 904 153' 'wait 500' 'click 954 153' 'wait 600'
  must 'not d["eq"]["enabled"] and not d["eq_open"]' "the equalizer is off and put away"

  # A search among all songs.
  ui 'click 60 99' 'wait 800' 'click 200 22' 'wait 300' "text $5" 'wait 900'
  must 'd["nav"] == "Songs" and d["query"] != "" and d["rows"] > 0' "the search found songs"
  shot 05-search
  ui 'key Escape' 'wait 400'

  # The output: the device and how it is used (the second page of the settings).
  ui 'menu settings' 'wait 900' 'click 560 196' 'wait 900'
  must 'd["settings_open"]' "the settings are open"
  shot 06-output
  ui 'click 924 157' 'wait 500'
  must 'not d["settings_open"]' "the settings are closed"

  ui 'volume 0' 'wait 300'
  must 'd["volume"] == 0 and d["state"] == "Paused"' "the volume is 0 again"
  # (Not through ui: an app that has ended answers nothing.)
  printf 'quit\n' >> "$PROFILE/script.txt"
  sleep 2
  pkill -f "$APP" 2>/dev/null || true
}
# (Korean is searched by the first consonants of its syllables: ㄷㅂ finds 달빛.)
pictures ko Korean '온음계' '온음' 'ㄷㅂ'
pictures en English 'Deep Field' 'Glass Hour' 'light'

echo "== checking the pictures"
# The store takes no picture with transparency, and only its own sizes.
python3 - "$OUT" <<'PY'
import sys
from pathlib import Path
from PIL import Image

SIZE = (2560, 1600)
out = Path(sys.argv[1])
wrong = 0
paths = sorted(out.glob("[a-z][a-z]/*.png"))
for path in paths:
    image = Image.open(path)
    if image.mode != "RGB":
        image = image.convert("RGB")
        image.save(path)
    ok = image.size == SIZE
    wrong += not ok
    print(("  ok   " if ok else "  FAIL ") + f"{path} {image.size[0]}x{image.size[1]}")
sys.exit(1 if wrong or len(paths) != 12 else 0)
PY
echo "the pictures are in $OUT"
