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

The menu bar app never takes focus away from the game: tao's launch-time
activation is disabled, and after a menu click or the Open MIDI dialog focus is
handed back to the app that was frontmost before. Keyboard playback keeps the
verified system-wide HID path by default. **Output ▸ Direct to Roblox (no focus
needed)** is an optional toggle: it posts straight to the Roblox process
(`CGEventPostToPid`), so notes land even when another app is active and never
leak into a text field. Targeted events are now stamped with real, strictly
increasing timestamps (see below), which fixes the coalescing/delay they used
to cause; it is still off by default because Roblox's run loop has to drain
those events, making the HID path the more predictable timing reference.
Humanize starts at **0%** (exact playback); pick a level under **Humanize** to
opt in.


## 🎮 Usage

```bash
# Play a song exactly as written (humanize defaults to 0)
pianomi play -f song.mid

# Opt in to medium humanization
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
| `--humanize` | `0` | Humanize level 0-100 (0 = exactly as written) |
| `-s/--speed` | `1.0` | Speed multiplier |
| `--config` | built-in | TOML config file |
| `--channel` | all | Restrict to one MIDI channel (1-16) |
| `--transpose` | `0` | Semitones to shift every note |
| `--loops` | `1` | Repeat count |
| `--shift-mode` | from config (`key`) | `key` = real Shift per note, `char` = Shift flag + typed character only |
| `--max-hold-ms [MS]` | `0` (off) | Keyboard only: shorten notes longer than N ms. **Starts never move**, only releases are pulled in. Try `300`–`500` for dense songs |
| `--retrigger-gap-ms [MS]` | `20` | Keyboard only: real key-up time between a release and the next press on the **same physical key**. Prevents the game from seeing one unbroken hold when two notes share `1`/`!`/`Ctrl+1`. Onsets never move; `0` restores back-to-back releases |
| `--same-key-gap-ms [MS]` | `30` | Gap between two *different pitches* that need one physical key on the same tick. Raised to `min_hold_ms + retrigger_gap_ms`; `0` drops the second |
| `--target-roblox` | off | macOS: post key events **only to the running Roblox.app process** (`CGEventPostToPid`) instead of system-wide — no focus stealing, nothing leaks into other apps |
| `--targeted-extra-gap-ms [MS]` | `30` | Direct mode only: extra key-up margin added to `--retrigger-gap-ms` because targeted events are drained in frame-sized batches (so 20 + 30 = 50 ms). Onsets never move; `0` disables the widening for an A/B comparison |
| `--target-pid [PID]` | off | **Debug:** post events to any process instead of the HID tap, e.g. a local event receiver. macOS only; implies the Direct-mode margins |
| `--trace` | off | **Debug:** write a per-event trace (scheduled vs actual time, gaps, delivery target, warnings) to `~/Library/Logs/Pianomi/trace.log`. Also `PIANOMI_TRACE=1` |
| `--trace-log [PATH]` | `~/Library/Logs/Pianomi/trace.log` | **Debug:** trace destination (implies `--trace`) |
| `--dry-run` | off | Print the whole event schedule instead of pressing keys — instant, not real-time. Combined with `--target-roblox` it previews the Direct-mode margins |
| `--midi-out [NAME]` | off | Send MIDI instead of keystrokes; no value publishes a virtual port |

## 🎯 Scoping playback to Roblox.app

By default key events are posted to the **global HID tap**: the Roblox window
must be focused, and a real Shift press is visible to every app on the Mac.

`--target-roblox` switches delivery to `CGEventPostToPid()`. Pianomi looks up
the running `RobloxPlayer` PID and posts every event straight into that
process's event queue:

```bash
pianomi play -f song.mid --target-roblox
```

- No focus stealing, no keystrokes leaking into other apps.
- The Roblox PID is re-resolved during playback (Roblox can quit and relaunch
  on a teleport or tray restart). PID lookups run on a **detached watcher
  thread**: `pgrep` costs ~25 ms (up to 62 ms measured), so doing it on the
  playback thread produced an audible tempo hiccup. The watcher uses a
  `kill(pid, 0)` liveness probe instead and only spawns `pgrep` when Roblox
  actually died.
- **Every targeted event is timestamped.** `CGEventCreateKeyboardEvent` leaves
  synthetic events with a timestamp of **0** (verified for `Private`,
  `CombinedSessionState` and `HIDSystemState` sources). `CGEventPost(HID)`
  gets away with it because the WindowServer stamps events entering the HID
  stream; `CGEventPostToPid` hands the event to the target's queue as-is. A
  queue full of zero-timestamp events gets coalesced, held or reordered by the
  receiving run loop — that was the "some notes are delayed" symptom. Pianomi
  now stamps each targeted event with a fresh, strictly increasing
  nanoseconds-since-boot clock (`CGEventSetTimestamp` + the mach timebase)
  before posting. The system-wide HID branch is deliberately left untouched.
- If Roblox disappears, events are dropped rather than falling back to the
  global tap, so keys never spray into other apps.
- Requires `/Applications/Roblox.app` to be running, and still needs
  Accessibility permission for the launching binary.
- It remains **off by default**. Targeted events still have to be drained by
  Roblox's own run loop, so the system-wide HID path is the timing reference;
  turn this on only when you specifically need to play while another app is
  frontmost.
- **It is still keyboard input.** This scoping changes *where* the event goes,
  not *what* the game can receive. The Virtual Piano still has 36 physical keys
  reused across plain / Shift / Ctrl layers, no note-off, no velocity and no
  sustain. Note loss from shared physical keys is solved by the scheduler
  (above), not by the transport.
- Untested caveat: `UserInputService:IsKeyDown(LeftShift)` may read global
  modifier state that a PID-targeted event does not update. If sharps come out
  as naturals, try `--shift-mode char` or drop `--target-roblox`.

### Direct mode: why some notes used to be skipped

Targeted delivery was verified to be lossless at the OS level — an AppKit
receiver got **100% of the posted events**, even while completely unfocused.
The loss happens one layer up: `CGEventPost(HID)` changes the modifier/key state
synchronously, whereas `CGEventPostToPid` hands events to Roblox's run loop,
which **drains them in frame-sized batches**. The Virtual Piano polls
`IsKeyDown` once per frame. If a key-up and the following key-down land in the
same drain, the game never observes the key going up: it sees one unbroken hold
and never fires `InputBegan` for the second note. That is the "ada nada yang
di-skip" symptom — timing, not missing events.

So Direct mode automatically widens the gap that the game can actually see:

| Margin | HID (default) | Direct to Roblox |
| --- | --- | --- |
| key-up before the next press on one physical key | 20 ms | **50 ms** (20 + 30, linear) |
| two pitches sharing one key on the same tick | 30 ms, raised to `min_hold_ms + retrigger_gap_ms` (= 50 ms) | 60 ms floor, raised to `min_hold_ms + retrigger_gap_ms` (**= 80 ms** with defaults) |

`targeted_extra_gap_ms` (default 30) controls the widening and is **linear**:
`--targeted-extra-gap-ms 10` gives 30 ms, `0` gives the stock 20 ms for a clean
A/B. As always, **only releases move** — every note onset stays exactly as
written, and no note is shortened below `min_hold_ms`. MIDI output is unaffected.
`--dry-run` posts nothing, but it *does* preview the Direct-mode schedule when
`--target-roblox`/`--target-pid` is also given, which is the cheapest way to
inspect the wider margins. The CLI prints the effective numbers, and playback
logs a `Direct mode : …` line with the margins actually used.

#### Measured delivery jitter

A local AppKit receiver was posted a `down → up → gap → down` pattern through
`CGEventPostToPid` (12 repetitions per requested gap, 100 ms hold). Every event
arrived (336/336), but the gap the receiver actually observed was not the gap
that was posted:

| Requested key-up gap | Observed min | Observed median | Observed max | Notes |
| --- | --- | --- | --- | --- |
| 0 ms | 0.13 ms | 0.15 ms | 0.22 ms | — |
| 5 ms | 0.15 ms | 11.5 ms | 16.7 ms | **1 of 12 collapsed to ~0.1 ms** |
| 10 ms | 10.7 ms | 12.5 ms | 17.7 ms | still inside one 60 Hz frame |
| 20 ms | 15.1 ms | 23.8 ms | 30.6 ms | **can shrink below one frame** |
| 30 ms | 29.5 ms | 33.7 ms | 35.7 ms | ~2 frames |
| 50 ms | 50.3 ms | 53.0 ms | 58.6 ms | ~3 frames, never collapsed |
| 80 ms | 78.0 ms | 84.0 ms | 87.4 ms | — |

So the queued path adds a few ms of latency plus jitter, and small gaps can be
swallowed outright. A 20 ms key-up — fine on the synchronous HID path — is
marginal here, which is exactly why the same song can play cleanly system-wide
and lose notes in Direct mode. 50 ms is the smallest value that stayed above
three 60 Hz frames in every sample.

Widening further is **not** free, though, because the extra key-up has to fit
inside the gap between two existing onsets. Sweeping the flag over the same
dense file (`【Animenz】吉卜力组曲.mid`, 5977 notes) makes the trade-off visible:

| `--targeted-extra-gap-ms` | Effective key-up | Pairs that no longer fit |
| --- | --- | --- |
| 0 | 20 ms | 2 |
| 10 | 30 ms | 8 |
| 20 | 40 ms | 28 |
| **30** (default) | **50 ms** | **31** |
| 50 | 70 ms | 80 |

Those counts are pairs where the next onset arrives before the key-up window
can be honoured — the run prints them as a `warning:`. So the margin buys
queue safety at the cost of truncating more short holds, and the default 30 ms
sits right before the knee of the curve: one more 20 ms step roughly triples
the damage. Lower `min_hold_ms` if a particular file starts sounding clipped
rather than skipped.

If notes are still skipped in Direct mode, the fastest isolation is
`--target-roblox --targeted-extra-gap-ms 0` vs `--target-roblox`: if only the
latter is clean, the queue-drain theory is confirmed for that song. Very dense
files may also need a smaller `min_hold_ms` (30 by default) — the run reports
any release/press pair it could not fully separate as a `warning:` line rather
than dropping the note silently.

### Why not "just send digital note data"?

A live Roblox experience exposes no MIDI port, socket or external note API.
Luau runs in a sandbox fed by `UserInputService`; there is nothing outside the
client to write note-on/note-off into. Sending real note data (exact durations,
velocity, sustain, unlimited polyphony) is only possible if you **own the
place**: add a Script that receives a data channel and calls the piano's own
sound function directly. For a third-party experience like Virtual Piano there
is no supported bridge — the honest ceiling for this tool is keyboard events
(with `--midi-out` available when the target is a DAW/synth instead of Roblox).

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
*\<destination\>***, with *Rescan MIDI ports* to refresh the list. The keyboard
branch additionally has **Direct to Roblox (no focus needed)** (off by default),
which targets the Roblox process instead of the active window.

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

### How sharps are sent

A black key is the same physical key as the white note below it, so the game has
to be told a sharp was meant. There are two ways to say it, and which one works
depends on how the experience reads input.

Roblox's Virtual Piano reads `UserInputService:IsKeyDown(LeftShift)` — the
**global** modifier state. An event flag is invisible to that call, so the real
Shift key has to go down. The default (`shift_mode = "key"`) posts Shift down,
the key, Shift up, scoped to that one note, and *also* stamps the Shift flag and
the typed character on the key event itself so a game that reads either of those
still works.

Getting this wrong is subtle and it used to be the default bug: with the flag
alone the app *looks* like it is pressing `Shift+P`, but the game sees a plain
`P`, so **every sharp plays as the natural a semitone below**. In a piece in
B♭ major that rewrites every E♭, B♭ and A♭, and turns every minor chord into a
major one — which is exactly what "sounds nothing like the original" means.

Check what actually reaches the OS:

```bash
cargo run --release --example release_probe          # key mode
cargo run --release --example release_probe char     # flag-only, for comparison
```

It installs an event tap and reports, for every key-down, whether a real Shift
was held at that instant:

```text
modifier (FlagsChanged) events seen: 36
key-downs with a real Shift held:    4   <- IsKeyDown sees these
key-downs with the Shift flag only:  0
key-downs with a real Ctrl held:     5   <- IsKeyDown sees these
```

In `char` mode the first count is `0` and the second is `4`: the flag is set,
the global state never changes.

| `player.shift_mode` | What it posts | Use when |
| --- | --- | --- |
| `"key"` (default) | real Shift down, key, Shift up — plus the flag and the character | the game asks `IsKeyDown` (Roblox Virtual Piano) |
| `"char"` | one event: base keycode + Shift flag + typed character, no real modifier | the game reacts to the typed character only |

Override it per run without touching the config:

```bash
pianomi play -f song.mid --shift-mode key
pianomi play -f song.mid --shift-mode char
```

The startup banner prints which one is active:

```text
Sharp keys   : key — real Shift per note (IsKeyDown)
```

Both modes scope the modifier to a single event, so neither can leak into the
next note. The same choice applies to the Ctrl layer.

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

Every effect scales with `--humanize` (0 = exactly as written, 100 = full
variation defined in the config). The default is **0**, in both the CLI and the
menu bar app, so the first playback is always faithful to the MIDI file —
humanization is strictly opt-in:

- `timing_variation_ms` — random ±offset on each note start.
- `duration_variation_ms` — random ±offset on each note length.
- `use_velocity_for_duration` — softer notes are held a little less. This only
  ever *shortens*: a note that outlasts the next one on the same key could not
  be retriggered, so the next note would be lost.
- `chord_delay_ms` — simultaneous notes are staggered so chords don't all
  strike on the same tick.

### No note is ever dropped

A key can only sound one note at a time, and files routinely violate that
assumption — a sustain-pedal artefact, a note exported with an overlong
duration, or the same pitch note-on'd three times on one tick. All of those
used to make the *next* note on that key silently disappear.

**The keyboard layout makes this worse than it looks.** A sharp is the *same
physical key* as the white note below it (`!` = Shift+`1`), and the outer
octaves reuse those keys again with Ctrl (`Ctrl+1`). So a long-held C2 blocks
not just the next C2 but also C#2 and A0 — notes of completely different
pitches. On a dense arrangement that can cost hundreds of notes.

The player therefore normalises per **physical key** before it runs:

- note-ons for the same physical key on the same millisecond collapse into one
  press (two pitches on one key at one instant is genuinely impossible — the
  run reports these);
- a note that would still be held when the next one starts has its release
  pulled back to `retrigger_gap_ms` (default 20) **before** that start.

That last detail matters. Releasing at *exactly* the next note's start puts the
key-up and the next key-down in one scheduler pass, back-to-back with no real
time between them. Roblox samples `IsKeyDown` once per frame, so it can see only
"still down" and never fires `InputBegan` for the second note — the key is
logically free but musically lost. 20 ms is just over one 60 Hz frame. The
release is clamped at `start + min_hold_ms` so no note becomes too short to
register; if the two onsets are too close for both a full hold and a key-up
window, playback reports the count as a `warning: N release/press pair(s)` line
so it never fails quietly.

Only the *end* of a note is ever changed. Note starts are copied verbatim, so
timing stays exactly as written. For very dense passages you can additionally
shorten long holds with `--max-hold-ms 300` (or `player.max_hold_ms` in the
config); again, only releases move. MIDI output ignores both the cap and the
retrigger gap and always sends the written duration. In Direct to Roblox mode
the gap is widened automatically — see
[Direct mode: why some notes used to be skipped](#direct-mode-why-some-notes-used-to-be-skipped).

If anything still cannot be retriggered, playback ends with a
`warning: N note(s) could not be retriggered` line instead of failing quietly.
`--dry-run` reports the same thing, so you can check a file before playing it.

## 🔍 Debugging playback

A menu bar bundle launched with `open` has **stdout and stderr attached to
`/dev/null`** — every warning the player produced was invisible. Debug logging
writes to a file instead:

- GUI: **Debug ▸ Log playback to file**, then **Debug ▸ Reveal Debug Log**.
- CLI: `--trace`, or `PIANOMI_TRACE=1`; override the path with `--trace-log PATH`
  or `PIANOMI_TRACE_LOG`.
- Default location: `~/Library/Logs/Pianomi/trace.log` (Console.app and `less`
  both read it; each line is flushed immediately, so a hang or OOM kill keeps
  the tail).

The header records the *effective* config, which is the whole point — in Direct
mode the numbers differ from the TOML:

```text
# mode=Keys targeted=true dry_run=false
# effective: retrigger_gap_ms=50 same_key_gap_ms=60 min_hold_ms=30 ...
# song: notes=2383 length_ms=284159.0 loops=1 humanize=0
# columns: E | scheduled_ms | actual_ms | drift_ms | kind | key_id | physical | held | dest
E    272910.0    272913    +3.0  RELEASE   j    j    held=0  dest=pid:11853
```

What the columns are for:

- **drift** (`actual - scheduled`): a positive spike means the playback thread
  stalled. A steady drift was the old `pgrep`-on-the-hot-path bug.
- **kind**: `PRESS` / `RELEASE` are posted events; `SUPPRESSED` is a note-on that
  could not fire because the *same label* was already held; `PHYSICAL_HELD` is a
  note-on whose **physical** key was still down under a different label (`j`
  held while an arpeggio hits `J` or `Ctrl+J`) — the event is delivered but the
  game sees no up→down transition, so the attack is inaudible; `HELD_RELEASE` is
  a key-up withheld because another note still owns the key; `DEFERRED` is a
  key-up that arrived before its own key-down and was retried.
- **physical**: the stripped key. Filter on it to audit the key-up gaps:

```bash
grep '^E' ~/Library/Logs/Pianomi/trace.log |
  awk -F'\t' '$5=="RELEASE"{r[$7]=$2} $5=="PRESS"&&($7 in r){print $2-r[$7], $6, $7}' |
  sort -n | head
```

Anything below ~17 ms is invisible to a 60 Hz `IsKeyDown` poll; in Direct mode
aim for 50 ms. The run ends with a `# summary:` line carrying the same counters
the UI reports — `presses`, `releases`, `dropped`, `physical_drops`,
`multiplexed`, `tight_retriggers`, and `physical_conflicts`. The last one is the
only counter that measures *inaudible* notes: a `PHYSICAL_HELD` key-down is
delivered but the game never sees the up→down edge, so it stays silent even
though the event arrived. Non-zero means the song asks for two different
pitches on one physical key with no room between them, and nothing on the
playback side can fix it.

`--target-pid PID` points playback at any process, which is how the jitter table
above was measured: run a tiny AppKit event receiver, post to its PID, and
compare arrival times with the trace. It never touches Roblox.

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
