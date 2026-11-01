#!/usr/bin/env bash
# Build, refresh the bundle, stop any running copy, and install into /Applications.
#
# The installed copy is a *copy* of the bundle, so after every rebuild it goes
# stale — same trap as build-app.sh. Run this instead of dragging the .app
# around by hand.
set -euo pipefail

cd "$(dirname "$0")"
SRC="Pianomi.app"
DST="/Applications/Pianomi.app"

echo "==> building"
./build-app.sh

echo "==> stopping running copies"
pkill -f "pianomi-gui" 2>/dev/null || true
sleep 1
if pgrep -f "pianomi-gui" >/dev/null; then
    echo "    still running, forcing"
    pkill -9 -f "pianomi-gui" 2>/dev/null || true
    sleep 1
fi

echo "==> installing to $DST"
rm -rf "$DST"
ditto "$SRC" "$DST"
xattr -dr com.apple.quarantine "$DST" 2>/dev/null || true

echo "==> launching"
open "$DST"
sleep 1
pgrep -fl "pianomi-gui" || echo "    (not running — check Console)"

cat <<'NOTE'

Installed. Look for the music-note icon in the menu bar (no Dock icon).

Accessibility: the bundle is signed with a real Apple Development identity, so
the code requirement is stable and the grant survives rebuilds. If macOS asks,
enable "Pianomi" once in
  System Settings ▸ Privacy & Security ▸ Accessibility
NOTE
