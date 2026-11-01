import struct


def write_varlen(x):
    out = b""
    while x > 0x7F:
        out = bytes([0x80 | (x & 0x7F)]) + out
        x >>= 7
    return bytes([x & 0x7F]) + out


def make_midi(name, notes, div=480):
    """notes: list of (start_tick, midi_note, dur_tick)."""
    ev = []
    for s, n, d in notes:
        ev.append((s, 0x90, n, 90))
        ev.append((s + d, 0x80, n, 0))
    ev.sort(key=lambda e: (e[0], e[1]))
    trk = b""
    t = 0
    for s, st, n, v in ev:
        trk += write_varlen(s - t) + bytes([st, n, v])
        t = s
    trk += write_varlen(0) + bytes([0xFF, 0x2F, 0, 0])
    with open(name, "wb") as f:
        f.write(b"MThd" + struct.pack(">IHHH", 6, 0, 1, div))
        f.write(b"MTrk" + struct.pack(">I", len(trk)) + trk)
    print("wrote", name)


if __name__ == "__main__":
    # C4..C5 chromatic (MIDI 60..72)
    make_midi("chromatic_test.mid", [(i * 100, 60 + i, 80) for i in range(13)])
