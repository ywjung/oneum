#!/bin/sh
# Makes what Google Play's store page shows of the Android app, in Korean and in
# English, on an emulator:
#
#   dist/play/store/<ko|en>/01-albums.png … 07-search.png   phone screenshots, 1080 × 1920
#   dist/play/store/feature-graphic-<ko|en>.png             the feature graphic, 1024 × 500
#   dist/play/store/icon-512.png                            the store's icon, 512 × 512
#   dist/play/store/foreground-service.mp4                  a recording for the
#       declaration of the playback service: a song is started, the app is left, the
#       song plays on and is paused from its notification
#
#   scripts/dev/play-shots.sh
#   ONEUM_ANDROID_SERIAL   the emulator to use (as `adb devices` names it); default: the only one
#   ONEUM_DEMO_LIBRARY     a folder that scripts/dev/gen-demo-library.py made, where there
#                          is one (making it takes some minutes; default: target/demo-library,
#                          made when it is not there and kept)
#
# Needs a running emulator of a phone 1080 pixels wide (the SDK's "Medium Phone"):
#
#   QEMU_AUDIO_DRV=none ~/Library/Android/sdk/emulator/emulator -avd <name> -no-window -no-audio &
#
# It is the build for Google Play that is shown (scripts/android-build.sh --play): the
# one that may read music and nothing beside it. So the covers and the lyrics of the
# pictures are inside the music files (a copy of the demo library is made that way),
# as they have to be for someone who gets the app from Play. The run also checks that
# this build does not offer all files access.
#
# The music is made up (gen-demo-library.py), and so are the lyrics below: nothing in
# the pictures is anybody's. Needs ffmpeg and Pillow.
#
# Silent: the app's volume is 0 whenever something plays, and the emulator's media
# volume is 0. The player and the lyrics show the volume, so they are taken with the
# song paused and the volume at full for that moment; the meters need the song to play
# and are taken at volume 0.
#
# The emulator's screen is set to 1080 × 1920 for the run (9:16, what the store asks
# for) and its status bar to a fixed clock; both are put back at the end, and the app,
# its data and the music the run put there are removed. It refuses to run on a real
# phone.
set -eu
cd "$(dirname "$0")/../.."
. scripts/android-env.sh
export PATH="$HOME/.cargo/bin:$PATH"
ADB="$ANDROID_SDK/platform-tools/adb"
[ -n "${ONEUM_ANDROID_SERIAL:-}" ] && ADB="$ADB -s $ONEUM_ANDROID_SERIAL"
PKG=app.oneum.android
REMOTE=/sdcard/Music/oneum-store
VERIFY=/data/user/0/$PKG/files/verify
OUT=dist/play/store
WORK=$(mktemp -d /tmp/oneum-play-shots.XXXXXX)
STATE="$WORK/state"
mkdir -p "$STATE"
# shellcheck disable=SC2086
[ "$($ADB shell getprop ro.kernel.qemu | tr -d '\r')" = 1 ] || [ "$($ADB shell getprop ro.boot.qemu | tr -d '\r')" = 1 ] \
  || { echo "this is not an emulator: the pictures are only made on one" >&2; exit 1; }
# shellcheck disable=SC2086
[ "$($ADB shell wm size | sed -n 's/^Physical size: //p' | tr -d '\r' | cut -dx -f1)" = 1080 ] \
  || { echo "the emulator's screen is not 1080 pixels wide" >&2; exit 1; }

demo() { # demo <command> [-e key value]…: the status bar's demonstration mode
  # shellcheck disable=SC2086
  $ADB shell am broadcast -a com.android.systemui.demo -e command "$@" >/dev/null
}
cleanup() {
  # (Whatever happened, nothing is left playing aloud.)
  $ADB shell am force-stop $PKG >/dev/null 2>&1 || true
  $ADB shell cmd locale set-app-locales $PKG --locales "" >/dev/null 2>&1 || true
  $ADB uninstall $PKG >/dev/null 2>&1 || true
  $ADB shell rm -rf "$REMOTE" /sdcard/oneum-store.mp4 /sdcard/oneum-ui.xml >/dev/null 2>&1 || true
  demo exit || true
  $ADB shell settings put global sysui_demo_allowed 0 >/dev/null 2>&1 || true
  $ADB shell wm size reset >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

ui() { # ui <command>…: sends commands and waits until the app has carried them out
  # (It does them in order: when the file of a last "dump" is there, all are done.)
  for c in "$@" 'dump state.json'; do
    case "$c" in
      dump\ *)
        $ADB shell "run-as $PKG rm -f files/verify/${c#* }" </dev/null
        printf 'dump %s\n' "$VERIFY/${c#* }" | $ADB shell "run-as $PKG sh -c 'cat >> files/verify/script.txt'" ;;
      *) printf '%s\n' "$c" | $ADB shell "run-as $PKG sh -c 'cat >> files/verify/script.txt'" ;;
    esac
  done
  i=0
  while ! $ADB shell "run-as $PKG test -f files/verify/state.json" </dev/null 2>/dev/null; do
    i=$((i + 1))
    [ $i -lt 300 ] || { echo "timed out waiting for the app" >&2; exit 1; }
    sleep 0.2
  done
  sleep 0.3
  $ADB exec-out run-as $PKG cat files/verify/state.json > "$STATE/state.json" </dev/null
}
value() { # value <python expression over d (the state) and p (its phone part)>
  ui 'wait 100'
  python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
p = d.get("phone", {})
print(eval(sys.argv[2]))' "$STATE/state.json" "$1"
}
must() { # must <python expression> <what it says>: stops where the app is not there
  [ "$(value "$1")" = True ] || { echo "not as expected: $2" >&2; exit 1; }
  echo "  ok   $2"
}
until_true() { # until_true <python expression> <what it says>: waits up to a minute
  i=0
  while [ "$(value "$1")" != True ]; do
    i=$((i + 1))
    [ $i -lt 60 ] || { echo "waited in vain: $2" >&2; exit 1; }
    sleep 1
  done
}
place() { # place <words>: where on the screen (in pixels) the thing that says so is
  # (From what the app tells a screen reader, as the system's own tool reads it: the
  # places of albums and buttons are not guessed.)
  $ADB shell uiautomator dump /sdcard/oneum-ui.xml >/dev/null 2>&1
  $ADB exec-out cat /sdcard/oneum-ui.xml | python3 -c '
import html, re, sys
want = sys.argv[1]
for node in re.findall(r"<node [^>]*>", sys.stdin.read()):
    said = [html.unescape(m) for m in re.findall(r"(?:text|content-desc)=\"([^\"]*)\"", node)]
    if want in said:
        l, t, r, b = map(int, re.findall(r"\d+", re.search(r"bounds=\"([^\"]*)\"", node).group(1)))
        print((l + r) // 2, (t + b) // 2)
        break
' "$1"
}
touch_on() { # touch_on <words>: a finger on the thing that says so
  at=$(place "$1")
  [ -n "$at" ] || { echo "nothing on the screen says '$1'" >&2; exit 1; }
  # shellcheck disable=SC2086
  $ADB shell input tap $at
  sleep 1.5
}
shot() { # shot <language> <name>: the screen as the phone shows it, status bar and all
  mkdir -p "$OUT/$1"
  $ADB exec-out screencap -p > "$OUT/$1/$2.png"
  echo "  $1/$2.png"
}
start() { # start <locale>: the app anew in that language, silent
  $ADB shell am force-stop $PKG
  $ADB shell cmd locale set-app-locales $PKG --locales "$1" >/dev/null
  $ADB shell "run-as $PKG sh -c 'mkdir -p files/verify && : > files/verify/script.txt'"
  $ADB shell am start -n $PKG/.MainActivity \
    --es ONEUM_SCRIPT "$VERIFY/script.txt" --es ONEUM_MUSIC_DIR "$REMOTE" >/dev/null
  sleep 6
  ui 'volume 0' 'wait 300'
  must 'd["volume"] == 0' "the app's volume is 0"
}

echo "== the music"
MUSIC=${ONEUM_DEMO_LIBRARY:-target/demo-library}
if [ ! -d "$MUSIC" ] || [ -z "$(ls "$MUSIC" 2>/dev/null)" ]; then
  mkdir -p "$MUSIC"
  python3 scripts/dev/gen-demo-library.py "$MUSIC"
fi
# The album and the song each language's pictures are of.
KO_ALBUM="달빛 사중주/온음계"; KO_SONG="01 온음"; KO_NAME="온음계"
EN_ALBUM="Atlas Minor/Deep Field"; EN_SONG="01 Glass Hour"; EN_NAME="Deep Field"
for song in "$KO_ALBUM/$KO_SONG" "$EN_ALBUM/$EN_SONG"; do
  [ -f "$MUSIC/$song.flac" ] || { echo "$MUSIC is not a demo library: no $song.flac" >&2; exit 1; }
done
# Made-up lyrics for one song of each language (times, so that a line is lit).
cat > "$WORK/ko.lrc" <<'EOF'
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
cat > "$WORK/en.lrc" <<'EOF'
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
# A copy of the library whose songs carry their album's cover, and those two their
# lyrics, inside the file: the build for Google Play reads music files and not the
# pictures and lyrics files beside them.
python3 - "$MUSIC" "$WORK/music" "$KO_ALBUM/$KO_SONG" "$WORK/ko.lrc" "$EN_ALBUM/$EN_SONG" "$WORK/en.lrc" <<'PY'
import subprocess
import sys
from pathlib import Path

source, out = Path(sys.argv[1]), Path(sys.argv[2])
lyrics = {Path(song): Path(text).read_text(encoding="utf-8")
          for song, text in zip(sys.argv[3::2], sys.argv[4::2])}
for track in sorted(source.rglob("*")):
    if track.suffix not in (".flac", ".mp3"):
        continue
    inside = track.relative_to(source)
    target = out / inside
    target.parent.mkdir(parents=True, exist_ok=True)
    cover = next((c for c in (track.parent / "cover.png", track.parent / "cover.jpg") if c.exists()), None)
    command = ["ffmpeg", "-v", "error", "-y", "-i", str(track)]
    if cover:
        command += ["-i", str(cover), "-map", "0:a", "-map", "1:v", "-c", "copy",
                    "-disposition:v", "attached_pic"]
        if track.suffix == ".mp3":
            command += ["-id3v2_version", "3", "-metadata:s:v", "title=Album cover",
                        "-metadata:s:v", "comment=Cover (front)"]
    else:
        command += ["-c", "copy"]
    text = lyrics.get(inside.with_suffix(""))
    if text:
        command += ["-metadata", f"LYRICS={text}"]
    subprocess.run(command + [str(target)], check=True)
PY
SONGS=$(find "$WORK/music" -type f \( -name '*.flac' -o -name '*.mp3' \) | wc -l | tr -d ' ')

echo "== the emulator: a screen of 1080 x 1920, a fixed status bar, silent"
$ADB shell wm size 1080x1920
$ADB shell cmd audio set-volume 3 0 >/dev/null 2>&1 || true
$ADB shell settings put global sysui_demo_allowed 1
demo enter
demo clock -e hhmm 0941
demo battery -e level 100 -e plugged false
demo network -e wifi show -e level 4
demo network -e mobile hide
demo notifications -e visible false

echo "== the app (the build for Google Play), installed afresh"
APK=$(scripts/android-build.sh --automation --play | tail -1)
$ADB shell am force-stop $PKG >/dev/null 2>&1 || true
$ADB uninstall $PKG >/dev/null 2>&1 || true
$ADB install "$APK" >/dev/null
$ADB shell rm -rf "$REMOTE"
$ADB shell mkdir -p "$REMOTE"
$ADB push "$WORK/music/." "$REMOTE/" >/dev/null
# The system's list of media is what lets an app with the right to music alone read a
# file: it is told of the new files.
$ADB shell cmd media_store scan-volume external_primary >/dev/null 2>&1 \
  || $ADB shell content call --uri content://media --method scan_volume --arg external_primary >/dev/null 2>&1 || true
# The two rights the app asks for are given beforehand, so that the system's questions
# do not come up over the pictures. It is never given all files access.
$ADB shell pm grant $PKG android.permission.READ_MEDIA_AUDIO
$ADB shell pm grant $PKG android.permission.POST_NOTIFICATIONS

pictures() { # pictures <language> <locale> <album's name> <search> <top|end>
  # (The album is among the first of the albums or among the last: the picture of the
  # albums is of that end of the list.)
  LANGUAGE=$1; ALBUM=$3
  echo "== $LANGUAGE"
  start "$2"
  until_true "d['library_tracks'] == $SONGS and not d['scan_running']" "the $SONGS songs are read"
  must 'not d["all_files_offered"] and not d["all_files_access"]' "the build for Google Play: it reads the music without all files access, and does not offer it"
  must '(d["language"] == "ko") == ("'"$LANGUAGE"'" == "ko")' "the app speaks $LANGUAGE"
  # Where things are (the constants of src/ui/phone.rs, as scripts/dev/smoke-android.sh
  # has them), in the app's points.
  must 'round(d["screen"]["full"][2]) == 411' "a screen 411 points wide"
  W=$(value 'round(d["screen"]["full"][2])')
  TOP=$(value 'round(d["screen"]["safe"][1])')
  BOTTOM=$(value 'round(d["screen"]["safe"][3])')
  HEAD=$((TOP + 25)); TABS=$((BOTTOM - 27)); MINI=$((BOTTOM - 85))

  # The album, played from its first song (silently); then the pictures of the lists,
  # with the song that plays in the bar above the tabs.
  ui "tap $((W / 8)) $TABS" 'wait 600' "tap 200 $((TOP + 131))" 'wait 1500'
  must 'p["page"] == "List(Albums)"' "the albums are open"
  [ "$5" != end ] || ui "scroll 200 $((TOP + 300)) -3000" 'wait 1200'
  # (Should the album still be further down than the screen is tall, the list is moved
  # up until the album is on it, clear of the bars at the bottom.)
  i=0
  while :; do
    at=$(place "$ALBUM")
    [ -z "$at" ] || [ "${at##* }" -gt 1300 ] || break
    i=$((i + 1))
    [ $i -lt 6 ] || { echo "the album $ALBUM is not among the albums" >&2; exit 1; }
    $ADB shell input swipe 540 1300 540 800 600
    sleep 1.5
  done
  touch_on "$ALBUM"
  must 'str(p["page"]).startswith("List(Album(")' "the album is open"
  ui "tap 108 $((TOP + 375))" 'wait 2500'
  must 'd["state"] == "Playing" and d["volume"] == 0 and d["queue_index"] == 0' "its first song plays, at volume 0"
  $ADB shell input keyevent KEYCODE_BACK
  ui 'wait 1500'
  must 'p["page"] == "List(Albums)"' "back among the albums"
  [ "$5" != end ] || ui "scroll 200 $((TOP + 300)) -3000" 'wait 1200'
  shot "$LANGUAGE" 01-albums
  touch_on "$ALBUM"
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
  ui "tap $((W - 112)) $HEAD" 'wait 1500'
  must 'p["player_view"] == "Lyrics" and d["lyrics"]["timed_lines"] == 12' "the lyrics that are inside the file are shown"
  shot "$LANGUAGE" 04-lyrics
  ui 'volume 0' 'wait 500'
  must 'd["volume"] == 0 and d["state"] == "Paused"' "the volume is 0 again before the song goes on"
  ui 'menu play_pause' "tap $((W - 70)) $HEAD" 'wait 3000'
  must 'p["player_view"] == "Meters" and d["state"] == "Playing" and d["volume"] == 0' "the meters, with the song playing at volume 0"
  shot "$LANGUAGE" 05-meters
  ui "tap $((W - 70)) $HEAD" 'wait 600'
  $ADB shell input keyevent KEYCODE_BACK
  ui 'wait 1000'
  must 'not p["player_open"]' "the player is put away"

  # The equalizer, switched on, with one of its presets.
  ui "tap $((W * 7 / 8)) $TABS" 'wait 900' "tap 200 $((TOP + 185))" 'wait 1200'
  must 'p["page"] == "Settings(Equalizer)"' "the equalizer is open"
  [ "$(value 'd["eq"]["enabled"]')" = True ] || ui "tap $((W - 57)) $((TOP + 90))" 'wait 600'
  [ "$(value 'd["eq"]["preset"]')" = jazz ] \
    || ui "tap $((W / 2 - 30)) $((TOP + 158))" 'wait 900' "tap 100 $((TOP + 537))" 'wait 1200'
  must 'd["eq"]["enabled"] and d["eq"]["preset"] == "jazz"' "the equalizer is on, with the jazz preset"
  shot "$LANGUAGE" 06-equalizer
  # (Off again: the other language starts from the same state, and its meters show
  # the same steps under them.)
  ui "tap $((W - 57)) $((TOP + 90))" 'wait 600'
  must 'not d["eq"]["enabled"]' "the equalizer is off again"
  $ADB shell input keyevent KEYCODE_BACK
  ui 'wait 800'

  # A search. The keyboard is put away again for the picture: what is found is what
  # the picture is of.
  ui "tap $((W * 3 / 8)) $TABS" 'wait 800' "tap 150 $((TOP + 72))" 'wait 600' "text $4" 'wait 1500'
  must 'p["tab"] == "Search" and p["search_rows"] > 0' "the search found songs"
  $ADB shell input keyevent KEYCODE_BACK
  ui 'wait 1200'
  must 'not d["text_focus"] and p["tab"] == "Search" and p["search_rows"] > 0' "the keyboard is away, the songs that were found stay"
  shot "$LANGUAGE" 07-search
}
# (Korean is searched by the first consonants of its syllables: ㄷㅂ finds 달빛.)
pictures ko ko-KR "$KO_NAME" 'ㄷㅂ' end
pictures en en-US "$EN_NAME" 'light' top

echo "== a recording for the declaration of the playback service"
# What Google Play asks to be shown of a service that plays media: the user starts a
# song, and it plays on with the app off the screen. In English, silent (volume 0).
start en-US
until_true "d['library_tracks'] == $SONGS and not d['scan_running']" "the songs are read"
W=$(value 'round(d["screen"]["full"][2])'); TOP=$(value 'round(d["screen"]["safe"][1])')
BOTTOM=$(value 'round(d["screen"]["safe"][3])'); TABS=$((BOTTOM - 27))
demo exit
$ADB shell screenrecord --time-limit 40 --bit-rate 4000000 /sdcard/oneum-store.mp4 &
RECORDER=$!
sleep 2
ui "tap $((W / 8)) $TABS" 'wait 1000' "tap 200 $((TOP + 131))" 'wait 2000'
touch_on "$EN_NAME"
sleep 1
ui "tap 108 $((TOP + 375))" 'wait 4000'
must 'd["state"] == "Playing" and d["volume"] == 0' "a song is started by a tap, at volume 0"
$ADB shell input keyevent KEYCODE_HOME
sleep 4
[ "$($ADB shell dumpsys media_session | grep -A14 "package=$PKG" | sed -n 's/.*state=PlaybackState {state=\([A-Z_]*\).*/\1/p' | head -1)" = PLAYING ] \
  || { echo "the song does not play on with the app off the screen" >&2; exit 1; }
echo "  ok   the song plays on with the app off the screen"
$ADB shell cmd statusbar expand-notifications
sleep 5
$ADB shell cmd media_session dispatch pause >/dev/null
sleep 3
$ADB shell cmd statusbar collapse
sleep 2
wait $RECORDER || true
mkdir -p "$OUT"
$ADB pull /sdcard/oneum-store.mp4 "$WORK/recording.mp4" >/dev/null
# (Without sound and without what the recorder wrote about itself.)
ffmpeg -v error -y -i "$WORK/recording.mp4" -an -map_metadata -1 -c:v libx264 -crf 26 -pix_fmt yuv420p "$OUT/foreground-service.mp4"
echo "  foreground-service.mp4"

echo "== the feature graphic and the store's icon"
# Drawn here, with the letters of the fonts Android itself has (Roboto and Noto Sans
# CJK, both free to use), fetched from the emulator.
$ADB pull /system/fonts/Roboto-Regular.ttf "$WORK/latin.ttf" >/dev/null
$ADB pull /system/fonts/NotoSansCJK-Regular.ttc "$WORK/korean.ttc" >/dev/null
python3 scripts/dev/play-graphics.py "$OUT" "$WORK/latin.ttf" "$WORK/korean.ttc"

echo "== checking the pictures"
# What Google Play takes: 2 to 8 phone screenshots, PNG or JPEG, each side 320 to 3,840
# pixels, the long side at most 2.3 times the short one; to be shown in its
# recommendations, four or more at 1080 x 1920 or larger, 9:16. A feature graphic of
# 1024 x 500 without transparency, an icon of 512 x 512.
python3 - "$OUT" <<'PY'
import sys
from pathlib import Path
from PIL import Image

out = Path(sys.argv[1])
wrong = 0
def look(path, size, modes):
    global wrong
    image = Image.open(path)
    ok = image.size == size and image.mode in modes
    wrong += not ok
    print(("  ok   " if ok else "  FAIL ") + f"{path} {image.size[0]}x{image.size[1]} {image.mode}")
for language in ("ko", "en"):
    shots = sorted((out / language).glob("*.png"))
    if len(shots) != 7:
        wrong += 1
        print(f"  FAIL {len(shots)} pictures in {out / language}, not 7")
    for path in shots:
        image = Image.open(path)
        if image.mode != "RGB":
            image.convert("RGB").save(path)
        look(path, (1080, 1920), ("RGB",))
    look(out / f"feature-graphic-{language}.png", (1024, 500), ("RGB",))
look(out / "icon-512.png", (512, 512), ("RGBA",))
sys.exit(1 if wrong else 0)
PY
echo "the pictures are in $OUT"
