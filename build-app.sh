#!/usr/bin/env bash
# Rebuild the menu bar app and refresh the .app bundle with the fresh binary.
#
# The bundle holds a *copy* of the executable, so after every `cargo build` the
# copy goes stale — and a stale copy silently keeps the old bugs (including the
# SIGTRAP that was fixed in 0.2.0). Always run this before testing the bundle.
set -euo pipefail

cd "$(dirname "$0")"
APP="Pianomi.app"
BIN="target/release/pianomi-gui"

echo "==> building release"
cargo build --release

echo "==> refreshing $APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/pianomi-gui"

# --- signing -------------------------------------------------------------
# An ad-hoc signature (what the linker produces) is derived from the file's own
# hash, so it changes on EVERY build. macOS TCC keys the Accessibility grant on
# the code requirement, which for an ad-hoc binary is that hash — so the grant
# resets on every rebuild and the app starts failing with "synthetic input
# blocked".
#
# Signing with a real identity (Apple Development / Developer ID) makes the
# requirement `identifier + certificate`, which is stable across builds, so the
# permission is granted once and survives every rebuild afterwards.
# Sign by SHA-1 hash, not by name: two certificates can share the same display
# name (e.g. after a re-issued Apple Development cert) and `codesign -s <name>`
# then fails with "ambiguous".
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    SIGN_ID="$CODESIGN_IDENTITY"
else
    SIGN_ID="$(security find-identity -v -p codesigning 2>/dev/null \
        | sed -n 's/^[[:space:]]*[0-9]*) \([0-9A-F]\{40\}\) ".*"$/\1/p' | head -1)"
fi

if [ -n "${SIGN_ID:-}" ]; then
    echo "==> signing with: $SIGN_ID"
    if ! codesign --force --sign "$SIGN_ID" "$APP"; then
        echo "    signing failed — the bundle keeps its ad-hoc signature."
        echo "    Accessibility permission will reset on every rebuild."
    fi
else
    echo "==> no code-signing identity found — keeping the ad-hoc signature."
    echo "    Accessibility permission will reset on every rebuild."
fi

VER=$("$APP/Contents/MacOS/pianomi-gui" --version 2>/dev/null || true)
echo "==> done. bundle executable: $VER"
ls -l "$APP/Contents/MacOS/pianomi-gui" | awk '{print "    " $5 " bytes  " $6 " " $7 " " $8}'
echo
echo "Run it with:  open \"$APP\"        (or)  ./run-gui.sh"
