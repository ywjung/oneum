# Sourced by the smoke tests: the checks of room correction, which are the same on
# every system. They use the caller's `ui` and `check`.
#
#   room_file <where to write it>      makes an impulse response file there
#   room_checks <the file as the app reaches it>
#
# To be called while a song plays. The response is one that changes little (half as
# loud, nothing else): what is checked is that it is taken in, kept, applied to what
# plays, switched off and on, and taken away with its file.
room_file() {
  python3 - "$1" <<'PY'
import struct, sys, wave
with wave.open(sys.argv[1], "wb") as out:
    out.setnchannels(2)
    out.setsampwidth(2)
    out.setframerate(44100)
    frames = [(16384, 16384)] + [(0, 0)] * 63
    out.writeframes(b"".join(struct.pack("<hh", left, right) for left, right in frames))
PY
}
room_checks() {
  ui "menu room_import $1" 'wait 2500' 'dump rm1.json'
  check rm1.json 'd["room"]["kept"] == "room" and d["room"]["enabled"] and d["room"]["applied"] == "room" and d["room"]["kept_in_file"]' "room correction: an impulse response is imported, kept and applied"
  check rm1.json 'd["state"] == "Playing" and d["underruns"] == 0 and any(stage.startswith(("룸 보정", "Room correction")) for stage in (d["signal_path"] or []))' "the song plays on through it, and the signal path says so"
  ui 'menu room_toggle' 'wait 2000' 'dump rm2.json' 'menu room_toggle' 'wait 2000' 'dump rm3.json'
  check rm2.json 'not d["room"]["enabled"] and d["room"]["applied"] is None and d["room"]["kept"] == "room" and d["state"] == "Playing"' "switched off, it is kept and not applied"
  check rm3.json 'd["room"]["applied"] == "room" and d["state"] == "Playing" and d["underruns"] == 0' "switched on again, from what was kept"
  ui "menu room_import $1.not-there" 'wait 800' 'dump rm4.json' 'menu room_remove' 'wait 1500' 'dump rm5.json'
  check rm4.json 'd["room"]["applied"] == "room" and d["toast"]' "a file that is no sound is refused, and what was there stays"
  check rm5.json 'd["room"]["kept"] is None and d["room"]["applied"] is None and not d["room"]["kept_in_file"] and d["state"] == "Playing"' "taken away, with the file it was kept in"
}
