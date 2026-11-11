#!/bin/sh
# Creates a small synthetic music library for manual testing (requires ffmpeg).
#   scripts/dev/gen-test-library.sh <target-dir>
# Everything is synthesized (tones and generated cover images); nothing is copyrighted.
set -eu
OUT=${1:?usage: gen-test-library.sh <target-dir>}
FF="ffmpeg -v error -y"
mkdir -p "$OUT"

cover() { # cover <file> <color> <text>
  $FF -f lavfi -i "color=c=$2:s=600x600:d=1" -frames:v 1 -update 1 "$1"
}

track() { # track <dir> <file> <codec-args> <freq> <secs> <title> <artist> <album> <albumartist> <n> <year> <genre> [extra ffmpeg args]
  dir=$1; file=$2; codec=$3; freq=$4; secs=$5; title=$6; artist=$7; album=$8; aa=$9
  shift 9
  n=$1; year=$2; genre=$3; shift 3
  mkdir -p "$dir"
  # A tone with a slow tremolo so tracks are distinguishable by ear.
  # shellcheck disable=SC2086
  $FF -f lavfi -i "sine=frequency=$freq:sample_rate=44100:duration=$secs" \
      -af "volume=2,tremolo=f=3:d=0.4" -ac 2 $codec \
      -metadata title="$title" -metadata artist="$artist" -metadata album="$album" \
      -metadata album_artist="$aa" -metadata track="$n" -metadata date="$year" -metadata genre="$genre" \
      "$@" "$dir/$file"
}

A="$OUT/아이유/꽃갈피"
cover "$OUT/cover_a.png" "0x7B6CF6" a
track "$A" "01 나의 옛날이야기.flac" "-c:a flac" 262 40 "나의 옛날이야기" "아이유" "꽃갈피" "아이유" 1 2014 "가요" -metadata REPLAYGAIN_TRACK_GAIN="-6.50 dB" -metadata REPLAYGAIN_TRACK_PEAK="0.707" -metadata REPLAYGAIN_ALBUM_GAIN="-5.00 dB" -metadata REPLAYGAIN_ALBUM_PEAK="0.707"
track "$A" "02 꽃.flac" "-c:a flac" 294 35 "꽃" "아이유" "꽃갈피" "아이유" 2 2014 "가요" -metadata REPLAYGAIN_TRACK_GAIN="-3.00 dB" -metadata REPLAYGAIN_TRACK_PEAK="0.707" -metadata REPLAYGAIN_ALBUM_GAIN="-5.00 dB" -metadata REPLAYGAIN_ALBUM_PEAK="0.707"
track "$A" "03 사랑이 지나가면.flac" "-c:a flac" 330 45 "사랑이 지나가면" "아이유" "꽃갈피" "아이유" 3 2014 "가요"
track "$A" "04 너의 의미 (feat. 김창완).flac" "-c:a flac" 349 30 "너의 의미 (feat. 김창완)" "아이유" "꽃갈피" "아이유" 4 2014 "가요"
cp "$OUT/cover_a.png" "$A/cover.png"

B="$OUT/The Beatles/Abbey Road"
cover "$OUT/cover_b.png" "0x2E8B57" b
i=1
for t in "Come Together" "Something" "Maxwell's Silver Hammer" "Oh! Darling" "Octopus's Garden" "Here Comes the Sun"; do
  track "$B" "$(printf '%02d' $i) $t.mp3" "-c:a libmp3lame -b:a 192k" $((392 + i * 20)) $((25 + i * 3)) "$t" "The Beatles" "Abbey Road" "The Beatles" $i 1969 "Rock"
  i=$((i + 1))
done
cp "$OUT/cover_b.png" "$B/folder.png"

C="$OUT/Various/Late Night Sessions 🎧"
i=1
for pair in "Midnight Drive|Nova Lane" "서울의 밤|한강 프로젝트" "A Very Long Title That Keeps Going And Going To Test How The Interface Truncates Overflowing Text In Narrow Columns|The Verbose Orchestra" "Blue Hour|Nova Lane" "새벽 세 시|한강 프로젝트"; do
  title=${pair%%|*}; artist=${pair##*|}
  case $i in
    1) codec="-c:a aac -b:a 160k"; ext=m4a ;;
    2) codec="-c:a alac"; ext=m4a ;;
    3) codec="-c:a pcm_s16le"; ext=wav ;;
    4) codec="-c:a pcm_s16be"; ext=aiff ;;
    *) codec="-c:a vorbis -strict -2 -q:a 5"; ext=ogg ;;
  esac
  track "$C" "$(printf '%02d' $i) track.$ext" "$codec" $((440 + i * 30)) $((20 + i * 4)) "$title" "$artist" "Late Night Sessions 🎧" "Various Artists" $i 2021 "Electronic"
  i=$((i + 1))
done

D="$OUT/Nova Lane/Greatest Hits"
E="$OUT/한강 프로젝트/Greatest Hits"
track "$D" "01 First Light.flac" "-c:a flac" 523 28 "First Light" "Nova Lane" "Greatest Hits" "" 1 2019 "Pop"
track "$D" "02 Second Wind.flac" "-c:a flac" 587 26 "Second Wind" "Nova Lane" "Greatest Hits" "" 2 2019 "Pop"
track "$E" "01 강변북로.mp3" "-c:a libmp3lame -b:a 160k" 659 24 "강변북로" "한강 프로젝트" "Greatest Hits" "" 1 2020 "가요"
track "$E" "02 잠수교.mp3" "-c:a libmp3lame -b:a 160k" 698 22 "잠수교" "한강 프로젝트" "Greatest Hits" "" 2 2020 "가요"

# A file that is not audio at all, and one with a 48 kHz sample rate.
head -c 30000 /dev/urandom > "$OUT/broken.mp3"
$FF -f lavfi -i "sine=frequency=880:sample_rate=48000:duration=20" -af "volume=2" -ac 2 -c:a flac \
    -metadata title="48k Test Tone" -metadata artist="Test Signals" -metadata album="Sample Rates" "$OUT/tone_48k.flac"
rm -f "$OUT/cover_a.png" "$OUT/cover_b.png"
find "$OUT" -type f | wc -l
