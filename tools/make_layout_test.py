#!/usr/bin/env python3
"""Build a short listening test that proves whether the key mapping is correct.

Writes `layout_test.mid` (play it through the app in Roblox) and
`layout_test.wav` (the same thing rendered here, so you know what it MUST
sound like).

Three parts, each with a clear pass/fail criterion:

  1. C major chord  C4-E4-G4     -> must sound like ONE clean major chord
  2. chromatic run  C4..C5       -> must sound like a smooth staircase
  3. low octave     C3-D3-E3     -> must sound like the same chord shape, lower

If part 1 is clean but part 2 lurches around, the white keys are right and the
BLACK keys are mapped to the wrong octave.
"""

import struct
import wave

import numpy as np

SR = 44100
BPM = 125
PPQ = 480
TICKS_PER_STEP = PPQ // 4  # one 1/16 note

NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


def nm(n):
    return f"{NAMES[n % 12]}{n // 12 - 1}"


def freq(n):
    return 440.0 * 2 ** ((n - 69) / 12.0)


def vlq(n):
    out = bytearray([n & 0x7F])
    n >>= 7
    while n:
        out.insert(0, (n & 0x7F) | 0x80)
        n >>= 7
    return bytes(out)


def write_midi(path, notes):
    """notes: list of (start_step, midi, dur_steps)."""
    ev = []
    for start, note, dur in notes:
        # steps -> ticks (one step is a 1/16 note)
        ev.append((start * TICKS_PER_STEP, 0x90, note, 100))
        ev.append(((start + dur) * TICKS_PER_STEP, 0x80, note, 0))
    ev.sort(key=lambda e: (e[0], e[1]))

    track = bytearray()
    track += bytes([0x00, 0xFF, 0x51, 0x03])
    track += (60_000_000 // BPM).to_bytes(3, "big")
    track += bytes([0x00, 0xC0, 0x00])
    prev = 0
    for tick, status, note, vel in ev:
        track += vlq(tick - prev)
        prev = tick
        track += bytes([status, note, vel])
    track += bytes([0x00, 0xFF, 0x2F, 0x00])

    out = bytearray(b"MThd" + (6).to_bytes(4, "big"))
    out += (0).to_bytes(2, "big") + (1).to_bytes(2, "big") + PPQ.to_bytes(2, "big")
    out += b"MTrk" + len(track).to_bytes(4, "big") + track
    open(path, "wb").write(out)
    print(f"wrote {path}  ({len(notes)} notes)")


def render(notes, secs):
    n = int(SR * secs) + SR
    buf = np.zeros(n)
    for start, midi, dur in notes:
        f0 = freq(midi)
        s0 = int(start * SR)
        s1 = min(n, s0 + int((dur + 0.4) * SR))
        t = np.arange(s1 - s0) / SR
        env = np.exp(-t * 2.8) * np.minimum(1.0, t / 0.006)
        w = sum(a * np.sin(2 * np.pi * f0 * h * t)
                for h, a in ((1, 1.0), (2, 0.4), (3, 0.2), (4, 0.1)))
        buf[s0:s1] += w * env * 0.12
    return buf / (np.max(np.abs(buf)) or 1) * 0.9


def write_wav(path, samples):
    pcm = (np.clip(samples, -1, 1) * 32767).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(pcm.tobytes())
    print(f"wrote {path}")


def main():
    sec_per_step = 60 / BPM / 4
    notes = []  # (start_step, midi, dur_steps)

    def add(start_s, midi, dur_s):
        notes.append((int(round(start_s / sec_per_step)), midi, max(1, int(round(dur_s / sec_per_step)))))

    # 1. C major chord, held
    for m in (60, 64, 67):
        add(0.0, m, 1.6)

    # 2. chromatic run C4 -> C5, one note every 0.36 s
    for i, m in enumerate(range(60, 73)):
        add(2.4 + i * 0.36, m, 0.30)

    # 3. low octave C3-E3-G3
    for m in (48, 52, 55):
        add(8.0, m, 1.6)

    write_midi("layout_test.mid", notes)

    audio = render([(s * sec_per_step, m, d * sec_per_step) for s, m, d in notes], 10.5)
    write_wav("layout_test.wav", audio)

    print()
    print("Part 1  t=0.0s  chord C4-E4-G4      -> listen for ONE clean major chord")
    print("Part 2  t=2.4s  chromatic C4..C5    -> listen for a smooth staircase")
    print("Part 3  t=8.0s  chord C3-E3-G3      -> same chord shape, one octave down")
    print()
    print("Part 2 note sequence:", " ".join(nm(m) for m in range(60, 73)))


if __name__ == "__main__":
    main()
