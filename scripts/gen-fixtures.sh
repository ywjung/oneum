#!/bin/sh
# Regenerates the audio test fixtures in tests/fixtures (requires ffmpeg with
# libmp3lame; Vorbis uses the built-in encoder). All signals are synthesized here; nothing is copyrighted.
#
# "gap_a.*" and "gap_b.*" are the two halves of one continuous 441 Hz tone, cut exactly at
# sample 44100. Played back to back without a gap they must reproduce the original tone.
set -eu
cd "$(dirname "$0")/../tests/fixtures"
FF="ffmpeg -v error -y"
SINE="sine=frequency=441:sample_rate=44100:duration=2"

$FF -f lavfi -i "$SINE" -af "volume=4" -ac 2 -c:a pcm_s16le full.wav
$FF -i full.wav -af "atrim=end_sample=44100" -c:a pcm_s16le gap_a.wav
$FF -i full.wav -af "atrim=start_sample=44100,asetpts=PTS-STARTPTS" -c:a pcm_s16le gap_b.wav
for half in gap_a gap_b; do
  $FF -i $half.wav -c:a flac $half.flac
  $FF -i $half.wav -c:a libmp3lame -b:a 192k $half.mp3
  $FF -i $half.wav -c:a mp2 -b:a 256k $half.mp2
  $FF -i $half.wav -c:a aac -b:a 160k $half.aac.m4a
  $FF -i $half.wav -c:a alac $half.alac.m4a
  $FF -i $half.wav -c:a pcm_s16be $half.aiff
  $FF -i $half.wav -c:a vorbis -strict -2 -q:a 6 $half.ogg
done
rm full.wav

# Other sample rates / layouts for resampling and channel mapping.
$FF -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=1" -af "volume=4" -ac 2 -c:a pcm_s16le tone_48k.wav
$FF -f lavfi -i "sine=frequency=1000:sample_rate=96000:duration=1" -af "volume=4" -ac 2 -c:a flac -sample_fmt s32 -bits_per_raw_sample 24 tone_96k_24bit.flac
$FF -f lavfi -i "sine=frequency=1000:sample_rate=44100:duration=1" -af "volume=4" -ac 1 -c:a pcm_s16le tone_mono.wav

# Damaged inputs.
head -c 9000 gap_a.mp3 > truncated.mp3
python3 - <<'PY'
import random
random.seed(7)
data = bytearray(open("gap_a.flac", "rb").read())
for i in range(len(data) // 2, len(data) // 2 + 600):
    data[i] = random.randrange(256)
open("damaged_middle.flac", "wb").write(data)
open("garbage.mp3", "wb").write(bytes(random.randrange(256) for _ in range(20000)))
open("empty.flac", "wb").write(b"")
PY
ls -la
