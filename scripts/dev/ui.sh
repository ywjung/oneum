#!/bin/sh
# Sends verification commands to an app started by scripts/dev/run.sh and waits for
# the files produced by any `shot`/`dump` in them.
#   scripts/dev/ui.sh <profile-dir> 'click 300 200' 'wait 300' 'dump state.json'
# `shot`/`dump` paths are taken relative to the profile directory.
set -eu
PROFILE=${1:?usage: ui.sh <profile-dir> <command>…}
shift
OUTS=""
for c in "$@"; do
  case "$c" in
    shot\ *|dump\ *)
      f="$PROFILE/${c#* }"; rm -f "$f"; OUTS="$OUTS $f"
      printf '%s %s\n' "${c%% *}" "$f" >> "$PROFILE/script.txt" ;;
    *) printf '%s\n' "$c" >> "$PROFILE/script.txt" ;;
  esac
done
for f in $OUTS; do
  i=0
  while [ ! -f "$f" ] && [ $i -lt 900 ]; do sleep 0.1; i=$((i + 1)); done
  [ -f "$f" ] || { echo "timed out waiting for $f" >&2; exit 1; }
done
