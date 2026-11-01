#!/usr/bin/env bash
# Run the MIDI -> Roblox piano player from a terminal.
#
# Usage:
#   ./run.sh                      # plays Spring_Thief.mid with defaults
#   ./run.sh other.mid            # play another file
#   ./run.sh Spring_Thief.mid --humanize 80 -s 1.2 --transpose -24
#
# NOTE: macOS must grant *Accessibility* to your terminal app (Terminal /
# iTerm / etc.), not to this script. See the notes in the chat for how.
set -euo pipefail
cd "$(dirname "$0")"

SONG="${1:-Spring_Thief.mid}"
if [ "$#" -gt 0 ]; then
  shift
fi

if [ -x ./target/release/pianomi ]; then
  BIN=./target/release/pianomi
else
  BIN=./target/debug/pianomi
fi

echo "Playing: $SONG"
echo "Focus the Roblox window NOW — playback starts in ~3s (Ctrl+C to cancel)."
exec "$BIN" play -f "$SONG" "$@"
