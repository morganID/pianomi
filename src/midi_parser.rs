//! MIDI file parsing.
//!
//! Uses [`midly`] to decode a Standard MIDI File, build a tempo map, pair
//! note-on / note-off events and convert tick positions into milliseconds so
//! the player can schedule events against a wall clock.
//!
//! Beyond the notes themselves the parser keeps the score metadata a human
//! would want when inspecting a file: SMF format, timing base, every tempo
//! change, time-signature changes, key signature and a per-track summary.

use anyhow::{anyhow, Context, Result};
use midly::{Format, Fps, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::collections::{HashMap, VecDeque};
use std::path::Path;

/// A single played note.
#[derive(Debug, Clone)]
pub struct Note {
    pub note: u8,
    pub channel: u8,
    pub start_ms: f64,
    pub duration_ms: f64,
    pub velocity: u8,
}

/// One MIDI `ControlChange` (CC) message, resolved to wall-clock time.
///
/// Pianomi used to discard every CC (the parser's `_ => {}` arm swallowed
/// them), so sustain pedal, channel volume and pan — the events a real
/// performance leans on for its sound — never reached the output. These are
/// captured now and forwarded in MIDI mode so a synth/DAW gets the whole take.
#[derive(Debug, Clone)]
pub struct ControlEvent {
    pub channel: u8,
    /// Controller number, e.g. 64 = sustain pedal, 7 = channel volume, 10 = pan.
    pub controller: u8,
    /// Controller value, 0..=127.
    pub value: u8,
    /// Wall-clock position of the message, resolved through the tempo map.
    pub at_ms: f64,
}

/// One `FF 51` tempo change.
#[derive(Debug, Clone, Copy)]
pub struct TempoEvent {
    pub tick: u64,
    /// Wall-clock position of the change, resolved through the full tempo map.
    pub at_ms: f64,
    pub micros_per_beat: u32,
}

impl TempoEvent {
    /// Quarter-notes per minute for this event.
    pub fn bpm(&self) -> f64 {
        60_000_000.0 / self.micros_per_beat.max(1) as f64
    }
}

/// One `FF 58` time-signature change.
#[derive(Debug, Clone, Copy)]
pub struct TimeSigEvent {
    pub tick: u64,
    /// Wall-clock position of the change.
    pub at_ms: f64,
    pub numerator: u8,
    /// The real denominator (already expanded from the stored power of two).
    pub denominator: u16,
}

impl TimeSigEvent {
    pub fn label(&self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }
}

/// One `FF 59` key-signature change.
#[derive(Debug, Clone, Copy)]
pub struct KeySigEvent {
    pub tick: u64,
    /// Wall-clock position of the change.
    pub at_ms: f64,
    /// Negative = flats, positive = sharps.
    pub sharps: i8,
    pub minor: bool,
}

impl KeySigEvent {
    /// Tonic name, e.g. `"A"` for three sharps in a major key.
    pub fn tonic(&self) -> &'static str {
        const MAJOR: [&str; 15] = [
            "Cb", "Gb", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E", "B", "F#", "C#",
        ];
        const MINOR: [&str; 15] = [
            "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E", "B", "F#", "C#", "G#", "D#", "A#",
        ];
        let idx = (self.sharps as i32 + 7).clamp(0, 14) as usize;
        if self.minor {
            MINOR[idx]
        } else {
            MAJOR[idx]
        }
    }

    /// e.g. `"A major"`.
    pub fn label(&self) -> String {
        format!(
            "{} {}",
            self.tonic(),
            if self.minor { "minor" } else { "major" }
        )
    }
}

/// Per-track summary, so a multi-track file can be inspected track by track
/// instead of only in aggregate.
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub index: usize,
    /// `FF 03` track name, if the file bothers to set one.
    pub name: Option<String>,
    /// `FF 04` instrument name, if present.
    pub instrument: Option<String>,
    /// Note-on events with velocity > 0 seen in this track.
    pub note_events: usize,
    /// Notes that found a matching note-off (and therefore get played).
    pub notes: usize,
    pub channels: Vec<u8>,
    /// GM program numbers selected by `ProgramChange`, if any.
    pub programs: Vec<u8>,
}

impl TrackInfo {
    /// `name` when present, otherwise a stable placeholder.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n.clone(),
            _ => format!("Track {} (unnamed)", self.index),
        }
    }
}

/// A fully parsed song.
#[derive(Debug, Clone, Default)]
pub struct Song {
    pub notes: Vec<Note>,
    /// Every ControlChange (sustain, volume, pan, …) in the file, resolved to
    /// wall-clock time and sorted by position.
    pub control_events: Vec<ControlEvent>,
    pub ticks_per_beat: u16,
    /// Total length of the song in milliseconds (last note end).
    pub length_ms: f64,
    /// One entry per SMF track, in file order.
    pub tracks: Vec<TrackInfo>,
    pub channels_used: Vec<u8>,
    /// The tempo in force at the start of the file (beats per minute).
    pub initial_bpm: f64,
    /// SMF format: 0 = single track, 1 = parallel tracks, 2 = sequential.
    pub format: u8,
    /// Human-readable description of the timing base.
    pub timing: String,
    /// Every distinct tempo change, sorted by tick.
    pub tempo_events: Vec<TempoEvent>,
    pub min_bpm: f64,
    pub max_bpm: f64,
    /// Time-signature changes, sorted by tick, consecutive duplicates removed.
    pub time_signatures: Vec<TimeSigEvent>,
    /// Key-signature changes, sorted by tick, consecutive duplicates removed.
    pub key_signatures: Vec<KeySigEvent>,
}

impl Song {
    /// Keep only notes on the given MIDI channel (1-16).
    pub fn filter_channel(&mut self, channel: u8) {
        let ch = channel.saturating_sub(1); // user-facing 1-16 -> 0-15
        self.notes.retain(|n| n.channel == ch);
    }

    /// Distinct `(numerator, denominator)` pairs, in order of first appearance.
    pub fn distinct_time_signatures(&self) -> Vec<(u8, u16)> {
        let mut out: Vec<(u8, u16)> = Vec::new();
        for ts in &self.time_signatures {
            let pair = (ts.numerator, ts.denominator);
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
        out
    }

    /// Key signature in force at tick 0, if the file declares one.
    pub fn initial_key(&self) -> Option<&KeySigEvent> {
        self.key_signatures.first()
    }

    /// Total note-on events across all tracks (may exceed `notes.len()` when a
    /// note-on never received its note-off).
    pub fn total_note_events(&self) -> usize {
        self.tracks.iter().map(|t| t.note_events).sum()
    }

    /// Number of distinct MIDI channels touched by played notes.
    pub fn channel_count(&self) -> usize {
        self.channels_used.len()
    }
}

/// How ticks turn into wall-clock time.
#[derive(Clone, Copy)]
enum TimingBase {
    /// Metrical timing: the tempo map gives µs per beat, so each tick is
    /// `µs_per_beat / ticks_per_beat`.
    Metrical { ticks_per_beat: f64 },
    /// SMPTE timing: a fixed number of µs per tick, independent of tempo.
    Timecode { micros_per_tick: f64 },
}

/// A note-on / note-off pair still carrying its track, before tick → ms
/// conversion.
struct RawNote {
    track: u32,
    start: u64,
    end: u64,
    channel: u8,
    note: u8,
    velocity: u8,
}

/// Notes currently held, keyed by `(track, channel, pitch)`; each entry is a
/// FIFO queue of `(start tick, velocity)`.
type OpenNotes = HashMap<(u32, u8, u8), VecDeque<(u64, u8)>>;

/// Parse a MIDI file at `path`.
pub fn parse_midi(path: &Path) -> Result<Song> {
    let data =
        std::fs::read(path).with_context(|| format!("reading MIDI file {}", path.display()))?;
    let smf = Smf::parse(&data).map_err(|e| anyhow!("failed to parse MIDI: {:?}", e))?;

    // Resolve the tick base and a fallback tempo (used only when the file never
    // sets one; the SMF default is 120 BPM).
    let (ticks_per_beat, seed_micros, base, timing_desc) = match smf.header.timing {
        Timing::Metrical(tpb) => {
            let tpb = tpb.as_int().max(1);
            (
                tpb,
                500_000u32,
                TimingBase::Metrical {
                    ticks_per_beat: tpb as f64,
                },
                format!("{} ticks per beat (metrical)", tpb),
            )
        }
        Timing::Timecode(fps, tpf) => {
            let fps_val = match fps {
                Fps::Fps24 => 24.0,
                Fps::Fps25 => 25.0,
                Fps::Fps29 => 29.97,
                Fps::Fps30 => 30.0,
            };
            let tpf = tpf.max(1) as u16;
            (
                tpf,
                500_000u32,
                TimingBase::Timecode {
                    micros_per_tick: 1_000_000.0 / (tpf as f64 * fps_val),
                },
                format!("SMPTE {:.2} fps x {} ticks/frame (timecode)", fps_val, tpf),
            )
        }
    };

    let format = match smf.header.format {
        Format::SingleTrack => 0u8,
        Format::Parallel => 1,
        Format::Sequential => 2,
    };

    // Walk every track, accumulating absolute ticks, and collect tempo changes,
    // score metadata plus open/close note events.
    let mut declared_tempo: Vec<(u64, u32)> = Vec::new();
    let mut time_sigs: Vec<TimeSigEvent> = Vec::new();
    let mut key_sigs: Vec<KeySigEvent> = Vec::new();
    // (track, tick, channel, controller, value) for every ControlChange.
    let mut controls: Vec<(u32, u64, u8, u8, u8)> = Vec::new();
    let mut tracks: Vec<TrackInfo> = Vec::with_capacity(smf.tracks.len());
    // (track, channel, note) -> queue of (start_tick, velocity).
    //
    // A queue (not a single slot) is required: the same pitch can legitimately
    // be re-struck while the previous instance is still held down, and a plain
    // `insert` would silently overwrite the earlier note-on, losing it.
    //
    // It must be a **FIFO** queue: MIDI pairs the first note-on with the first
    // note-off. Popping the most recent one instead swaps the durations of
    // overlapping same-pitch notes (the earlier note gets the later note's
    // length and vice versa).
    // (track, channel, pitch) -> queue of (start tick, velocity) for the notes
    // currently held on that key. FIFO, so overlapping same-pitch notes keep
    // their own durations.
    let mut open: OpenNotes = HashMap::new();
    let mut raw: Vec<RawNote> = Vec::new();

    /// Close one open note for `key` at `tick`, recording it if one was open.
    fn close_note(open: &mut OpenNotes, key: (u32, u8, u8), tick: u64, raw: &mut Vec<RawNote>) {
        if let Some(queue) = open.get_mut(&key) {
            if let Some((start, vel)) = queue.pop_front() {
                if queue.is_empty() {
                    open.remove(&key);
                }
                raw.push(RawNote {
                    track: key.0,
                    start,
                    end: tick,
                    channel: key.1,
                    note: key.2,
                    velocity: vel,
                });
            }
        }
    }

    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut abs: u64 = 0;
        let mut info = TrackInfo {
            index: ti,
            ..Default::default()
        };

        for ev in track.iter() {
            abs += ev.delta.as_int() as u64;
            match ev.kind {
                TrackEventKind::Meta(meta) => match meta {
                    MetaMessage::Tempo(m) => {
                        declared_tempo.push((abs, m.as_int()));
                    }
                    MetaMessage::TimeSignature(num, den_pow, _, _) => {
                        // The stored denominator is a power of two; guard the
                        // shift so a corrupt file cannot overflow.
                        let den = 1u16.checked_shl(den_pow.min(15) as u32).unwrap_or(1);
                        time_sigs.push(TimeSigEvent {
                            tick: abs,
                            at_ms: 0.0, // resolved once the tempo map is final
                            numerator: num,
                            denominator: den,
                        });
                    }
                    MetaMessage::KeySignature(sharps, minor) => {
                        key_sigs.push(KeySigEvent {
                            tick: abs,
                            at_ms: 0.0, // resolved once the tempo map is final
                            sharps,
                            minor,
                        });
                    }
                    MetaMessage::TrackName(name) => {
                        if let Ok(s) = std::str::from_utf8(name) {
                            if info.name.is_none() {
                                info.name = Some(s.to_string());
                            }
                        }
                    }
                    MetaMessage::InstrumentName(name) => {
                        if let Ok(s) = std::str::from_utf8(name) {
                            if info.instrument.is_none() {
                                info.instrument = Some(s.to_string());
                            }
                        }
                    }
                    _ => {}
                },
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() != 0 => {
                            info.note_events += 1;
                            if !info.channels.contains(&ch) {
                                info.channels.push(ch);
                            }
                            open.entry((ti as u32, ch, key.as_int()))
                                .or_default()
                                .push_back((abs, vel.as_int()));
                        }
                        // NoteOff always ends a note.
                        MidiMessage::NoteOff { key, .. } => {
                            close_note(&mut open, (ti as u32, ch, key.as_int()), abs, &mut raw);
                        }
                        // NoteOn with velocity 0 is a note-off in disguise; a very
                        // common encoding that a naive parser turns into a
                        // zero-length note or drops entirely.
                        MidiMessage::NoteOn { key, .. } => {
                            close_note(&mut open, (ti as u32, ch, key.as_int()), abs, &mut raw);
                        }
                        MidiMessage::ProgramChange { program } => {
                            let p = program.as_int();
                            if !info.programs.contains(&p) {
                                info.programs.push(p);
                            }
                        }
                        // ControlChange (sustain pedal, channel volume, pan, …):
                        // captured so the output can reproduce the performance's
                        // dynamics instead of dropping them on the floor.
                        MidiMessage::Controller { controller, value } => {
                            controls.push((
                                ti as u32,
                                abs,
                                ch,
                                controller.as_int(),
                                value.as_int(),
                            ));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // A note-on with no matching note-off (truncated file, or a sequencer
        // that relies on "end of track" to release) is closed here rather than
        // silently dropped.
        let dangling: Vec<(u32, u8, u8)> =
            open.keys().copied().filter(|k| k.0 == ti as u32).collect();
        for k in dangling {
            close_note(&mut open, k, abs, &mut raw);
        }

        tracks.push(info);
    }

    // Count the notes each track actually contributed.
    for r in &raw {
        if let Some(t) = tracks.get_mut(r.track as usize) {
            t.notes += 1;
        }
    }

    // Sort by tick, then keep the **last** tempo defined at each tick.
    //
    // Keeping the first (which is what `dedup_by_key` does) is wrong in the one
    // case that matters most: a file whose own tempo event is also at tick 0
    // would otherwise be ignored in favour of the 120 BPM default.
    // "Last wins" also matches how a sequencer reads multiple events on one tick.
    declared_tempo.sort_by_key(|&(t, _)| t);
    let mut merged: Vec<(u64, u32)> = Vec::with_capacity(declared_tempo.len());
    for &(t, m) in &declared_tempo {
        match merged.last_mut() {
            Some(last) if last.0 == t => last.1 = m,
            _ => merged.push((t, m)),
        }
    }

    let tempo_events: Vec<TempoEvent> = merged
        .iter()
        .map(|&(tick, micros_per_beat)| TempoEvent {
            tick,
            at_ms: 0.0, // resolved below
            micros_per_beat,
        })
        .collect();

    // The tempo map needs an entry at tick 0 to seed `tick_to_micros`; if the
    // file's first tempo event is later, the SMF default 120 BPM applies first.
    let mut tempo_map = merged;
    if tempo_map.first().is_none_or(|&(t, _)| t > 0) {
        tempo_map.insert(0, (0, seed_micros));
    }

    // Now that the tempo map is final, stamp every event with its wall-clock
    // position so callers can print a readable table.
    let mut tempo_events = tempo_events;
    for ev in &mut tempo_events {
        ev.at_ms = tick_to_micros(ev.tick, &tempo_map, base) / 1000.0;
    }

    let (min_bpm, max_bpm) =
        tempo_events
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), ev| {
                let b = ev.bpm();
                (lo.min(b), hi.max(b))
            });
    let (min_bpm, max_bpm) = if tempo_events.is_empty() {
        (120.0, 120.0)
    } else {
        (min_bpm, max_bpm)
    };

    // Sort and collapse repeated identical signatures (files often restate the
    // same signature on every track, which is not a real "change").
    time_sigs.sort_by_key(|s| s.tick);
    time_sigs.dedup_by(|a, b| a.numerator == b.numerator && a.denominator == b.denominator);
    key_sigs.sort_by_key(|s| s.tick);
    key_sigs.dedup_by(|a, b| a.sharps == b.sharps && a.minor == b.minor);
    for s in &mut time_sigs {
        s.at_ms = tick_to_micros(s.tick, &tempo_map, base) / 1000.0;
    }
    for s in &mut key_sigs {
        s.at_ms = tick_to_micros(s.tick, &tempo_map, base) / 1000.0;
    }

    let mut notes: Vec<Note> = Vec::with_capacity(raw.len());
    for r in raw {
        let s = tick_to_micros(r.start, &tempo_map, base) / 1000.0;
        let e = tick_to_micros(r.end, &tempo_map, base) / 1000.0;
        notes.push(Note {
            note: r.note,
            channel: r.channel,
            start_ms: s,
            duration_ms: (e - s).max(0.0),
            velocity: r.velocity,
        });
    }

    notes.sort_by(|a, b| {
        a.start_ms
            .partial_cmp(&b.start_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Resolve every ControlChange to wall-clock time and sort by position so the
    // player can replay them in order.
    let mut control_events: Vec<ControlEvent> = controls
        .iter()
        .map(|&(_, tick, ch, cc, val)| ControlEvent {
            channel: ch,
            controller: cc,
            value: val,
            at_ms: tick_to_micros(tick, &tempo_map, base) / 1000.0,
        })
        .collect();
    control_events.sort_by(|a, b| {
        a.at_ms
            .partial_cmp(&b.at_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let length_ms = notes
        .iter()
        .map(|n| n.start_ms + n.duration_ms)
        .fold(0.0_f64, f64::max);

    let mut channels: Vec<u8> = notes.iter().map(|n| n.channel).collect();
    channels.sort_unstable();
    channels.dedup();

    let initial_micros = tempo_map
        .first()
        .map(|&(_, m)| m as f64)
        .unwrap_or(500_000.0);

    Ok(Song {
        notes,
        control_events,
        ticks_per_beat,
        length_ms,
        tracks,
        channels_used: channels,
        initial_bpm: 60_000_000.0 / initial_micros,
        format,
        timing: timing_desc,
        tempo_events,
        min_bpm,
        max_bpm,
        time_signatures: time_sigs,
        key_signatures: key_sigs,
    })
}

/// Convert an absolute tick position into microseconds.
fn tick_to_micros(tick: u64, tempo_map: &[(u64, u32)], base: TimingBase) -> f64 {
    let ticks_per_beat = match base {
        // SMPTE: wall time is a plain tick count, tempo events do not apply.
        TimingBase::Timecode { micros_per_tick } => return tick as f64 * micros_per_tick,
        TimingBase::Metrical { ticks_per_beat } => ticks_per_beat,
    };

    let mut total = 0.0_f64;
    let mut cursor = 0u64;
    let mut micros_per_beat = tempo_map
        .first()
        .map(|&(_, m)| m as f64)
        .unwrap_or(500_000.0);

    // `tempo_map` is sorted by tick; each entry (except the first) starts a new
    // tempo segment. Walk the segments that end before `tick`.
    for &(t, m) in tempo_map.iter().skip(1) {
        if tick >= t {
            total += (t - cursor) as f64 * micros_per_beat / ticks_per_beat;
            cursor = t;
            micros_per_beat = m as f64;
        } else {
            break;
        }
    }

    // Time inside the final (current) tempo segment.
    total += (tick - cursor) as f64 * micros_per_beat / ticks_per_beat;
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sharps: i8, minor: bool) -> KeySigEvent {
        KeySigEvent {
            tick: 0,
            at_ms: 0.0,
            sharps,
            minor,
        }
    }

    #[test]
    fn key_signature_names_follow_the_circle_of_fifths() {
        assert_eq!(key(0, false).label(), "C major");
        assert_eq!(key(3, false).label(), "A major");
        assert_eq!(key(-3, false).label(), "Eb major");
        assert_eq!(key(0, true).label(), "A minor");
        assert_eq!(key(-7, true).label(), "Ab minor");
        assert_eq!(key(7, false).label(), "C# major");
        // Out-of-range values must not panic, just clamp.
        assert_eq!(key(99, false).label(), "C# major");
        assert_eq!(key(-99, false).label(), "Cb major");
    }

    #[test]
    fn tempo_event_bpm_is_derived_from_micros_per_beat() {
        let ev = TempoEvent {
            tick: 0,
            at_ms: 0.0,
            micros_per_beat: 500_000,
        };
        assert!((ev.bpm() - 120.0).abs() < 1e-9);

        let slow = TempoEvent {
            tick: 0,
            at_ms: 0.0,
            micros_per_beat: 3_000_003,
        };
        // Truncating this to an integer yields 19, which is where the widely
        // quoted "19 bpm" for some files comes from.
        assert!(slow.bpm() > 19.99 && slow.bpm() < 20.0);
    }

    #[test]
    fn tick_to_micros_uses_the_tempo_in_force() {
        let metrical = TimingBase::Metrical {
            ticks_per_beat: 480.0,
        };
        // Single constant tempo: 480 ticks = one beat = 500 ms.
        let map = [(0u64, 500_000u32)];
        assert!((tick_to_micros(480, &map, metrical) - 500_000.0).abs() < 1e-6);
        assert!((tick_to_micros(960, &map, metrical) - 1_000_000.0).abs() < 1e-6);

        // Tempo doubles at tick 480: first beat 500 ms, second beat 250 ms.
        let map = [(0u64, 500_000u32), (480, 250_000)];
        assert!((tick_to_micros(960, &map, metrical) - 750_000.0).abs() < 1e-6);
        // A tick before the change keeps the original tempo.
        assert!((tick_to_micros(240, &map, metrical) - 250_000.0).abs() < 1e-6);
    }

    #[test]
    fn smpte_timing_ignores_tempo_events() {
        let timecode = TimingBase::Timecode {
            micros_per_tick: 1_000_000.0 / (30.0 * 40.0),
        };
        let map = [(0u64, 500_000u32), (100, 250_000)];
        // 1200 ticks at 30 fps x 40 ticks/frame = exactly one second.
        assert!((tick_to_micros(1200, &map, timecode) - 1_000_000.0).abs() < 1e-6);
    }

    #[test]
    fn distinct_time_signatures_keep_first_appearance_order() {
        let song = Song {
            time_signatures: vec![
                TimeSigEvent {
                    tick: 0,
                    at_ms: 0.0,
                    numerator: 4,
                    denominator: 4,
                },
                TimeSigEvent {
                    tick: 1920,
                    at_ms: 0.0,
                    numerator: 3,
                    denominator: 4,
                },
                TimeSigEvent {
                    tick: 3360,
                    at_ms: 0.0,
                    numerator: 4,
                    denominator: 4,
                },
                TimeSigEvent {
                    tick: 37440,
                    at_ms: 0.0,
                    numerator: 2,
                    denominator: 4,
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            song.distinct_time_signatures(),
            vec![(4, 4), (3, 4), (2, 4)]
        );
    }
}
