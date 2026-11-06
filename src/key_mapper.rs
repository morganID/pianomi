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

    /// The **physical** key this binding lands on, with all modifiers stripped.
    ///
    /// The Virtual Piano layout reuses the same 36 QWERTY keys three times:
    /// plain (`1` = C2), Shift (`!` = C#2) and Ctrl (`Ctrl+1` = A0). Those are
    /// three different notes but *one* physical key, and a key that is already
    /// down cannot produce a second key-down — so a `!` arriving while `1` is
    /// held simply never sounds. The scheduler must therefore group and trim
    /// by this id, not by [`Binding::id`].
    ///
    /// Shifted digit symbols are folded back to their base digit, and uppercase
    /// letters to lowercase, matching [`crate::keycode::resolve_key`].
    pub fn physical_id(&self) -> String {
        let label = self.label.trim();
        // Spelled-out keys ("Space", "Enter", …) are not single characters, so
        // they cannot collide with a letter binding by accident.
        if label.chars().count() > 1 {
            return format!("name:{}", label.to_ascii_lowercase());
        }
        let c = label.chars().next().unwrap_or('?');
        let base = match c {
            '!' => '1',
            '@' => '2',
            '#' => '3',
            '$' => '4',
            '%' => '5',
            '^' => '6',
            '&' => '7',
            '*' => '8',
            '(' => '9',
            ')' => '0',
            c if c.is_ascii_uppercase() => c.to_ascii_lowercase(),
            c => c,
        };
        base.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_variants_share_one_physical_key() {
        assert_eq!(Binding::new("1", false).physical_id(), "1");
        assert_eq!(Binding::new("!", false).physical_id(), "1");
        assert_eq!(Binding::new("1", true).physical_id(), "1");
        assert_eq!(Binding::new("s", false).physical_id(), "s");
        assert_eq!(Binding::new("S", false).physical_id(), "s");
        assert_eq!(Binding::new("y", true).physical_id(), "y");
    }

    #[test]
    fn different_physical_keys_stay_distinct() {
        assert_ne!(
            Binding::new("1", false).physical_id(),
            Binding::new("2", false).physical_id()
        );
        assert_ne!(
            Binding::new("s", false).physical_id(),
            Binding::new("d", false).physical_id()
        );
        // Spelled-out labels must not be mistaken for their first letter.
        assert_ne!(
            Binding::new("Space", false).physical_id(),
            Binding::new("s", false).physical_id()
        );
    }

    #[test]
    fn logical_ids_still_keep_modifiers_apart() {
        // Refcounting needs the finer-grained identity; only scheduling folds
        // the three variants together.
        assert_ne!(Binding::new("1", false).id(), Binding::new("!", false).id());
        assert_ne!(Binding::new("1", false).id(), Binding::new("1", true).id());
    }
}
