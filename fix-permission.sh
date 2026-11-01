#!/usr/bin/env bash
# Fix macOS Accessibility permission for the MIDI piano player.
#
# Run this in the SAME terminal you will use to run ./run.sh:
#   ./fix-permission.sh
#
# It (1) reports whether Accessibility is already granted for this terminal,
# (2) triggers the macOS permission dialog if it is not, and (3) opens the
# Accessibility settings pane.
set -uo pipefail
cd "$(dirname "$0")"

echo "== Pianomi — permission helper =="
# Walk up the process tree to the nearest .app bundle (the responsible app).
pid=$$
app=""
while [ "$pid" -ne 1 ] && [ -n "$pid" ]; do
  comm=$(ps -o comm= -p "$pid" 2>/dev/null || true)
  case "$comm" in
    *.app/*) app="$comm"; break ;;
  esac
  pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')
done
echo "Responsible app: ${app:-unknown}"
echo

python3 - <<'PY'
import ctypes

cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
ax = ctypes.CDLL("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")

CFStringCreateWithCString = cf.CFStringCreateWithCString
CFStringCreateWithCString.restype = ctypes.c_void_p
CFStringCreateWithCString.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_uint32]
key = CFStringCreateWithCString(None, b"AXTrustedCheckOptionPrompt", 0x08000100)

CFDictionaryCreate = cf.CFDictionaryCreate
CFDictionaryCreate.restype = ctypes.c_void_p
CFDictionaryCreate.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p),
                               ctypes.POINTER(ctypes.c_void_p), ctypes.c_long,
                               ctypes.c_void_p, ctypes.c_void_p]
true_ref = ctypes.c_void_p.in_dll(cf, "kCFBooleanTrue")
keys = (ctypes.c_void_p * 1)(key)
vals = (ctypes.c_void_p * 1)(true_ref.value)
opts = CFDictionaryCreate(None, keys, vals, 1, None, None)

ax.AXIsProcessTrustedWithOptions.restype = ctypes.c_bool
ax.AXIsProcessTrustedWithOptions.argtypes = [ctypes.c_void_p]
trusted = ax.AXIsProcessTrustedWithOptions(opts)

if trusted:
    print("[OK] Accessibility is ALREADY granted for this terminal.")
    print("     Just run:  ./run.sh")
else:
    print("[!] Accessibility is NOT granted yet.")
    print("    A macOS dialog should have popped up. Click 'Open System Settings',")
    print("    enable your terminal app, then QUIT + reopen the terminal and run ./run.sh")
PY

echo
echo "Opening System Settings -> Privacy & Security -> Accessibility ..."
open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility" 2>/dev/null || true
echo "Enable the terminal app you are using, then restart it."
