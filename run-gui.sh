#!/usr/bin/env bash
# Launch the macOS menu bar app FROM A TERMINAL.
#
# WHY THIS (and not `open "Pianomi.app"`):
#   Accessibility (TCC) permission is tied to the *responsible* app's code
#   signature. Launching the .app bundle directly ties it to the bundle's
#   ad-hoc signature, which changes on every `cargo build` -> permission keeps
#   resetting ("kebanyakan build app"). Launching from a terminal ties it to
#   YOUR TERMINAL APP, which has a stable identity, so the grant survives every
#   rebuild. Grant it once and forget about it.
#
# Usage:
#   ./run-gui.sh                 # icon appears in the menu bar, song preloaded
#   ./run-gui.sh other.mid
#
# The terminal stays attached (the app's event loop runs in the foreground).
# Press Ctrl+C to quit.
set -euo pipefail
cd "$(dirname "$0")"

SONG="${1:-Spring_Thief.mid}"

# --- Accessibility pre-flight -------------------------------------------
# AXIsProcessTrusted() reflects the *responsible* app (our terminal), not the
# child binary. If the terminal isn't trusted yet, surface the system prompt
# and open System Settings so the user can enable this terminal app.
python3 - <<'PY' || true
import ctypes as C
cf = C.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
ax = C.CDLL("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")
cf.CFStringCreateWithCString.restype = C.c_void_p
cf.CFStringCreateWithCString.argtypes = [C.c_void_p, C.c_char_p, C.c_uint32]
cf.CFDictionaryCreate.restype = C.c_void_p
cf.CFDictionaryCreate.argtypes = [C.c_void_p, C.c_void_p, C.c_void_p, C.c_long, C.c_void_p, C.c_void_p]
ax.AXIsProcessTrusted.restype = C.c_bool
ax.AXIsProcessTrustedWithOptions.argtypes = [C.c_void_p]
ax.AXIsProcessTrustedWithOptions.restype = C.c_bool

kCFBooleanTrue = C.c_void_p.in_dll(cf, "kCFBooleanTrue")
prompt_key = cf.CFStringCreateWithCString(None, b"AXTrustedCheckOptionPrompt", 0x08000100)
keys = (C.c_void_p * 1)(prompt_key)
vals = (C.c_void_p * 1)(kCFBooleanTrue)
opts = cf.CFDictionaryCreate(None, keys, vals, 1, None, None)

if not ax.AXIsProcessTrusted():
    print("Accessibility is NOT granted for this terminal yet.")
    print("  -> a system prompt should appear; click 'Open System Settings'")
    print("     then enable YOUR TERMINAL APP (Terminal / iTerm / Warp / etc.)")
    # Show the prompt (attributes to this terminal), and also jump to Settings.
    ax.AXIsProcessTrustedWithOptions(opts)
    import subprocess
    subprocess.run([
        "open",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    ])
    print("Grant it, then run ./run-gui.sh again. (You can ignore the prompt if")
    print("you already enabled the terminal manually.)")
PY

if [ -x ./target/release/pianomi-gui ]; then
  BIN=./target/release/pianomi-gui
else
  BIN=./target/debug/pianomi-gui
fi

echo "Starting menu bar app — look for the music-note icon at the top-right."
echo "Song: $SONG   |   Press Ctrl+C here to quit."
exec "$BIN" "$SONG"
