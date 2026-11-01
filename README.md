# 🎹 Pianomi

**Pianomi** reads a MIDI file and plays it into a Roblox piano — either as
keyboard input (with a **humanization layer** so it sounds played by a person
rather than a robot) or as **real MIDI messages** to any destination you like.

```
MIDI file ──▶ parser ──▶ key mapper ──▶ humanizer ──▶ ┌ enigo   ──▶ Roblox
                                                       └ MIDI out ──▶ DAW / synth
```

Ships as a **CLI** and a **macOS menu bar app**.

## ✨ Features

- **MIDI parsing** — tempo map, tracks, notes, velocity and duration
  (`midly`), with full score metadata (format, time & key signatures, tempo
  range, per-track summary).
- **Key mapping** — configurable MIDI-note → keyboard-key layout, all 88 keys
  of the in-game piano via plain / Shift / Ctrl layers.
- **MIDI output** — publish a virtual MIDI port or connect to an existing
  destination; no keyboard, no Accessibility permission (`midir`).
- **Humanization engine** — timing jitter, duration jitter, velocity-based
  holding and chord arpeggiation (`rand`).
- **Real-time playback** — cross-platform key simulation (`enigo`, supports
  Windows / macOS / Linux).
- **Flexible config** — TOML file for the mapping and every humanize/playback
  parameter (`serde` + `toml`).
- **Dry-run mode** — print the whole event schedule without touching anything,
  instantly.
- **Loop, channel filter, transpose, speed** from the CLI.
- **Key-coverage report** — tells you up front whether a file is playable with
  your mapping, and which pitches are not.

## 🚀 Build

```bash
cargo build --release
# binaries: target/release/pianomi  and  target/release/pianomi-gui
```

Requires Rust 1.70+. On **macOS** you must grant the terminal / built binary
*Accessibility* permission (System Settings → Privacy & Security →
Accessibility) or `enigo` cannot send key events. This is only needed for the
keyboard path — `--midi-out` works without it.

### macOS menu bar app

```bash
./install-app.sh        # build, refresh the bundle, install to /Applications
```

`build-app.sh` refreshes the local `Pianomi.app`; `install-app.sh` also stops
any running copy and installs to `/Applications`. The bundle is signed with a
real code-signing identity when one is available, so the Accessibility grant
survives rebuilds (an ad-hoc signature would reset it on every build).


## 🎮 Usage

```bash
# Play a song with medium humanization
pianomi play -f song.mid --humanize 50

# Play twice as fast, only channel 1, 3 loops
pianomi play -f song.mid -s 2.0 --channel 1 --loops 3

# Preview the schedule without pressing anything
pianomi play -f song.mid --dry-run

# Inspect a file
pianomi info -f song.mid

# …and dump the full tempo / metre / key change tables
pianomi info -f song.mid --verbose

# Show the resolved key → note mapping
pianomi list-mappings

# List MIDI destinations this machine exposes
pianomi midi-ports

# Send real MIDI instead of keystrokes (publishes a virtual port)
pianomi play -f song.mid --midi-out

# …or connect to an existing destination by name
pianomi play -f song.mid --midi-out "IAC Driver Bus 1"

# Dump the built-in default config to start customizing
pianomi print-config > my-config.toml
pianomi play -f song.mid --config my-config.toml
```

### Subcommands

| Command | Purpose |
| --- | --- |
| `play` | Play a MIDI file into the focused window |
| `info` | Print MIDI metadata: format, timing, per-track summary, tempo range, time signatures, key signature, length, note range, key coverage (`--verbose` adds change tables) |
| `list-mappings` | Print the key → note mapping from the config |
| `midi-ports` | List the MIDI destinations available for `play --midi-out` |
| `print-config` | Print the embedded default TOML config |

### `play` options

| Flag | Default | Meaning |
| --- | --- | --- |
| `-f/--file` | — | MIDI file path (required) |
| `--humanize` | `50` | Humanize level 0-100 |
| `-s/--speed` | `1.0` | Speed multiplier |
| `--config` | built-in | TOML config file |
| `--channel` | all | Restrict to one MIDI channel (1-16) |
| `--transpose` | `0` | Semitones to shift every note |
| `--loops` | `1` | Repeat count |
| `--dry-run` | off | Print the whole event schedule instead of pressing keys — instant, not real-time |
| `--midi-out [NAME]` | off | Send MIDI instead of keystrokes; no value publishes a virtual port |

## 🎛 MIDI output

Keystrokes are the only thing a Roblox piano understands, but they are a lossy
way to carry music: the in-game piano tops out at 88 keys across three modifier
layers, and nothing downstream can tell which note was which. `--midi-out`
replaces the whole keyboard path with real MIDI messages.

```bash
# Publish a virtual source called "Pianomi"; pick it in any app's MIDI input list
pianomi play -f song.mid --midi-out

# Publish under your own name
pianomi play -f song.mid --midi-out "My Piano"

# Connect to a destination that already exists (IAC bus, synth, DAW input)
pianomi midi-ports
pianomi play -f song.mid --midi-out "IAC Driver Bus 1"
```

Name resolution is exact first, then case-insensitive substring, so
`"IAC Driver Bus 1"` never lands on `"IAC Driver Bus 10"`. A name that matches
nothing is published as a virtual port under that name.

What the receiver gets is the file as written: its own channels, velocities and
tempo map, all 128 pitches, and a real `0x80` note-off per note. On stop or at
the end of playback the app sends All Notes Off / All Sound Off — but only on
the channels it actually used, so it will not silence a DAW's other tracks.

In the menu bar app this is **Output ▸ Keyboard / Virtual MIDI port /
*\<destination\>***, with *Rescan MIDI ports* to refresh the list.

Two limits worth knowing: a virtual port only exists while playback is running
(the receiving app has to be able to see new ports appear, which DAWs handle and
simple listeners may not), and `--dry-run --midi-out` prints the messages
without opening a port.

## 🎹 Default keyboard mapping

The built-in config reproduces the **Virtual Piano** layout: 61 keys spanning
**C2 (36) … C7 (96)**, i.e. the middle five octaves of the in-game 88-key piano.
White keys run in reading order, black keys sit on the row above them:

```
White   1 2 3 4 5 6 7 8 9 0  q w e r t y u i o p  a s d f g h j k l  z x c v b n m
        C2 ...                                   C4 ...                C6 ... C7

Black   ! @   $ % ^     * (    Q W E   T Y   I O P   S D   G H J   L   Z C V   B
        C#2 D#2 F#2 G#2 A#2 C#3 D#3 F#3 G#3 A#3 C#4 D#4 F#4 G#4 A#4 C#5 D#5 F#5 G#5 A#5 C#6 D#6 F#6 G#6 A#6
```

Note the gaps: there is no black key between E–F and B–C, so `#`, `&`, `)` are
*not* bound.

### Extended octaves — Ctrl + key

The in-game piano is a full **88 keys (A0 … C8)**. The 27 keys outside C2…C7 are
drawn dimmed because they need **Ctrl**, walked chromatically over the same 36
QWERTY keys:

```
Ctrl + 1 2 3 4 5 6 7 8 9 0 q w e r t   A0 … B1     (low end)
Ctrl + y u i o p a s d f g h j         C#7 … C8     (high end)
```

Those are *plain* characters, not Shift+key: every semitone gets its own key, so
the black key above `w` is `Ctrl+w`, not `Ctrl+W`.

Together that covers every note, so a song spanning F2–A7 plays in full.

### How sharps are sent (no Shift key)

A black key is the same physical key as the white note below it. Rather than
pressing the real Shift key, the player posts a single event whose *own*
modifier flag is set — `8` with `kCGEventFlagMaskShift` plus the literal
character `"*"`. The app sees a normal `*` keystroke, the system-wide modifier
state is never touched, and there is exactly one event per note (no races at
speed, and no chance of a stray Shift turning a natural into a sharp).

Verify it yourself:

```bash
cargo run --release --example probe_keys
```

### If the game ignores the flag

`player.shift_mode` in the config selects how a sharp is delivered:

| Value | Events per sharp | Use when |
| --- | --- | --- |
| `"char"` (default) | 1 — base keycode + Shift flag + literal character | the game reacts to the typed character |
| `"key"` | 3 — real Shift down, key, Shift up | the game asks its own input state (`UserInputService:IsKeyDown`) |

Both scope the modifier to a single event, so neither can leak into the next
note. The same choice applies to the Ctrl layer.

Edit `config/default.toml` (or your own `--config` file) to match the exact
layout your Roblox piano experience expects. Add more `[[keyboard.keys]]`
entries to extend the range, or set `keyboard.transpose` to shift everything
up/down an octave.

## 🎼 MIDI parsing

`info` is the window into the parser. It reports the SMF format, the timing
base, a per-track summary, every tempo change, the tempo range, the time
signatures, the key signature and the resulting note range:

```
Format          : 1 (parallel tracks)
Timing          : 480 ticks per beat (metrical)
Tracks          : 2
  #1  Track 0 (unnamed)                  note-ons 797    notes 797    ch [1]
  #2  Track 1 (unnamed)                  note-ons 412    notes 412    ch [1]
Notes           : 1209 (1209 note-on events)
Tempo events    : 12
Initial tempo   : 55.00 BPM
Tempo range     : 20.00 .. 70.00 BPM (unrounded: 19.99998 .. 70.00023)
Time signature  : 4/4, 3/4, 2/4 (9 changes)
Key signature   : A major (3 sharps)
Length          : 198.95s (3.32 min)

Mapped range    : 21 (A0) .. 108 (C8)
Key coverage    : 1209/1209 notes (100.0%)
Modifiers       : 879 plain, 318 shift, 12 ctrl
```

`--verbose` appends the tick/time/BPM table for every tempo change, plus the
time-signature and key-signature change tables.

The **key coverage** block answers the question the parse alone cannot: can the
configured mapping actually play this file? It counts how many notes fall
inside the mapped range, how many need Shift (black keys) or Ctrl (the outer
octaves), and lists any pitch that has no key at all. `--config` points the
report at a different mapping.

The parser handles the cases that make naive implementations sound wrong:

- **Tempo at tick 0 wins.** The tempo map is not pre-seeded in a way that
  discards the file's own first tempo event; when several events land on the
  same tick the last one wins, matching sequencer behaviour.
- **FIFO note pairing.** The same pitch re-struck while the previous instance
  is still held is paired first-on → first-off, so overlapping notes keep their
  own durations instead of swapping them.
- **Note-on with velocity 0** is treated as a note-off, not as a zero-length
  note.
- **Dangling note-ons** (truncated files, or sequencers that rely on
  end-of-track) are closed at the end of their track instead of being dropped.
- **SMPTE timing** is converted once (`1 / (ticks_per_frame × fps)` µs per
  tick) rather than being divided by the tick count twice.
- **Unrounded tempo.** A tempo stored as `3000003` µs/beat is `19.99998` BPM;
  sites that truncate report it as `19`. The unrounded pair is printed so the
  difference is explainable rather than looking like a parse error.

## 🤖 Humanization

Every effect scales with `--humanize` (0 = perfectly robotic, 100 = full
variation defined in the config):

- `timing_variation_ms` — random ±offset on each note start.
- `duration_variation_ms` — random ±offset on each note length.
- `use_velocity_for_duration` — louder MIDI notes are held a bit longer.
- `chord_delay_ms` — simultaneous notes are staggered so chords don't all
  strike on the same tick.

## ⚠️ Important notes

- **Respect Roblox's Terms of Service.** Use this for learning, practice, or
  private servers only — don't use it to cheat in competitive/public experiences.
- **Focus the window.** `enigo` cannot switch windows for you. Click the Roblox
  piano window before playback; `player.start_delay_ms` (default 3s) gives you
  time.
- **Calibrate latency.** If notes feel early/late, adjust `tick_ms`,
  `min_hold_ms` or add a lead-in delay to taste.
- **macOS** needs Accessibility permission for the binary that sends keys.

## 🧩 Project layout

```
pianomi/
├── Cargo.toml
├── src/
│   ├── main.rs          # CLI entry point & subcommands
│   ├── gui.rs           # macOS menu-bar app
│   ├── midi_parser.rs   # MIDI → notes + score metadata (midly)
│   ├── midi_out.rs      # MIDI output: virtual port / destination (midir)
│   ├── key_mapper.rs    # note → key mapping
│   ├── keycode.rs       # per-key CGEvent posting (macOS)
│   ├── humanizer.rs     # humanization layer
│   ├── player.rs        # real-time playback
│   ├── perm.rs          # Accessibility permission checks
│   └── config.rs        # TOML config loading
├── config/
│   └── default.toml     # default mapping & settings
├── examples/            # probe_keys, midi_probe, midi_sink, dump_notes, …
└── README.md
```

## 🛠️ Possible extensions

1. Terminal real-time preview of the notes being played.
2. Recording / replay analysis.
3. Auto-calibration by measuring in-game round-trip latency.
4. Windows / Linux menu bar (or tray) build of the GUI.
