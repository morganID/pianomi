//! Real-time playback engine.
//!
//! Schedules humanized notes against a wall clock and simulates key presses
//! with [`enigo`]. A single polling loop drives both key-down (when a note's
//! start time arrives) and key-up (when its computed end time arrives). Held
//! keys are reference-counted so overlapping notes on the same key don't cut
//! each other off.
//!
//! In `dry_run` mode no input device is touched; events are logged instead,
//! which is handy for checking timing/mapping without Roblox open.
//!
//! The engine can optionally log to a shared buffer (used by the GUI) instead
//! of `stdout`, and can be interrupted early via a stop flag.

use crate::config::PlayerConfig;
use crate::humanizer::Humanizer;
use crate::key_mapper::{Binding, KeyMapper};
#[cfg(not(target_os = "macos"))]
use crate::keycode::resolve_key;
use crate::midi_out::MidiOut;
use crate::midi_parser::{Note, Song};
use crate::note_name;
use anyhow::Result;
#[cfg(not(target_os = "macos"))]
use enigo::Keyboard;
use enigo::{Direction, Enigo, Settings};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

/// Where the notes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Simulate keystrokes in the focused window (the Roblox piano path).
    Keys,
    /// Send MIDI note messages instead.
    Midi,
}

/// What a scheduled event does when its time arrives.
#[derive(Debug, Clone)]
pub enum Target {
    /// Press a key in the focused window.
    Key(Binding),
    /// Emit a MIDI note on the open port.
    Midi { channel: u8, note: u8, velocity: u8 },
}

impl Target {
    /// Stable identity used for reference counting. Two notes on the same
    /// channel and pitch are the same physical event.
    pub fn id(&self) -> String {
        match self {
            Target::Key(b) => b.id(),
            Target::Midi {
                channel,
                note,
                velocity: _,
            } => format!("midi:{}:{}", channel + 1, note),
        }
    }

    /// Human-readable form for dry-run logs.
    pub fn describe(&self) -> String {
        match self {
            Target::Key(b) => b.id(),
            Target::Midi { channel, note, .. } => {
                format!("ch{} {} ({})", channel + 1, note, note_name(*note))
            }
        }
    }

    /// Same, but for a note-on, where the velocity is worth showing.
    pub fn describe_on(&self) -> String {
        match self {
            Target::Midi {
                channel,
                note,
                velocity,
            } => format!(
                "ch{} {} ({}) vel {}",
                channel + 1,
                note,
                note_name(*note),
                velocity
            ),
            other => other.describe(),
        }
    }
}

pub struct Player {
    /// `None` in dry-run mode (no input device opened) and in MIDI mode.
    enigo: Option<Enigo>,
    mapper: KeyMapper,
    humanizer: Humanizer,
    config: PlayerConfig,
    /// Keys / notes currently held down (so overlapping notes on the same key
    /// don't release prematurely). The value is the target plus its refcount.
    held: HashMap<String, (Target, usize)>,
    /// When set, log lines are appended here (used by the GUI). When `None`,
    /// lines are printed to stdout (the CLI default).
    logger: Option<Arc<Mutex<Vec<String>>>>,
    /// When set, the playback loops check this flag and stop early.
    stop: Option<Arc<AtomicBool>>,
    /// Key labels we already complained about (avoids log spam).
    warned: HashSet<String>,
    /// Notes that could not be retriggered because the same key was already
    /// held (a genuine same-key overlap). Reported at the end of a run.
    dropped: usize,
    /// Which output path this player drives.
    mode: OutputMode,
    /// Open MIDI connection; `None` when playing to the keyboard, or when
    /// previewing MIDI in dry-run mode.
    midi: Option<MidiOut>,
}

impl Player {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mapper: KeyMapper,
        humanizer: Humanizer,
        config: PlayerConfig,
        dry_run: bool,
        mode: OutputMode,
        midi: Option<MidiOut>,
        logger: Option<Arc<Mutex<Vec<String>>>>,
        stop: Option<Arc<AtomicBool>>,
    ) -> Result<Self> {
        // Only the keyboard path needs Enigo; MIDI mode must not ask macOS for
        // Accessibility at all.
        let enigo = if dry_run || mode == OutputMode::Midi {
            None
        } else {
            Some(Enigo::new(&Settings::default()).map_err(|e| match e {
                enigo::NewConError::NoPermission => anyhow::anyhow!(
                    "macOS blocked synthetic input (Accessibility not granted).\n\
                     Grant it to the app that launches this binary — your terminal — in\n\
                     System Settings ▸ Privacy & Security ▸ Accessibility, then relaunch."
                ),
                other => anyhow::anyhow!("failed to init Enigo: {:?}", other),
            })?)
        };
        Ok(Player {
            enigo,
            mapper,
            humanizer,
            config,
            held: HashMap::new(),
            logger,
            stop,
            warned: HashSet::new(),
            mode,
            midi,
            dropped: 0,
        })
    }

    /// Emit a log line — to the shared buffer if present, otherwise stdout.
    fn log(&self, msg: &str) {
        match &self.logger {
            Some(buf) => buf.lock().unwrap().push(msg.to_string()),
            None => println!("{}", msg),
        }
    }

    /// Whether the stop flag has been set (returns false when no flag exists).
    fn should_stop(&self) -> bool {
        self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
    }

    /// Play `song`, repeating `loops` times (minimum 1).
    pub fn play(&mut self, song: &Song, loops: u32, dry_run: bool) -> Result<()> {
        let loops = loops.max(1);
        let mut base = song.clone();
        self.humanizer.apply(&mut base.notes);
        let song_len = base.length_ms.max(1.0);

        if !dry_run {
            self.log(&format!(
                "{} — playback starts in {}s …",
                if self.mode == OutputMode::Midi {
                    "Notes are going out as MIDI"
                } else {
                    "Focus the Roblox piano window now"
                },
                self.config.start_delay_ms as f64 / 1000.0
            ));
            sleep(Duration::from_millis(self.config.start_delay_ms));
            if self.should_stop() {
                self.release_all(dry_run)?;
                self.log("Stopped before playback started.");
                return Ok(());
            }
        } else {
            self.log("[dry-run] no keys will be pressed");
        }

        for lap in 0..loops {
            if self.should_stop() {
                break;
            }
            let mut notes = base.notes.clone();
            let offset = lap as f64 * (song_len + self.config.loop_gap_ms as f64);
            for n in notes.iter_mut() {
                n.start_ms += offset;
            }
            if loops > 1 {
                self.log(&format!("── loop {}/{} ──", lap + 1, loops));
            }
            self.run(&notes, dry_run)?;
        }

        if self.dropped > 0 {
            self.log(&format!(
                "warning: {} note(s) could not be retriggered — the same key was \
                 already held at that instant.",
                self.dropped
            ));
        }

        // Safety: release anything still held.
        self.release_all(dry_run)?;
        Ok(())
    }

    /// Core scheduler.
    fn run(&mut self, notes: &[Note], dry_run: bool) -> Result<()> {
        // Build (start_ms, target, end_ms) tuples.
        let mut events: Vec<(f64, Target, f64)> = Vec::new();
        for n in notes {
            let end = (n.start_ms + n.duration_ms).max(n.start_ms + self.config.min_hold_ms);
            let target = match self.mode {
                // MIDI carries the note itself, so nothing needs to be mapped:
                // every pitch the file contains goes out, 0..127.
                OutputMode::Midi => match self.mapper.note_after_transpose(n.note) {
                    Some(note) => Target::Midi {
                        channel: n.channel,
                        note,
                        velocity: n.velocity,
                    },
                    None => continue, // transposed out of MIDI range
                },
                // The keyboard path can only play notes that have a key.
                OutputMode::Keys => match self.mapper.key_for_note(n.note) {
                    Some(k) => Target::Key(k.clone()),
                    None => continue,
                },
            };
            events.push((n.start_ms, target, end));
        }
        events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // One physical key can only sound one note at a time.
        //
        // Two things break that assumption in real files, and both used to drop
        // notes silently:
        //   * the same pitch note-on'd several times on the same tick (pedal or
        //     export artefacts) — that is one press, not three;
        //   * a note that outlasts the next note on the same key, which can
        //     never be retriggered while the first is still held.
        //
        // So: collapse notes that would press the same key in the same
        // millisecond, and trim each surviving note at the next one's start.
        // Trimming keeps the release *at* the retrigger's tick, and releases are
        // processed before presses, so the key is always free when the new note
        // lands.
        {
            let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
            for (i, (_, target, _)) in events.iter().enumerate() {
                by_key.entry(target.id()).or_default().push(i);
            }

            let mut drop = vec![false; events.len()];
            for idxs in by_key.values() {
                // `events` is sorted by start time, so each list is already in
                // ascending start order.
                let mut prev: Option<usize> = None;
                let mut prev_tick: Option<u64> = None;
                for &i in idxs {
                    let tick = events[i].0.round() as u64;
                    if prev_tick == Some(tick) {
                        // Same key, same instant: one physical press. Keep the
                        // longest of the collapsed durations so a stray short
                        // duplicate cannot cut the real note off early.
                        if let Some(p) = prev {
                            if events[i].2 > events[p].2 {
                                events[p].2 = events[i].2;
                            }
                        }
                        drop[i] = true;
                        continue;
                    }
                    if let Some(p) = prev {
                        // Trim only when it actually overlaps. Assigning the
                        // next start unconditionally would *stretch* every
                        // short note up to the following one on the same key,
                        // which leaves the key held long after the note ended.
                        if events[p].2 > events[i].0 {
                            events[p].2 = events[i].0;
                        }
                    }
                    prev = Some(i);
                    prev_tick = Some(tick);
                }
            }

            let mut i = 0usize;
            events.retain(|_| {
                let keep = !drop[i];
                i += 1;
                keep
            });
        }

        // Group release events by (rounded) end time for efficient lookup.
        let mut releases: BTreeMap<u64, Vec<Target>> = BTreeMap::new();
        for (_, t, e) in &events {
            releases
                .entry((*e).round() as u64)
                .or_default()
                .push(t.clone());
        }

        let tick = Duration::from_millis(self.config.tick_ms.max(1));
        let start = std::time::Instant::now();
        let mut ei = 0usize;
        // In dry-run the schedule is printed, not played, so there is nothing to
        // wait for. A virtual clock that jumps straight to the next event turns
        // a four-minute preview into an instant dump you can diff.
        let mut virtual_now: u64 = 0;

        while ei < events.len() || !releases.is_empty() {
            if self.should_stop() {
                break;
            }
            let elapsed = if dry_run {
                virtual_now
            } else {
                start.elapsed().as_millis() as u64
            };

            // Release first, then press. Order matters: a legato repeat is
            // encoded as note-off and note-on on the *same* key at the same
            // tick, so pressing before releasing would find the key still held,
            // suppress the press, and then release it — the note would never
            // sound at all. Releasing first frees the key and the repeat lands.
            let due: Vec<(u64, Vec<Target>)> = releases
                .range(..=elapsed)
                .map(|(t, v)| (*t, v.clone()))
                .collect();
            for (_, targets) in &due {
                for target in targets {
                    self.release(target, dry_run)?;
                }
            }
            for (t, _) in &due {
                releases.remove(t);
            }

            // Press notes whose start time has arrived.
            while ei < events.len() && (events[ei].0.round() as u64) <= elapsed {
                let target = &events[ei].1;
                self.press(target, dry_run)?;
                ei += 1;
            }

            if dry_run {
                // Everything due at this instant has been emitted, so the next
                // press or release is strictly in the future — the clock always
                // advances and the loop terminates.
                let next_press = events.get(ei).map(|e| e.0.round() as u64);
                let next_release = releases.keys().next().copied();
                virtual_now = match (next_press, next_release) {
                    (Some(a), Some(b)) => a.min(b),
                    (Some(a), None) => a,
                    (None, Some(b)) => b,
                    (None, None) => break,
                };
            } else {
                sleep(tick);
            }
        }
        Ok(())
    }

    // --- low level key handling -------------------------------------------

    /// Send one key event to the OS.
    ///
    /// # Modifiers are per-event, never held
    ///
    /// A black key (`!`, `Q`, `S`, …) is the *same physical key* as the white
    /// note below it, distinguished only by Shift, and the 27 outer keys of the
    /// in-game piano add Ctrl on top of the same 36 keys. Holding a real
    /// modifier key for the length of a note is a trap for playback:
    ///
    /// * Modifier state is global and asynchronous, so at speed a stray
    ///   Shift-up can land between one note's key-down and the next note's
    ///   key-down and turn a natural into a sharp.
    /// * Every plain key pressed while Shift happens to be down is delivered as
    ///   its sharp (`8` → `*`, `s` → `S`), which silently rewrites chords.
    ///
    /// So each note is exactly one event whose own flags carry the modifier
    /// (see [`crate::keycode::post_key`]), or — in `shift_mode = "key"` — a
    /// modifier press/release scoped tightly around that single event.
    fn dispatch(&mut self, target: &Target, direction: Direction) -> Result<()> {
        // MIDI first: it needs no Enigo and must work even when `enigo` is None.
        if let Target::Midi {
            channel,
            note,
            velocity,
        } = target
        {
            let Some(out) = self.midi.as_mut() else {
                return Ok(()); // dry-run preview: the log line is the whole point
            };
            match direction {
                Direction::Press => out.note_on(*channel, *note, *velocity)?,
                Direction::Release => out.note_off(*channel, *note)?,
                Direction::Click => {}
            }
            return Ok(());
        }

        let Target::Key(b) = target else {
            return Ok(());
        };

        // `None` means dry-run or MIDI mode: no key events to post.
        if self.enigo.is_none() {
            return Ok(());
        }

        let down = match direction {
            Direction::Press => true,
            Direction::Release => false,
            Direction::Click => return Ok(()),
        };

        #[cfg(target_os = "macos")]
        {
            let mode = crate::keycode::parse_shift_mode(&self.config.shift_mode);
            if !crate::keycode::post_key(&b.label, down, mode, b.ctrl)? {
                // Warn once per distinct label so a bad config doesn't flood the log.
                if self.warned.insert(b.id()) {
                    self.log(&format!(
                        "! key {:?} has no known keycode on this platform — skipped",
                        b.id()
                    ));
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // Non-macOS still goes through enigo, where `Key::Unicode` carries
            // the character (and its shift) itself. Ctrl bindings are not
            // supported on that path yet.
            let e = self.enigo.as_mut().expect("checked above");
            let Some((k, _shifted)) = resolve_key(&b.label) else {
                if self.warned.insert(b.id()) {
                    self.log(&format!(
                        "! key {:?} has no known keycode on this platform — skipped",
                        b.id()
                    ));
                }
                return Ok(());
            };
            let dir = if down {
                Direction::Press
            } else {
                Direction::Release
            };
            e.key(k, dir)
                .map_err(|err| anyhow::anyhow!("key {:?} dispatch failed: {:?}", b.id(), err))?;
        }

        Ok(())
    }

    fn press(&mut self, target: &Target, dry_run: bool) -> Result<()> {
        let id = target.id();
        let slot = self.held.entry(id.clone()).or_insert_with(|| (target.clone(), 0));
        slot.1 += 1;
        if slot.1 > 1 {
            self.dropped += 1;
        }
        if slot.1 == 1 {
            if dry_run {
                self.log(&format!("[PRESS] {}", target.describe_on()));
            } else {
                self.dispatch(target, Direction::Press)?;
            }
        }
        Ok(())
    }

    fn release(&mut self, target: &Target, dry_run: bool) -> Result<()> {
        let id = target.id();
        if let Some((_, count)) = self.held.get_mut(&id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.held.remove(&id);
                if dry_run {
                    self.log(&format!("[RELEASE] {}", target.describe()));
                } else {
                    self.dispatch(target, Direction::Release)?;
                }
            }
        }
        Ok(())
    }

    fn release_all(&mut self, dry_run: bool) -> Result<()> {
        let targets: Vec<Target> = self.held.drain().map(|(_, (target, _))| target).collect();
        for t in targets {
            if dry_run {
                self.log(&format!("[RELEASE] {}", t.describe()));
                continue;
            }
            self.dispatch(&t, Direction::Release)?;
        }
        // Belt and braces for MIDI: a stuck note on the receiver is far more
        // annoying than two extra control messages.
        if let Some(out) = self.midi.as_mut() {
            out.all_notes_off()?;
        }
        // No Shift bookkeeping is needed any more: every event carries its own
        // modifier flag, so no global modifier state can be left behind.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, PlayerConfig};
    use crate::humanizer::Humanizer;
    use crate::key_mapper::KeyMapper;
    use crate::midi_parser::{Note, Song};
    use std::sync::{Arc, Mutex};

    /// Play a note list in dry-run and return the log lines.
    fn play_notes(notes: Vec<Note>, humanize: u8) -> Vec<String> {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let humanizer = Humanizer::new(humanize, 1.0, cfg.humanize.clone());
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut player = Player::new(
            mapper,
            humanizer,
            PlayerConfig::default(),
            true,
            OutputMode::Keys,
            None,
            Some(log.clone()),
            None,
        )
        .expect("dry-run player");

        let song = Song {
            notes,
            length_ms: 10_000.0,
            ..Default::default()
        };
        player.play(&song, 1, true).expect("play");
        let out = log.lock().unwrap().clone();
        out
    }

    fn note(pitch: u8, start_ms: f64, duration_ms: f64) -> Note {
        Note {
            note: pitch,
            channel: 0,
            start_ms,
            duration_ms,
            velocity: 100,
        }
    }

    fn presses(lines: &[String]) -> usize {
        lines.iter().filter(|l| l.starts_with("[PRESS]")).count()
    }

    #[test]
    fn legato_repeats_are_all_pressed() {
        // The same key released exactly when the next note starts. Releasing
        // after pressing used to swallow every repeat.
        let notes = vec![
            note(60, 0.0, 500.0),
            note(60, 500.0, 500.0),
            note(60, 1000.0, 500.0),
        ];
        assert_eq!(presses(&play_notes(notes, 0)), 3);
    }

    #[test]
    fn duplicate_note_ons_on_the_same_tick_are_one_press() {
        // Three note-ons for one key at one instant is a single physical press;
        // it must not stick the key and swallow everything after it.
        let notes = vec![
            note(60, 0.0, 500.0),
            note(60, 500.0, 500.0),
            note(60, 500.0, 500.0),
            note(60, 500.0, 500.0),
            note(60, 1000.0, 500.0),
        ];
        let log = play_notes(notes, 0);
        // 3 distinct presses: t=0, t=500 (triple collapsed), t=1000.
        assert_eq!(presses(&log), 3);
        assert!(!log.iter().any(|l| l.contains("could not be retriggered")));
    }

    #[test]
    fn collapsed_duplicates_keep_the_longest_duration() {
        // Spring_Thief has F5 note-on'd three times on one tick: 120, 120 and
        // 479 ms. One press, and it must last the 479 ms, not the first 120.
        let notes = vec![
            note(77, 0.0, 120.0),
            note(77, 0.0, 120.0),
            note(77, 0.0, 479.0),
        ];
        let log = play_notes(notes, 0);
        assert_eq!(presses(&log), 1);
        // The release is scheduled from the longest end, rounded to 479 ms.
        assert!(
            log.iter().any(|l| l.starts_with("[RELEASE]")),
            "the collapsed note must still be released"
        );
    }

    #[test]
    fn a_short_note_is_not_stretched_to_the_next_one() {
        // A 100 ms note followed by the same key at 500 ms must be released at
        // 100 ms. Assigning the next start unconditionally held the key for
        // 400 ms longer than the note.
        let notes = vec![note(60, 0.0, 100.0), note(60, 500.0, 100.0)];
        let log = play_notes(notes, 0);
        assert_eq!(presses(&log), 2);
        // The log is chronological, so a RELEASE must appear before the second
        // PRESS. If the note were stretched to 500 ms they would be adjacent
        // the other way round.
        let second_press = log
            .iter()
            .enumerate()
            .filter(|(_, l)| l.starts_with("[PRESS]"))
            .nth(1)
            .map(|(i, _)| i)
            .expect("two presses");
        let released_before = log[..second_press]
            .iter()
            .filter(|l| l.starts_with("[RELEASE]"))
            .count();
        assert_eq!(
            released_before, 1,
            "first note must be released before the second press"
        );
    }

    #[test]
    fn a_note_outlasting_the_next_one_is_trimmed_not_dropped() {
        // 0..2000 overlaps the note at 1000. The first must be cut at 1000 so
        // the second can sound.
        let notes = vec![note(60, 0.0, 2000.0), note(60, 1000.0, 500.0)];
        let log = play_notes(notes, 0);
        assert_eq!(presses(&log), 2);
    }

    #[test]
    fn humanize_zero_plays_durations_as_written() {
        // A quiet note used to be shortened by up to 40% even at level 0.
        let notes = vec![Note {
            velocity: 1,
            ..note(60, 0.0, 1000.0)
        }];
        let cfg = Config::load(None).expect("builtin config");
        let humanizer = Humanizer::new(0, 1.0, cfg.humanize.clone());
        let mut ns = notes;
        humanizer.apply(&mut ns);
        assert!((ns[0].duration_ms - 1000.0).abs() < 1e-9);
        assert!((ns[0].start_ms - 0.0).abs() < 1e-9);
    }

    #[test]
    fn velocity_never_lengthens_a_note() {
        // Lengthening pushed loud notes past the next one on the same key,
        // which dropped that next note. Jitter is zeroed so only the velocity
        // term is under test.
        let mut cfg = Config::load(None).expect("builtin config");
        cfg.humanize.timing_variation_ms = 0.0;
        cfg.humanize.duration_variation_ms = 0.0;
        cfg.humanize.chord_delay_ms = 0.0;
        let humanizer = Humanizer::new(100, 1.0, cfg.humanize.clone());
        let mut ns = vec![
            Note {
                velocity: 0,
                ..note(60, 0.0, 1000.0)
            },
            Note {
                velocity: 127,
                ..note(62, 0.0, 1000.0)
            },
        ];
        humanizer.apply(&mut ns);
        for n in &ns {
            assert!(
                n.duration_ms <= 1000.0 + 1e-9,
                "velocity {} lengthened the note to {}",
                n.velocity,
                n.duration_ms
            );
        }
        // ...but it still shortens the quiet one.
        assert!(ns[0].duration_ms < 1000.0);
    }
}
