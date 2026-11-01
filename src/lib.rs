//! `pianomi` — shared library for playing MIDI files into Roblox piano games
//! with a humanization layer.
//!
//! This crate is used by two binaries:
//!   * `pianomi`     — the command-line interface.
//!   * `pianomi-gui` — the macOS menu bar app.

pub mod config;
pub mod humanizer;
pub mod key_mapper;
pub mod keycode;
pub mod midi_out;
pub mod midi_parser;
pub mod os2midi;
pub mod perm;
pub mod player;

pub use config::Config;
pub use midi_parser::{Note, Song};

/// Convert a MIDI note number to its scientific name, e.g. 60 -> "C4".
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let name = NAMES[(note % 12) as usize];
    let octave = (note as i16 / 12) - 1;
    format!("{}{}", name, octave)
}
