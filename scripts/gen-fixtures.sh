#!/bin/sh
# Regenerates the audio test fixtures in tests/fixtures (requires ffmpeg with
# libmp3lame and libopus; Vorbis and WavPack use the built-in encoders). All signals are synthesized here; nothing is copyrighted.
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

# Opus is always 48 kHz: the halves of a 480 Hz tone (a period of 100 samples again).
$FF -f lavfi -i "sine=frequency=480:sample_rate=48000:duration=2" -af "volume=4" -ac 2 -c:a pcm_s16le full48.wav
$FF -i full48.wav -af "atrim=end_sample=48000" -c:a pcm_s16le half48_a.wav
$FF -i full48.wav -af "atrim=start_sample=48000,asetpts=PTS-STARTPTS" -c:a pcm_s16le half48_b.wav
$FF -i half48_a.wav -c:a libopus -b:a 128k gap_a.opus
$FF -i half48_b.wav -c:a libopus -b:a 128k gap_b.opus
rm full48.wav half48_a.wav half48_b.wav
# WavPack, of the lossless fixtures.
$FF -i gap_a.wav -c:a wavpack gap_a.wv
$FF -i gap_b.wav -c:a wavpack gap_b.wv

# Other sample rates / layouts for resampling and channel mapping.
$FF -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=1" -af "volume=4" -ac 2 -c:a pcm_s16le tone_48k.wav
$FF -f lavfi -i "sine=frequency=1000:sample_rate=96000:duration=1" -af "volume=4" -ac 2 -c:a flac -sample_fmt s32 -bits_per_raw_sample 24 tone_96k_24bit.flac
$FF -f lavfi -i "sine=frequency=1000:sample_rate=44100:duration=1" -af "volume=4" -ac 1 -c:a pcm_s16le tone_mono.wav

# Blocks longer than the buffer the decoder writes into (a second): three seconds of
# tone in FLAC blocks of the largest size.
$FF -f lavfi -i "sine=frequency=441:sample_rate=44100:duration=3" -af "volume=4" -ac 2 -c:a flac -frame_size 65535 tone_big_blocks.flac
$FF -i tone_96k_24bit.flac -c:a wavpack tone_96k_24bit.wv
$FF -i tone_mono.wav -c:a wavpack tone_mono.wv

# Monkey's Audio: there is no encoder to make a file of music with here, so this one is
# put together by hand, of frames the format marks as silent (which carry no coded
# audio): three frames, 183,456 samples of silence in stereo at 44.1 kHz. It shows
# that the file is read, its length known and its frames found; what a decoder makes
# of coded audio it does not show.
python3 - <<'PY'
import struct, zlib
BLOCKS, LAST, FRAMES = 73728, 36000, 3
def frame(blocks):
    crc = (zlib.crc32(bytes(blocks * 4)) & 0xFFFFFFFF) >> 1
    # The checksum with the mark "special codes follow", the codes (left and right
    # silent), and the bytes the range decoder takes in before and after a frame.
    return struct.pack("<II", crc | 0x80000000, 3) + bytes(8)
frames = [frame(BLOCKS), frame(BLOCKS), frame(LAST)]
first = 52 + 24 + 4 * FRAMES
places, at = [], first
for f in frames:
    places.append(at)
    at += len(f)
descriptor = b"MAC " + struct.pack("<HHIIIIIII", 3990, 0, 52, 24, 4 * FRAMES, 0, at - first, 0, 0) + bytes(16)
header = struct.pack("<HHIIIHHI", 2000, 1 << 5, BLOCKS, LAST, FRAMES, 16, 2, 44100)
open("silence.ape", "wb").write(descriptor + header + struct.pack("<%dI" % FRAMES, *places) + b"".join(frames))
PY

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
