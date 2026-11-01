//! Convert Online Sequencer note data into a Standard MIDI File (Type 0).
//!
//! Input format (as copied from the editor / share string):
//!     Online Sequencer:<id>:<start> <note> <dur> <inst> <vol>;<pos> <note> <dur> <inst> <vol>;...
//!
//! Each note token is:  <pos> <NOTE> <duration> <instrument> <volume>
//!   - pos/duration are in sequencer "steps"
//!   - NOTE is like C5, C#5, A#4, F#6
//!   - instrument is ignored (everything goes to channel 0, piano)
//!   - volume 0..1 maps to MIDI velocity 0..127
//!
//! Assumptions (no BPM is stored in the data):
//!   - Tempo `bpm` (the caller passes the real song tempo, e.g. 125)
//!   - 1 step = 1 sixteenth note -> 120 ticks per step at PPQ 480
//!     (Online Sequencer's grid unit is a 1/4 beat; treating a step as an
//!     eighth note makes everything play back at half speed.)
//!   - Notes with duration <= 0 are skipped.
//!
//! This is a Rust port of `os2midi.py` so the GUI can convert pasted data
//! without shelling out to Python.

use anyhow::{anyhow, Result};
use std::fs;
use std::path::Path;

const PPQ: u16 = 480;
// One Online Sequencer "step" is a 1/4 beat = a sixteenth note = 120 ticks.
const TICKS_PER_STEP: f64 = (PPQ / 4) as f64;

fn note_to_num(s: &str) -> Result<u8> {
    if s.len() < 2 {
        return Err(anyhow!("invalid note name: {:?}", s));
    }
    let bytes = s.as_bytes();
    let sharp = bytes[1] == b'#';
    let (name, octv_str) = if sharp {
        (&s[0..2], &s[2..])
    } else {
        (&s[0..1], &s[1..])
    };
    let idx: i16 = match name {
        "C" => 0,
        "C#" => 1,
        "D" => 2,
        "D#" => 3,
        "E" => 4,
        "F" => 5,
        "F#" => 6,
        "G" => 7,
        "G#" => 8,
        "A" => 9,
        "A#" => 10,
        "B" => 11,
        _ => return Err(anyhow!("invalid note name: {:?}", name)),
    };
    let octv: i16 = octv_str
        .parse()
        .map_err(|_| anyhow!("invalid octave in note {:?}", s))?;
    let n = (octv + 1) * 12 + idx;
    if !(0..=127).contains(&n) {
        return Err(anyhow!("note {:?} out of MIDI range", s));
    }
    Ok(n as u8)
}

/// Variable-length quantity encoding (MIDI delta times).
fn vlq(mut n: i64) -> Vec<u8> {
    let mut out = vec![(n & 0x7F) as u8];
    n >>= 7;
    while n > 0 {
        out.insert(0, ((n & 0x7F) as u8) | 0x80);
        n >>= 7;
    }
    out
}

/// Parse raw Online Sequencer text and build the raw note list
/// `(start_tick, note, length_tick, velocity)`.
fn parse_notes(raw: &str) -> Result<Vec<(i64, u8, i64, u8)>> {
    let raw = raw.trim();
    // Header is `Online Sequencer:<song id>:<first start> <NOTE> <dur> ...`.
    // Note that "Online Sequencer" itself contains a space, so the header must
    // be stripped as a whole prefix *before* splitting on the first space —
    // otherwise the first token becomes "Sequencer:<id>:<start>" and the first
    // note silently lands at tick 0 instead of its real position.
    //
    // Input without a header (`<start> <NOTE> ...` or `<NOTE> <dur> ...`) goes
    // through the same path: a leading number is read as the start, anything
    // else leaves the start at 0.
    let raw = raw.strip_prefix("Online Sequencer:").unwrap_or(raw);
    let (header_start, body) = match raw.split_once(' ') {
        Some((head, rest)) => match head.rsplit(':').next().and_then(|s| s.parse::<f64>().ok()) {
            Some(start) => (start, rest),
            None => (0.0, raw),
        },
        None => (0.0, raw),
    };

    let mut notes: Vec<(i64, u8, i64, u8)> = Vec::new();
    for chunk in body.split(';') {
        let toks: Vec<&str> = chunk.split_whitespace().collect();
        // Each chunk is either [note, dur, inst, vol] (first note, whose start
        // lives in the header) or [pos, note, dur, inst, vol].
        let (pos, note_s, dur_s, vol_s) = match toks.len() {
            5 => (
                toks[0].parse::<f64>().unwrap_or(header_start),
                toks[1],
                toks[2],
                toks[4],
            ),
            4 => (header_start, toks[0], toks[1], toks[3]),
            _ => continue,
        };
        let mut dur: f64 = dur_s.parse().unwrap_or(0.0);
        // Online Sequencer occasionally stores a 0-length note (always an exact
        // duplicate of a real note at the same tick). Clamp instead of dropping
        // so no note can ever disappear from the transcription.
        if dur <= 0.0 {
            dur = 1.0; // one step = 1/16 note
        }
        let vol: f64 = vol_s.parse().unwrap_or(0.0);
        let nn = match note_to_num(note_s) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let vel = (vol * 127.0).round().clamp(1.0, 127.0) as u8;
        let start = (pos * TICKS_PER_STEP).round() as i64;
        let length = (dur * TICKS_PER_STEP).round().max(1.0) as i64;
        notes.push((start, nn, length, vel));
    }

    if notes.is_empty() {
        return Err(anyhow!("no notes found in input"));
    }
    Ok(notes)
}

/// Convert raw Online Sequencer text into Standard MIDI File bytes.
pub fn convert_os_data(raw: &str, bpm: f64) -> Result<Vec<u8>> {
    let notes = parse_notes(raw)?;

    // Build (tick, status, note, velocity) events, sorted by time then status.
    let mut events: Vec<(i64, u8, u8, u8)> = Vec::with_capacity(notes.len() * 2);
    for (start, nn, length, vel) in &notes {
        events.push((*start, 0x90, *nn, *vel)); // note on (ch 0)
        events.push((start + length, 0x80, *nn, 0)); // note off (ch 0)
    }
    events.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let us_per_beat = (60_000_000.0 / bpm).round() as u32;

    let mut track: Vec<u8> = Vec::new();
    track.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03]);
    track.extend_from_slice(&us_per_beat.to_be_bytes()[1..]); // 3-byte tempo
    track.extend_from_slice(&[0x00, 0xC0, 0x00]); // program change -> grand piano
    let mut prev = 0i64;
    for (t, status, nn, vel) in &events {
        track.extend_from_slice(&vlq(t - prev));
        prev = *t;
        track.push(*status);
        track.push(*nn);
        track.push(*vel);
    }
    track.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]); // end of track

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"MThd");
    out.extend_from_slice(&[0, 0, 0, 6]); // chunk length = 6
    out.extend_from_slice(&[0, 0]); // format 0
    out.extend_from_slice(&[0, 1]); // 1 track
    out.extend_from_slice(&PPQ.to_be_bytes()); // division (ticks per beat)
    out.extend_from_slice(b"MTrk");
    out.extend_from_slice(&(track.len() as u32).to_be_bytes());
    out.extend_from_slice(&track);
    Ok(out)
}

/// Convert raw Online Sequencer text and write it to `path`.
pub fn write_os_midi(raw: &str, path: &Path, bpm: f64) -> Result<usize> {
    let bytes = convert_os_data(raw, bpm)?;
    fs::write(path, &bytes).map_err(|e| anyhow!("writing {}: {}", path.display(), e))?;
    Ok(bytes.len())
}
