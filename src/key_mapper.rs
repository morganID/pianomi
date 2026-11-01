//! Maps MIDI note numbers to keyboard keys using the loaded configuration.

use crate::config::KeyboardConfig;
use std::collections::HashMap;

/// One resolved key press: which key, plus the modifier the game needs to see.
///
/// The Virtual Piano layout uses two modifier layers on the same 36 physical
/// keys:
///
/// * **plain / Shift** — the middle 61 keys, C2 (36) … C7 (96).
/// * **Ctrl** — the 27 outer keys of the in-game 88-key piano, A0 (21) … B1 (35)
///   and C#7 (97) … C8 (108). They are drawn dimmed because they need the
///   second modifier.
///
/// A sharp is still just `key` holding Shift, so `ctrl` and Shift can in
/// principle combine; the default config never needs that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The literal label from the config: `"a"`, `"A"`, `"!"`, `"8"`, …
    pub label: String,
    /// Whether Ctrl must be held (the extended octaves).
    pub ctrl: bool,
}

impl Binding {
    pub fn new(label: impl Into<String>, ctrl: bool) -> Self {
        Binding {
            label: label.into(),
            ctrl,
        }
    }

    /// Stable identity used for reference counting held keys. `Ctrl+1` and `1`
    /// are different physical events even though both carry the label `"1"`.
    pub fn id(&self) -> String {
        if self.ctrl {
            format!("ctrl+{}", self.label)
        } else {
            self.label.clone()
        }
    }
}

pub struct KeyMapper {
    /// MIDI note number -> keyboard binding.
    note_to_key: HashMap<u8, Binding>,
    transpose: i16,
}

impl KeyMapper {
    pub fn from_config(cfg: &KeyboardConfig) -> Self {
        let mut map = HashMap::new();
        for km in &cfg.keys {
            map.insert(km.note, Binding::new(km.key.clone(), km.uses_ctrl()));
        }
        KeyMapper {
            note_to_key: map,
            transpose: cfg.transpose as i16,
        }
    }

    /// Return the binding for `note` after applying transpose.
    /// Returns `None` when the (transposed) note is unmapped or out of range.
    pub fn key_for_note(&self, note: u8) -> Option<&Binding> {
        self.note_to_key.get(&self.note_after_transpose(note)?)
    }

    /// Apply the configured transpose and reject anything outside 0..=127.
    ///
    /// MIDI output needs this without a keymap: transposing a song up a
    /// semitone should move the notes it *sends*, not just the keys it presses.
    pub fn note_after_transpose(&self, note: u8) -> Option<u8> {
        let n = note as i16 + self.transpose;
        if !(0..=127).contains(&n) {
            return None;
        }
        Some(n as u8)
    }

    /// The configured transpose in semitones.
    pub fn transpose(&self) -> i16 {
        self.transpose
    }

    /// Lowest and highest MIDI note that has a binding (transpose excluded).
    pub fn mapped_range(&self) -> (u8, u8) {
        let mut min = 255u8;
        let mut max = 0u8;
        for &n in self.note_to_key.keys() {
            min = min.min(n);
            max = max.max(n);
        }
        (min, max)
    }
}
