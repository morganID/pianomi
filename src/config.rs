//! Configuration loading for `pianomi`.
//!
//! The config is a TOML file describing the Roblox keyboard layout, the
//! humanization parameters and playback tuning. If no config file is supplied
//! (or it cannot be read) a built-in copy of `config/default.toml` is used so
//! the program always has sensible defaults.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Built-in fallback config, embedded at compile time.
const BUILTIN: &str = include_str!("../config/default.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct GameConfig {
    pub name: String,
    /// Informational only — the actual OS window the user must focus.
    pub window_title: String,
}

/// A single (key -> MIDI note) binding.
#[derive(Debug, Clone, Deserialize)]
pub struct KeyMap {
    pub key: String,
    pub note: u8,
    /// `"ctrl"` (or `"control"`) when this key belongs to one of the extended
    /// octaves, which the game reaches with Ctrl instead of plain/Shift.
    #[serde(default)]
    pub mods: Option<String>,
}

impl KeyMap {
    pub fn uses_ctrl(&self) -> bool {
        matches!(
            self.mods.as_deref().map(str::trim).map(str::to_ascii_lowercase),
            Some(ref m) if m == "ctrl" || m == "control"
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeyboardConfig {
    pub keys: Vec<KeyMap>,
    #[serde(default)]
    pub transpose: i8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HumanizeConfig {
    #[serde(default = "default_f64_35")]
    pub timing_variation_ms: f64,
    #[serde(default = "default_f64_25")]
    pub duration_variation_ms: f64,
    #[serde(default = "default_f64_18")]
    pub chord_delay_ms: f64,
    #[serde(default = "default_true")]
    pub use_velocity_for_duration: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerConfig {
    #[serde(default = "default_u64_3000")]
    pub start_delay_ms: u64,
    #[serde(default = "default_f64_30")]
    pub min_hold_ms: f64,
    /// Keyboard mode only: cap how long a key stays down, in ms. `0` keeps the
    /// written duration. Onsets are never moved — only the release is pulled in.
    #[serde(default = "default_f64_0")]
    pub max_hold_ms: f64,
    /// Keyboard mode only: when two DIFFERENT pitches need the same physical
    /// key on the same tick (e.g. C2 `1` + C#2 `!`), play them in sequence
    /// separated by this many ms instead of dropping one. `0` disables it.
    /// Values below `min_hold_ms` are raised to it so both presses register.
    #[serde(default = "default_f64_30")]
    pub same_key_gap_ms: f64,
    /// Keyboard mode only: real-time gap (ms) between a key-up and the next
    /// key-down on the SAME physical key. A sharp is the same physical key as
    /// the white note below it (`1` / `!`); releasing exactly when the next
    /// note starts puts both CGEvent posts in one scheduler pass, and the game
    /// — which samples `IsKeyDown` once per frame — only sees "still down" and
    /// never fires `InputBegan` for the second note. Pulling the release back
    /// by this much leaves a real frame boundary. **Onsets are never moved;**
    /// only the earlier note's duration shrinks.
    #[serde(default = "default_f64_20")]
    pub retrigger_gap_ms: f64,
    /// Keyboard mode only, and only when key events are posted straight into
    /// Roblox's event queue (`--target-roblox` / Direct to Roblox). Those events
    /// are drained in frame-sized batches, so a key-up and the next key-down
    /// need a wider real-time separation than they do on the synchronous HID
    /// path. This many ms is added on top of `retrigger_gap_ms`; `0` disables
    /// the widening for an A/B comparison. Only releases move — onsets do not.
    #[serde(default = "default_f64_30")]
    pub targeted_extra_gap_ms: f64,
    #[serde(default = "default_u64_1")]
    pub tick_ms: u64,
    #[serde(default = "default_u64_800")]
    pub loop_gap_ms: u64,
    /// `"char"` (default) stamps the Shift flag onto the key event itself;
    /// `"key"` presses the real Shift key around that one event. See
    /// [`crate::keycode::ShiftMode`].
    #[serde(default = "default_shift_mode")]
    pub shift_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub game: GameConfig,
    pub keyboard: KeyboardConfig,
    #[serde(default)]
    pub humanize: HumanizeConfig,
    #[serde(default)]
    pub player: PlayerConfig,
    #[serde(default)]
    pub library: LibraryConfig,
}

/// Where the menu bar app's **Library** menu looks for songs.
///
/// Only the GUI uses this; the CLI takes an explicit `--file`. A missing or
/// empty folder simply hides the Library menu, so this never blocks playback.
#[derive(Debug, Clone, Deserialize)]
pub struct LibraryConfig {
    /// Folder scanned (one level deep) for `.mid` / `.midi` files. A leading
    /// `~/` is expanded to the home directory. `PIANOMI_LIBRARY` overrides it,
    /// which is the only way to change it without rebuilding the bundle.
    #[serde(default = "default_library_dir")]
    pub dir: String,
}

impl LibraryConfig {
    /// The configured folder, with `~` expanded and the env override applied.
    pub fn resolved_dir(&self) -> std::path::PathBuf {
        let raw = std::env::var("PIANOMI_LIBRARY").unwrap_or_else(|_| self.dir.clone());
        expand_tilde(&raw)
    }
}

/// Expand a leading `~/` against `$HOME`. Anything else is returned as-is.
fn expand_tilde(raw: &str) -> std::path::PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    std::path::PathBuf::from(raw)
}

// --- defaults for optional fields ------------------------------------------
fn default_library_dir() -> String {
    // Deliberately a plain path in the config rather than a constant in the
    // code: the Library is user content, so it belongs in the config file.
    "~/Documents/MIDI-Piano-Klasifikasi/Piano-Solo".to_string()
}
fn default_f64_35() -> f64 {
    35.0
}
fn default_f64_25() -> f64 {
    25.0
}
fn default_f64_18() -> f64 {
    18.0
}
fn default_true() -> bool {
    true
}
fn default_u64_3000() -> u64 {
    3000
}
fn default_f64_30() -> f64 {
    30.0
}
fn default_f64_20() -> f64 {
    20.0
}
fn default_f64_0() -> f64 {
    0.0
}
fn default_u64_1() -> u64 {
    1
}
fn default_u64_800() -> u64 {
    800
}
fn default_shift_mode() -> String {
    // Roblox's Virtual Piano asks `IsKeyDown(LeftShift)`, which never sees an
    // event flag, so the real key has to go down.
    "key".to_string()
}

impl Default for HumanizeConfig {
    fn default() -> Self {
        HumanizeConfig {
            timing_variation_ms: default_f64_35(),
            duration_variation_ms: default_f64_25(),
            chord_delay_ms: default_f64_18(),
            use_velocity_for_duration: default_true(),
        }
    }
}

impl Default for LibraryConfig {
    fn default() -> Self {
        LibraryConfig {
            dir: default_library_dir(),
        }
    }
}

impl Default for PlayerConfig {
    fn default() -> Self {
        PlayerConfig {
            start_delay_ms: default_u64_3000(),
            min_hold_ms: default_f64_30(),
            max_hold_ms: default_f64_0(),
            same_key_gap_ms: default_f64_30(),
            retrigger_gap_ms: default_f64_20(),
            targeted_extra_gap_ms: default_f64_30(),
            tick_ms: default_u64_1(),
            loop_gap_ms: default_u64_800(),
            shift_mode: default_shift_mode(),
        }
    }
}

impl Config {
    /// Load configuration from `path` if given and readable, otherwise fall
    /// back to the embedded built-in defaults.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let text = match path {
            Some(p) => {
                if p.exists() {
                    std::fs::read_to_string(p)
                        .with_context(|| format!("reading config {}", p.display()))?
                } else {
                    eprintln!(
                        "warning: config {} not found, using built-in defaults",
                        p.display()
                    );
                    BUILTIN.to_string()
                }
            }
            None => BUILTIN.to_string(),
        };

        let cfg: Config = toml::from_str(&text).context("parsing TOML config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Returns the embedded default config (useful for `--print-config`).
    pub fn builtin() -> &'static str {
        BUILTIN
    }

    fn validate(&self) -> Result<()> {
        if self.keyboard.keys.is_empty() {
            return Err(anyhow!("config has no keyboard.keys bindings"));
        }
        for km in &self.keyboard.keys {
            if km.key.chars().count() != 1 {
                return Err(anyhow!("key '{}' must be a single character", km.key));
            }
            if km.note > 127 {
                return Err(anyhow!("note {} out of MIDI range (0-127)", km.note));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_against_home() {
        let home = std::env::var("HOME").expect("HOME is always set on macOS/Linux");
        assert_eq!(
            expand_tilde("~/Music/songs"),
            std::path::PathBuf::from(home).join("Music/songs")
        );
    }

    #[test]
    fn absolute_paths_are_left_alone() {
        assert_eq!(
            expand_tilde("/tmp/songs"),
            std::path::PathBuf::from("/tmp/songs")
        );
    }

    #[test]
    fn library_dir_resolves_to_an_absolute_path() {
        let lib = LibraryConfig::default();
        assert!(!lib.dir.is_empty());
        assert!(lib.resolved_dir().is_absolute());
    }
}
