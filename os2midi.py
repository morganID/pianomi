#!/usr/bin/env python3
"""
Convert Online Sequencer note data into a Standard MIDI File (Type 0).

Input format (as copied from the editor / share string):
    Online Sequencer:<id>:<start> <note> <dur> <inst> <vol>;<pos> <note> <dur> <inst> <vol>;...

Each note token is:  <pos> <NOTE> <duration> <instrument> <volume>
  - pos/duration are in sequencer "steps"
  - NOTE is like C5, C#5, A#4, F#6
  - instrument is ignored (everything goes to channel 0, piano)
  - volume 0..1 maps to MIDI velocity 0..127

Assumptions (no BPM is stored in the data):
  - Tempo passed on the command line (default 125 BPM — the song's real tempo)
  - 1 step = 1 sixteenth note -> 120 ticks per step at PPQ 480
    (Online Sequencer's grid unit is a 1/4 beat; using an eighth note here
     makes everything play back at half speed.)
  - Notes with duration <= 0 are skipped.
"""
import re
import struct
import sys

PPQ = 480
TICKS_PER_STEP = PPQ // 4  # sixteenth note (1/4 beat)

NOTE_INDEX = {
    'C': 0, 'C#': 1, 'D': 2, 'D#': 3, 'E': 4, 'F': 5,
    'F#': 6, 'G': 7, 'G#': 8, 'A': 9, 'A#': 10, 'B': 11,
}

# Matches a note token; the leading <pos> is optional (first note reuses the
# header's trailing number, which the regex still captures as pos).
TOKEN = re.compile(
    r'(?:(\d+(?:\.\d+)?)\s+)?'          # optional position
    r'([A-G]#?\d)\s+'                   # note name
    r'(\d+(?:\.\d+)?)\s+'               # duration
    r'(\d+)\s+'                         # instrument
    r'(\d+(?:\.\d+)?)'                  # volume
)


def note_to_num(s: str) -> int:
    sharp = s[1] == '#'
    name = s[:2] if sharp else s[0]
    octv = int(s[2:] if sharp else s[1:])
    return (octv + 1) * 12 + NOTE_INDEX[name]


def vlq(n: int) -> bytes:
    out = bytearray([n & 0x7F])
    n >>= 7
    while n:
        out.insert(0, (n & 0x7F) | 0x80)
        n >>= 7
    return bytes(out)


def main() -> None:
    if len(sys.argv) < 3:
        print("usage: os2midi.py <input.txt> <output.mid> [bpm]", file=sys.stderr)
        sys.exit(1)
    infile, outfile = sys.argv[1], sys.argv[2]
    bpm = float(sys.argv[3]) if len(sys.argv) > 3 else 125.0
    us_per_beat = int(round(60_000_000 / bpm))

    raw = open(infile, encoding='utf-8', errors='ignore').read()
    notes = []
    for m in TOKEN.finditer(raw):
        pos_s, note, dur_s, _inst, vol_s = m.groups()
        pos = float(pos_s) if pos_s is not None else 0.0
        dur = float(dur_s)
        vol = float(vol_s)
        if dur <= 0:
            continue
        try:
            nn = note_to_num(note)
        except Exception:
            continue
        vel = max(1, min(127, int(round(vol * 127))))
        start = int(round(pos * TICKS_PER_STEP))
        length = max(int(round(dur * TICKS_PER_STEP)), 1)
        notes.append((start, nn, length, vel))

    if not notes:
        print("no notes found in input", file=sys.stderr)
        sys.exit(1)

    events = []
    for start, nn, length, vel in notes:
        events.append((start, 0x90, nn, vel))            # note on (ch 0)
        events.append((start + length, 0x80, nn, 0))     # note off (ch 0)
    events.sort(key=lambda e: (e[0], e[1]))

    track = bytearray()
    track += bytes([0x00, 0xFF, 0x51, 0x03]) + us_per_beat.to_bytes(3, 'big')
    track += bytes([0x00, 0xC0, 0x00])  # program change -> acoustic grand piano
    prev = 0
    for t, status, nn, vel in events:
        track += vlq(t - prev)
        prev = t
        track += bytes([status, nn, vel])
    track += bytes([0x00, 0xFF, 0x2F, 0x00])

    out = (b'MThd' + struct.pack('>IHHH', 6, 0, 1, PPQ)
           + b'MTrk' + struct.pack('>I', len(track)) + track)
    with open(outfile, 'wb') as f:
        f.write(out)
    print(f"OK: {outfile}  |  {len(notes)} notes  |  BPM {bpm}  |  PPQ {PPQ}")


if __name__ == '__main__':
    main()
