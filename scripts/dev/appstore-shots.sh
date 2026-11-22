#!/bin/sh
# Makes the pictures of the iPhone app for the App Store, in Korean and in English:
# the albums, an album, the player, lyrics, the meters, the equalizer and a search,
# taken from the large-screen iPhone of the Simulator (1320 × 2868, the size the store
# asks for). They go to dist/appstore/ko and dist/appstore/en, not into the repository.
#
#   scripts/dev/appstore-shots.sh
#   ONEUM_IOS_DEVICE    the simulator, by name or identifier (default "iPhone 18 Pro Max";
#                       it has to be one whose screen is 440 points wide)
#   ONEUM_DEMO_LIBRARY  a folder that scripts/dev/gen-demo-library.py made, where there
#                       is one (making it takes some minutes; default: target/demo-library,
#                       made when it is not there and kept)
#
# The music is made up (gen-demo-library.py), and so are the lyrics below: nothing in
# the pictures is anybody's. Needs ffmpeg and Pillow.
#
# Silent: the app's volume is 0 whenever something plays. The player and the lyrics
# show the volume, so they are taken with the song paused and the volume at full for
# that moment; the meters need the song to play and are taken at volume 0.
#
# The app and its data on that simulator are removed first and again at the end.
set -eu
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"
BUNDLE=app.oneum.OnEum
OUT=dist/appstore
WORK=$(mktemp -d /tmp/oneum-appstore-shots.XXXXXX)
PROFILE="$WORK/profile"
mkdir -p "$PROFILE"

# The simulator: the one that is named, booted here if it is not (and then shut down
# again at the end). Of two with one name, the one that runs, or the newest system.
WANTED=${ONEUM_IOS_DEVICE:-iPhone 18 Pro Max}
FOUND=$(xcrun simctl list devices available -j | python3 -c '
import json, re, sys
wanted = sys.argv[1]
found = []
for runtime, devices in json.load(sys.stdin)["devices"].items():
    version = tuple(int(n) for n in re.findall(r"\d+", runtime.rsplit(".", 1)[-1]))
    for d in devices:
        if wanted in (d["name"], d["udid"]):
            found.append((d["state"] == "Booted", version, d["udid"], d["state"]))
if found:
    best = max(found)
    print(best[2], best[3])
' "$WANTED")
DEVICE=${FOUND%% *}
[ -n "$DEVICE" ] || { echo "no simulator named '$WANTED' (xcrun simctl list devices available)" >&2; exit 1; }
BOOTED_HERE=0
if [ "${FOUND##* }" != Booted ]; then
  xcrun simctl boot "$DEVICE"
  BOOTED_HERE=1
fi
xcrun simctl bootstatus "$DEVICE" -b >/dev/null

cleanup() {
  # (Whatever happened, nothing is left playing aloud.)
  printf 'volume 0\n' >> "$PROFILE/script.txt" 2>/dev/null || true
  xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
  xcrun simctl uninstall "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
  xcrun simctl status_bar "$DEVICE" clear >/dev/null 2>&1 || true
  [ "$BOOTED_HERE" = 0 ] || xcrun simctl shutdown "$DEVICE" >/dev/null 2>&1 || true
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
value() { # value <python expression over d (the state) and p (its phone part)>
  ui 'dump state.json'
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
p = d.get("phone", {})
print(eval(sys.argv[2]))' "$PROFILE/state.json" "$1"
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
shot() { # shot <language> <name>: the screen as the phone shows it, status bar and all
  mkdir -p "$OUT/$1"
  xcrun simctl io "$DEVICE" screenshot "$OUT/$1/$2.png" >/dev/null 2>&1
  echo "  $1/$2.png"
}
start() { # start <language> <locale>: the app anew in that language, silent
  xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
  sleep 1
  : > "$PROFILE/script.txt"
  SIMCTL_CHILD_ONEUM_SCRIPT="$PROFILE/script.txt" SIMCTL_CHILD_ONEUM_NO_MEDIA_SESSION=1 \
    xcrun simctl launch "$DEVICE" "$BUNDLE" -AppleLanguages "($1)" -AppleLocale "$2" >/dev/null
  sleep 4
  ui 'volume 0' 'wait 300'
  must 'd["volume"] == 0' "the app's volume is 0"
}

echo "== the music"
MUSIC=${ONEUM_DEMO_LIBRARY:-target/demo-library}
if [ ! -d "$MUSIC" ] || [ -z "$(ls "$MUSIC" 2>/dev/null)" ]; then
  mkdir -p "$MUSIC"
  python3 scripts/dev/gen-demo-library.py "$MUSIC"
fi
# The album and the song each language's pictures are of, and where that album is
# among the albums (they are in the order of their names: two beside each other).
KO_ALBUM="달빛 사중주/온음계"; KO_SONG="01 온음"
EN_ALBUM="Atlas Minor/Deep Field"; EN_SONG="01 Glass Hour"
for song in "$KO_ALBUM/$KO_SONG" "$EN_ALBUM/$EN_SONG"; do
  [ -f "$MUSIC/$song.flac" ] || { echo "$MUSIC is not a demo library: no $song.flac" >&2; exit 1; }
done

echo "== the app, installed afresh"
xcrun simctl terminate "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
xcrun simctl uninstall "$DEVICE" "$BUNDLE" >/dev/null 2>&1 || true
ONEUM_IOS_DEVICE="$DEVICE" scripts/ios-sim.sh --no-launch >/dev/null
# A first start makes the app's folders; then the music goes into its Documents, with
# made-up lyrics beside one song of each language (times, so that a line is lit).
start ko ko_KR
DOCUMENTS="$(xcrun simctl get_app_container "$DEVICE" "$BUNDLE" data)/Documents"
cp -R "$MUSIC"/* "$DOCUMENTS/"
cat > "$DOCUMENTS/$KO_ALBUM/$KO_SONG.lrc" <<'EOF'
[00:04.00]창가에 달이 걸리면
[00:10.00]한 음씩 방을 채우는 소리
[00:16.00]서두르지 않아도 돼
[00:22.00]온음 하나만큼만 걸어와
[00:28.00]낮은 현이 숨을 고르고
[00:34.00]높은 건반이 대답을 하면
[00:40.00]우리는 말없이 알게 돼
[00:46.00]오늘 밤의 박자를
[00:52.00]온전한 소리, 온전한 밤
[00:58.00]끝나지 않을 것처럼 흐르고
[01:04.00]마지막 음이 사라져도
[01:10.00]방 안엔 아직 달빛이 남아
EOF
cat > "$DOCUMENTS/$EN_ALBUM/$EN_SONG.lrc" <<'EOF'
[00:04.00]The window holds the morning still
[00:10.00]A glass hour, slow to fill
[00:16.00]Every sound a thread of light
[00:22.00]Drawn across the edge of night
[00:28.00]We listen till the room goes wide
[00:34.00]Nothing left to keep inside
[00:40.00]Stay a while, the hour is clear
[00:46.00]All the quiet gathers here
[00:52.00]Let it turn, let it glow
[00:58.00]Let the last note come and go
[01:04.00]When the glass is full again
[01:10.00]We will know the song by then
EOF
SONGS=$(find "$MUSIC" -type f \( -name '*.flac' -o -name '*.mp3' \) | wc -l | tr -d ' ')
# A keyboard that comes up for the first time on a simulator explains itself over its
# keys: it is told that it has done so.
for key in DidShowContinuousPathIntroduction DidShowGestureKeyboardIntroduction; do
  xcrun simctl spawn "$DEVICE" defaults write com.apple.Preferences "$key" -bool true || true
done
# (The clock of every screenshot of the store: 9:41, full bars, a full battery.)
xcrun simctl status_bar "$DEVICE" override --time "9:41" --batteryState charged \
  --batteryLevel 100 --wifiBars 3 --cellularBars 4 --cellularMode active --dataNetwork wifi

pictures() { # pictures <language> <locale> <row of the album among the albums> <search>
  LANGUAGE=$1
  echo "== $LANGUAGE"
  start "$LANGUAGE" "$2"
  until_true "d['library_tracks'] == $SONGS and not d['scan_running']" "the $SONGS songs are read"
  # Where things are (the constants of src/ui/phone.rs, as scripts/dev/smoke-ios.sh has
  # them; the places inside pages are those of a screen 440 points wide).
  must 'd["screen"]["full"][2] == 440' "a screen 440 points wide (an iPhone Pro Max)"
  TOP=$(value 'round(d["screen"]["safe"][1])')
  BOTTOM=$(value 'round(d["screen"]["safe"][3])')
  HEAD=$((TOP + 25)); TABS=$((BOTTOM - 27)); MINI=$((BOTTOM - 85))
  ALBUM_Y=$((TOP + 158 + 250 * $3))

  # The album, played from its first song (silently); then the pictures of the lists,
  # with the song that plays in the bar above the tabs.
  ui "tap 55 $TABS" 'wait 600' "tap 200 $((TOP + 131))" 'wait 1500'
  must 'p["page"] == "List(Albums)"' "the albums are open"
  ui "tap 110 $ALBUM_Y" 'wait 1200'
  must 'str(p["page"]).startswith("List(Album(")' "an album is open"
  ui "tap 115 $((TOP + 390))" 'wait 2500'
  must 'd["state"] == "Playing" and d["volume"] == 0 and d["queue_index"] == 0' "its first song plays, at volume 0"
  ui "tap 30 $HEAD" 'wait 1500'
  shot "$LANGUAGE" 01-albums
  ui "tap 110 $ALBUM_Y" 'wait 1500'
  must 'str(p["page"]).startswith("List(Album(")' "the album is open again"
  shot "$LANGUAGE" 02-album

  # The player. Its fourth line of lyrics is sung from 0:22 to 0:28: the song is held
  # there, and only while it is held is the volume at full.
  ui "tap 200 $MINI" 'wait 1200'
  must 'p["player_open"]' "the player is open"
  until_true 'd["position_ms"] >= 23500' "the song is 23 seconds in"
  ui 'menu play_pause' 'wait 700'
  must 'd["state"] == "Paused"' "the song is paused"
  ui 'volume 1' 'wait 500'
  shot "$LANGUAGE" 03-player
  ui "tap 328 $HEAD" 'wait 1500'
  must 'p["player_view"] == "Lyrics" and d["lyrics"]["timed_lines"] == 12' "the lyrics are shown"
  shot "$LANGUAGE" 04-lyrics
  ui 'volume 0' 'wait 500'
  must 'd["volume"] == 0 and d["state"] == "Paused"' "the volume is 0 again before the song goes on"
  ui 'menu play_pause' "tap 370 $HEAD" 'wait 3000'
  must 'p["player_view"] == "Meters" and d["state"] == "Playing" and d["volume"] == 0' "the meters, with the song playing at volume 0"
  shot "$LANGUAGE" 05-meters
  ui "tap 370 $HEAD" 'wait 600' "tap 30 $HEAD" 'wait 1000'
  must 'not p["player_open"]' "the player is put away"

  # The equalizer, switched on, with one of its presets (the ninth of the list).
  ui "tap 385 $TABS" 'wait 900' "tap 200 $((TOP + 185))" 'wait 1200'
  must 'p["page"] == "Settings(Equalizer)"' "the equalizer is open"
  [ "$(value 'd["eq"]["enabled"]')" = True ] || ui "tap 383 $((TOP + 90))" 'wait 600'
  [ "$(value 'd["eq"]["preset"]')" = jazz ] \
    || ui "tap 181 $((TOP + 158))" 'wait 900' "tap 100 $((TOP + 537))" 'wait 1200'
  must 'd["eq"]["enabled"] and d["eq"]["preset"] == "jazz"' "the equalizer is on, with the jazz preset"
  shot "$LANGUAGE" 06-equalizer
  # (Off again: the other language starts from the same state, and its meters show
  # the same steps under them.)
  ui "tap 383 $((TOP + 90))" 'wait 600'
  must 'not d["eq"]["enabled"]' "the equalizer is off again"
  ui "tap 30 $HEAD" 'wait 800'

  # A search, with the keyboard of the language.
  ui "tap 165 $TABS" 'wait 800' "tap 200 $((TOP + 78))" 'wait 600' "text $4" 'wait 1500'
  must 'p["tab"] == "Search" and p["search_rows"] > 0' "the search found songs"
  shot "$LANGUAGE" 07-search
}
# (Korean is searched by the first consonants of its syllables: ㄷㅂ finds 달빛.)
pictures ko ko_KR 2 'ㄷㅂ'
pictures en en_US 0 'light'

echo "== checking the pictures"
# The store takes no picture with transparency, and only its own sizes.
python3 - "$OUT" <<'PY'
import sys
from pathlib import Path
from PIL import Image

wrong = 0
for path in sorted(Path(sys.argv[1]).glob("*/*.png")):
    image = Image.open(path)
    if image.mode != "RGB":
        image = image.convert("RGB")
        image.save(path)
    ok = image.size == (1320, 2868)
    wrong += not ok
    print(("  ok   " if ok else "  FAIL ") + f"{path} {image.size[0]}x{image.size[1]}")
sys.exit(1 if wrong else 0)
PY
echo "the pictures are in $OUT"
