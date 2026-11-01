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
}

// --- defaults for optional fields ------------------------------------------
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
fn default_u64_1() -> u64 {
    1
}
fn default_u64_800() -> u64 {
    800
}
fn default_shift_mode() -> String {
    "char".to_string()
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

impl Default for PlayerConfig {
    fn default() -> Self {
        PlayerConfig {
            start_delay_ms: default_u64_3000(),
            min_hold_ms: default_f64_30(),
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
