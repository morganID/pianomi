//! The humanization layer.
//!
//! Turns a mechanically perfect note list into something that feels played by a
//! person:
//!   * speed scaling (so `--speed 2` plays twice as fast),
//!   * small random timing jitter on every note start,
//!   * small random duration jitter on every note length,
//!   * velocity-dependent holding (louder notes are held a touch longer),
//!   * chord arpeggiation (simultaneous notes are staggered slightly).
//!
//! Every effect is scaled by the humanize level (0-100): at 0 the output is
//! perfectly robotic, at 100 it uses the full variation configured in TOML.

use crate::config::HumanizeConfig;
use crate::midi_parser::Note;
use rand::Rng;

pub struct Humanizer {
    /// 0.0 .. 1.0
    level: f64,
    /// Playback speed multiplier (>= 0.01).
    speed: f64,
    cfg: HumanizeConfig,
}

impl Humanizer {
    pub fn new(level_pct: u8, speed: f64, cfg: HumanizeConfig) -> Self {
        Humanizer {
            level: (level_pct.clamp(0, 100) as f64) / 100.0,
            speed: speed.max(0.01),
            cfg,
        }
    }

    /// Mutates `notes` in place, applying speed scaling and humanization.
    pub fn apply(&self, notes: &mut [Note]) {
        // 1. Speed scaling — everything is relative to the original timeline.
        if self.speed != 1.0 {
            for n in notes.iter_mut() {
                n.start_ms /= self.speed;
                n.duration_ms /= self.speed;
            }
        }

        // 2. Timing jitter (± timing_variation_ms * level).
        let tvar = self.cfg.timing_variation_ms * self.level;
        if tvar > 0.0 {
            let mut rng = rand::thread_rng();
            for n in notes.iter_mut() {
                let j = rng.gen_range(-tvar..=tvar);
                n.start_ms = (n.start_ms + j).max(0.0);
            }
        }

        // 3. Duration jitter + velocity-based hold.
        let dvar = self.cfg.duration_variation_ms * self.level;
        let mut rng = rand::thread_rng();
        for n in notes.iter_mut() {
            if self.cfg.use_velocity_for_duration {
                // velocity 0..127 -> factor 0.6..1.1
                let factor = 0.6 + (n.velocity as f64 / 127.0) * 0.5;
                n.duration_ms *= factor;
            }
            if dvar > 0.0 {
                let j = rng.gen_range(-dvar..=dvar);
                n.duration_ms = (n.duration_ms + j).max(5.0);
            }
        }

        // 4. Chord arpeggiation: stagger notes that start within ~5ms of each
        //    other so they don't all trigger on the exact same tick.
        let cdel = self.cfg.chord_delay_ms * self.level;
        if cdel > 0.0 {
            notes.sort_by(|a, b| {
                a.start_ms
                    .partial_cmp(&b.start_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let mut i = 0;
            while i < notes.len() {
                let base = notes[i].start_ms;
                let mut k = 0usize;
                let mut j = i;
                while j < notes.len() && (notes[j].start_ms - base).abs() <= 5.0 {
                    notes[j].start_ms += k as f64 * cdel;
                    k += 1;
                    j += 1;
                }
                i = j;
            }
        }

        // Final sort so downstream scheduling is monotonic.
        notes.sort_by(|a, b| {
            a.start_ms
                .partial_cmp(&b.start_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}
