#!/bin/sh
# Builds a debug bundle and launches it against a throw-away profile with the
# verification script channel enabled (see src/ui/automation.rs).
#   scripts/dev/run.sh <profile-dir> [--release] [extra `open` args…]
# The system media session (media keys / Now Playing) is left alone unless ONEUM_MEDIA=1.
set -eu
cd "$(dirname "$0")/../.."
PROFILE=${1:?usage: run.sh <profile-dir>}
shift
MODE=--debug
if [ "${1:-}" = "--release" ]; then MODE=""; shift; fi
# shellcheck disable=SC2086
scripts/package.sh $MODE >/dev/null
pkill -f "dist/OnEum.app/Contents/MacOS/OnEum" 2>/dev/null || true
sleep 0.5
mkdir -p "$PROFILE"
: > "$PROFILE/script.txt"
MEDIA_ENV="--env ONEUM_NO_MEDIA_SESSION=1"
[ "${ONEUM_MEDIA:-0}" = 1 ] && MEDIA_ENV=""
# shellcheck disable=SC2086
open -n dist/OnEum.app --env ONEUM_DATA_DIR="$PROFILE" --env ONEUM_SCRIPT="$PROFILE/script.txt" $MEDIA_ENV \
  --env ONEUM_LOG="${ONEUM_LOG:-info}" --stderr "$PROFILE/stderr.log" "$@"
sleep 3
pgrep -f "dist/OnEum.app/Contents/MacOS/OnEum" >/dev/null && echo "running" || { echo "not running" >&2; tail -20 "$PROFILE/stderr.log" >&2; exit 1; }
