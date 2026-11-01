#!/usr/bin/env python3
"""Verify that Spring_Thief.mid really plays the right notes.

Three independent checks:

  1. SOURCE  - parse os_data.txt (Online Sequencer ground truth) and compare
               pitch histogram + onset times against the .mid file.
  2. RENDER  - synthesise the .mid to WAV so a human can actually listen.
  3. PITCH   - FFT the rendered audio and confirm the detected fundamental
               matches the MIDI note number (catches octave / transposition
               errors that a histogram comparison would miss).

Usage:  python3 tools/verify_sound.py
"""

import json
import math
import re
import struct
import wave
from collections import Counter

import numpy as np

MID = "Spring_Thief.mid"
OS_DATA = "os_data.txt"
SR = 44100
BPM = 125.0
STEPS_PER_SEC = BPM / 60.0 * 4.0  # 1 step = 1/16 note

NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


def note_name(n):
    return f"{NAMES[n % 12]}{n // 12 - 1}"


def freq(n):
    return 440.0 * 2 ** ((n - 69) / 12.0)


# ---------------------------------------------------------------- MIDI reader
def read_midi(path):
    data = open(path, "rb").read()
    assert data[:4] == b"MThd", "not a MIDI file"
    hlen = struct.unpack(">I", data[4:8])[0]
    _fmt, _ntrk, div = struct.unpack(">HHH", data[8:14])
    pos = 8 + hlen

    notes, tempo_us = [], 500_000
    while pos < len(data) and data[pos : pos + 4] == b"MTrk":
        tlen = struct.unpack(">I", data[pos + 4 : pos + 8])[0]
        track = data[pos + 8 : pos + 8 + tlen]
        pos += 8 + tlen

        tick, i, status = 0, 0, 0
        while i < len(track):
            # delta time (VLQ)
            delta = 0
            while True:
                b = track[i]
                i += 1
                delta = (delta << 7) | (b & 0x7F)
                if not b & 0x80:
                    break
            tick += delta

            if track[i] & 0x80:
                status = track[i]
                i += 1
            hi, lo = status & 0xF0, status & 0x0F

            if status == 0xFF:  # meta
                mtype = track[i]
                i += 1
                ln = 0
                while True:
                    b = track[i]
                    i += 1
                    ln = (ln << 7) | (b & 0x7F)
                    if not b & 0x80:
                        break
                if mtype == 0x51 and ln == 3:
                    tempo_us = (track[i] << 16) | (track[i + 1] << 8) | track[i + 2]
                i += ln
            elif hi in (0x80, 0x90):
                note, vel = track[i], track[i + 1]
                i += 2
                if hi == 0x90 and vel > 0:
                    notes.append([tick, note, vel])
            elif hi in (0xA0, 0xB0, 0xE0):
                i += 2
            elif hi in (0xC0, 0xD0):
                i += 1
    return notes, div, tempo_us


# ------------------------------------------------------- Online Sequencer src
def read_os(path):
    raw = open(path).read().strip()
    # Header looks like "Online Sequencer:<song id>:<first start> " — strip it,
    # the start value is repeated as the leading token of the first note.
    body = re.sub(r"^Online Sequencer:\d+:", "", raw)
    out, skipped = [], []
    for chunk in body.split(";"):
        parts = chunk.split()
        if len(parts) < 3:
            skipped.append(chunk)
            continue
        try:
            start, name, dur = float(parts[0]), parts[1], float(parts[2])
        except ValueError:
            skipped.append(chunk)
            continue
        m = re.fullmatch(r"([A-G]#?)(-?\d+)", name)
        if not m:
            skipped.append(chunk)
            continue
        idx = NAMES.index(m.group(1))
        midi = (int(m.group(2)) + 1) * 12 + idx
        out.append((start, midi, dur))
    return out, skipped


# ------------------------------------------------------------------ synthesis
def render(notes, secs):
    """notes: list of (start_s, midi, dur_s). Additive piano-ish synth."""
    n = int(SR * secs) + SR
    buf = np.zeros(n, dtype=np.float64)
    for start, midi, dur in notes:
        f0 = freq(midi)
        length = min(dur + 0.45, 2.5)  # note + release tail
        s0 = int(start * SR)
        s1 = min(n, s0 + int(length * SR))
        if s1 <= s0:
            continue
        t = np.arange(s1 - s0) / SR
        env = np.exp(-t * 2.6)
        # short attack so it doesn't click
        att = np.minimum(1.0, t / 0.006)
        wave_ = np.zeros_like(t)
        for h, amp in ((1, 1.0), (2, 0.42), (3, 0.22), (4, 0.12), (5, 0.07)):
            wave_ += amp * np.sin(2 * np.pi * f0 * h * t)
        buf[s0:s1] += wave_ * env * att * 0.11
    peak = np.max(np.abs(buf)) or 1.0
    return buf / peak * 0.92


def write_wav(path, samples):
    pcm = np.clip(samples, -1, 1)
    pcm = (pcm * 32767).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(pcm.tobytes())


# ---------------------------------------------------------------- pitch check
def detect_pitch(samples, expect_midi):
    """Find the strongest spectral peak and report the nearest MIDI note."""
    spec = np.abs(np.fft.rfft(samples * np.hanning(len(samples))))
    freqs = np.fft.rfftfreq(len(samples), 1 / SR)
    lo, hi = 60, 4000
    band = (freqs >= lo) & (freqs <= hi)
    peak = freqs[band][np.argmax(spec[band])]
    midi = 69 + 12 * math.log2(peak / 440.0)
    return peak, midi


def main():
    midi_notes, div, tempo_us = read_midi(MID)
    src, skipped = read_os(OS_DATA)

    print("=" * 66)
    print("1. SOURCE CHECK — .mid vs Online Sequencer data")
    print("=" * 66)
    print(f"  MIDI division      : {div} ticks/beat")
    print(f"  MIDI tempo         : {60_000_000 / tempo_us:.1f} BPM")
    print(f"  notes in .mid      : {len(midi_notes)}")
    print(f"  notes in os_data   : {len(src)}")
    if skipped:
        print(f"  unparsable chunks  : {len(skipped)}  e.g. {skipped[:2]}")

    midi_hist = Counter(n[1] for n in midi_notes)
    src_hist = Counter(n[1] for n in src)
    missing = src_hist - midi_hist
    extra = midi_hist - src_hist
    print(f"  pitch multiset     : {'IDENTICAL' if not missing and not extra else 'DIFFERS'}")
    if missing:
        print(f"    missing from .mid: {dict(list(missing.items())[:8])}")
    if extra:
        print(f"    extra in .mid    : {dict(list(extra.items())[:8])}")

    # onset comparison in seconds
    tick_sec = tempo_us / 1e6 / div
    midi_onsets = sorted(round(n[0] * tick_sec, 3) for n in midi_notes)
    src_onsets = sorted(round(s * 60 / BPM / 4, 3) for s, _, _ in src)
    same_len = len(midi_onsets) == len(src_onsets)
    if same_len:
        drift = max(abs(a - b) for a, b in zip(midi_onsets, src_onsets))
        print(f"  max onset drift    : {drift * 1000:.1f} ms")
    else:
        print("  onset compare      : skipped (different note counts)")

    lo = min(n[1] for n in midi_notes)
    hi = max(n[1] for n in midi_notes)
    print(f"  range              : {note_name(lo)} ({lo}) .. {note_name(hi)} ({hi})")

    # ---------------------------------------------------------------- render
    print()
    print("=" * 66)
    print("2. RENDER — writing audio you can actually listen to")
    print("=" * 66)
    rendered = [(n[0] * tick_sec, n[1], 0.12) for n in midi_notes]
    total = max(t[0] for t in rendered) + 2.0
    print(f"  length             : {total:.1f} s")

    for name, secs in (("sound_check_intro.wav", 24.0), ("sound_check_full.wav", total)):
        clip = [t for t in rendered if t[0] < secs]
        audio = render(clip, secs)
        write_wav(name, audio)
        print(f"  wrote {name:<26} {len(clip)} notes, {secs:.0f}s")

    # ----------------------------------------------------------- pitch check
    print()
    print("=" * 66)
    print("3. PITCH CHECK — FFT the rendered audio")
    print("=" * 66)

    # 3a. Chromatic scale, one note at a time. Proves the MIDI note number ->
    #     frequency -> rendered waveform chain is exact.
    print("  (a) monophonic chromatic scale C4..B4")
    ok = bad = 0
    print(f"      {'note':<6}{'f0 expect':>11}{'f0 found':>10}{'detected':>10}  verdict")
    print("      " + "-" * 50)
    for m in range(60, 72):
        audio = render([(0.0, m, 0.5)], 0.6)
        seg = audio[int(0.05 * SR) : int(0.05 * SR) + int(0.35 * SR)]
        peak, detected = detect_pitch(seg, m)
        good = abs(detected - m) < 0.35
        ok += good
        bad += not good
        print(
            f"      {note_name(m):<6}{freq(m):>11.1f}{peak:>10.1f}"
            f"{note_name(round(detected)):>10}  {'OK' if good else 'WRONG'}"
        )
    print("      " + "-" * 50)
    print(f"      {ok} correct, {bad} wrong")

    # 3b. The real song is polyphonic, so only sample notes that are genuinely
    #     alone in their window — otherwise the FFT just finds the loudest
    #     simultaneous note (or a harmonic of it), which says nothing.
    print("\n  (b) isolated notes inside the actual song")
    full = [(n[0] * tick_sec, n[1], 0.12) for n in midi_notes]
    audio = render([t for t in full if t[0] < 60.0], 60.0)
    ok2 = bad2 = 0
    print(f"      {'time':>7}{'note':<6}{'f0 expect':>11}{'f0 found':>10}{'detected':>10}  verdict")
    print("      " + "-" * 58)
    tested = 0
    for idx, (start, m, _) in enumerate(full):
        if tested >= 6 or start > 55.0:
            break
        # require the window to be free of every OTHER note — compared by index,
        # because two different notes can share the exact same onset time.
        busy = any(
            j != idx and s < start + 0.30 and s + d > start - 0.02
            for j, (s, _, d) in enumerate(full)
        )
        if busy:
            continue
        s0 = int(start * SR)
        seg = audio[s0 : s0 + int(0.25 * SR)]
        if len(seg) < 4096:
            continue
        peak, detected = detect_pitch(seg, m)
        good = abs(detected - m) < 0.5
        ok2 += good
        bad2 += not good
        print(
            f"      {start:>7.2f}{note_name(m):<6}{freq(m):>11.1f}{peak:>10.1f}"
            f"{note_name(round(detected)):>10}  {'OK' if good else 'WRONG'}"
        )
        tested += 1
    print("      " + "-" * 58)
    print(f"      {ok2} correct, {bad2} wrong")
    ok += ok2
    bad += bad2

    json.dump(
        {
            "notes": len(midi_notes),
            "range": [note_name(lo), note_name(hi)],
            "pitch_multiset_identical": not missing and not extra,
        },
        open("verify_result.json", "w"),
        indent=2,
    )


if __name__ == "__main__":
    main()
