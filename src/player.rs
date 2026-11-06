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
use crate::midi_parser::{ControlEvent, Note, Song};
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

    /// Identity of the *physical* resource this event occupies.
    ///
    /// On the keyboard path a sharp (`!`), its white neighbour (`1`) and the
    /// Ctrl-layer note (`Ctrl+1`) are all the same physical key. Only one can
    /// be down at a time, so scheduling decisions must be made on this id,
    /// while [`Target::id`] stays the finer-grained label used for refcounting.
    pub fn physical_id(&self) -> String {
        match self {
            Target::Key(b) => b.physical_id(),
            Target::Midi { .. } => self.id(),
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

/// A resolved schedule: `(start_ms, target, end_ms)`, sorted by start.
struct Schedule {
    events: Vec<(f64, Target, f64)>,
    /// Notes dropped because another *pitch* needed the same physical key at
    /// the same instant (only when multiplexing is disabled).
    physical_drops: usize,
    /// Notes shifted forward so a second pitch on the same physical key can
    /// still get its own key-down.
    multiplexed: usize,
    /// Consecutive notes on one physical key whose release still lands less
    /// than `retrigger_gap_ms` before the next press. `min_hold_ms` wins in
    /// these cases: there simply is not enough room between the two onsets to
    /// hold the first note and still leave a key-up window.
    tight_retriggers: usize,
}

/// Count release/press pairs on the same physical key that do not have a full
/// `gap` of real key-up time between them. This is the measurement that says
/// whether the schedule can actually be played, rather than an assumption.
fn count_tight_retriggers(events: &[(f64, Target, f64)], gap: f64) -> usize {
    if gap <= 0.0 {
        return 0;
    }
    let mut by_key: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    for (start, target, end) in events {
        by_key
            .entry(target.physical_id())
            .or_default()
            .push((*start, *end));
    }
    let mut tight = 0usize;
    for mut list in by_key.into_values() {
        list.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for pair in list.windows(2) {
            if pair[1].0 - pair[0].1 < gap - 1e-6 {
                tight += 1;
            }
        }
    }
    tight
}

/// Turn parsed notes into timed key/MIDI events.
///
/// ## Onsets are sacred
///
/// `start_ms` is copied verbatim and never adjusted — that is the part of the
/// music the ear reads as timing. Every correction this function makes lives
/// on the *end* of a note: a hold can only ever be shortened, never stretched
/// past a later note. Shortening can never cost a note; a long hold can,
/// because a physical key that is still down cannot produce a new key-down.
///
/// ## One physical key, three notes
///
/// The Virtual Piano reuses each QWERTY key three times (plain / Shift / Ctrl).
/// `1` (C2), `!` (C#2) and `Ctrl+1` (A0) are the same physical key. Grouping
/// only by the label used to leave a `!` scheduled while `1` was still held —
/// the OS delivered no second key-down and the sharp was silently lost. On the
/// Animenz Ghibli suite that was 284 notes of *different pitches*.
/// So grouping and trimming use [`Target::physical_id`] instead.
///
/// ## A key-up needs its own frame
///
/// Trimming a note's release to *exactly* the next note's start puts the
/// key-up and the next key-down in one scheduler pass — back-to-back, with no
/// real time in between. Roblox samples `IsKeyDown` once per frame, so it can
/// see only "still down" and never fires `InputBegan` for the second note.
/// `retrigger_gap_ms` pulls the release back by that much real time. Two
/// different pitches sharing a key are a semitone apart and are effectively
/// never meant to sound together, so shortening the earlier hold is musically
/// free; shifting its *start* would not be. The release is clamped to
/// `start + min_hold_ms` so no note becomes too short to register.
fn build_schedule(
    notes: &[Note],
    config: &PlayerConfig,
    mode: OutputMode,
    mapper: &KeyMapper,
) -> Schedule {
    let mut events: Vec<(f64, Target, f64)> = Vec::with_capacity(notes.len());
    for n in notes {
        let target = match mode {
            // MIDI carries the note itself, so nothing needs to be mapped:
            // every pitch the file contains goes out, 0..127.
            OutputMode::Midi => match mapper.note_after_transpose(n.note) {
                Some(note) => Target::Midi {
                    channel: n.channel,
                    note,
                    velocity: n.velocity,
                },
                None => continue, // transposed out of MIDI range
            },
            // The keyboard path can only play notes that have a key.
            OutputMode::Keys => match mapper.key_for_note(n.note) {
                Some(k) => Target::Key(k.clone()),
                None => continue,
            },
        };

        // A minimum hold guarantees the game sees a real press/release pair.
        let mut hold = n.duration_ms.max(config.min_hold_ms);
        // Optional keyboard-only cap: shorten long holds so they stop blocking
        // the next note. Never below min_hold, or notes stop registering at
        // all. MIDI output ignores the cap and keeps the written duration.
        if mode == OutputMode::Keys && config.max_hold_ms > 0.0 {
            hold = hold.min(config.max_hold_ms.max(config.min_hold_ms));
        }
        events.push((n.start_ms, target, n.start_ms + hold));
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // One physical key cannot fire two key-downs at the same instant. Three
    // cases, all resolved per physical key:
    //   * the SAME pitch twice on one tick  -> one press (export artefact);
    //   * a DIFFERENT pitch on the same tick -> serialize it (`same_key_gap_ms`)
    //     or, if disabled, drop it and report;
    //   * a note outlasting the next one    -> trim its release to
    //     `next_start - retrigger_gap_ms`, so the key is observably up first.
    //
    // Serializing shifts a note's start, which can create a new collision with
    // a later note, so the pass repeats until the schedule is stable.
    let retrigger_gap = if mode == OutputMode::Keys {
        config.retrigger_gap_ms.max(0.0)
    } else {
        0.0
    };
    let gap = if mode == OutputMode::Keys && config.same_key_gap_ms > 0.0 {
        // The first of a multiplexed pair needs a full `min_hold_ms` AND must
        // be up for `retrigger_gap_ms` before the second goes down. Never
        // shorter than that, or one of the two presses cannot register.
        config
            .same_key_gap_ms
            .max(config.min_hold_ms + retrigger_gap)
    } else {
        0.0
    };
    let mut physical_drops = 0usize;
    let mut multiplexed = 0usize;

    for pass in 0..8 {
        events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (_, target, _)) in events.iter().enumerate() {
            by_key.entry(target.physical_id()).or_default().push(i);
        }

        let mut drop = vec![false; events.len()];
        let mut shifted = false;
        for idxs in by_key.values() {
            // `events` is sorted by start, so each list is ascending.
            let mut prev: Option<usize> = None;
            let mut cluster_tick: Option<u64> = None;
            let mut cluster_shift = 0.0;
            for &i in idxs {
                let tick = events[i].0.round() as u64;
                if cluster_tick == Some(tick) {
                    if let Some(p) = prev {
                        if events[p].1.id() == events[i].1.id() {
                            // Same pitch, same instant: one physical press.
                            // Keep the longest hold so a short duplicate cannot
                            // cut the real note off early.
                            if events[i].2 > events[p].2 {
                                events[p].2 = events[i].2;
                            }
                            drop[i] = true;
                            continue;
                        }
                        if gap > 0.0 {
                            // Two pitches, one key: press the first, release it,
                            // then press the second after `gap` ms. Both get a
                            // real key-down; the ear hears a chord.
                            cluster_shift += gap;
                            events[i].0 += cluster_shift;
                            events[i].2 += cluster_shift; // keep its duration
                            if events[p].2 > events[i].0 - retrigger_gap {
                                let floor = events[p].0 + config.min_hold_ms;
                                events[p].2 = (events[i].0 - retrigger_gap).max(floor);
                            }
                            if pass == 0 {
                                multiplexed += 1;
                            }
                            shifted = true;
                            prev = Some(i);
                            continue;
                        }
                        // Multiplexing disabled: a real second pitch is lost.
                        physical_drops += 1;
                        drop[i] = true;
                        continue;
                    }
                } else {
                    cluster_tick = Some(tick);
                    cluster_shift = 0.0;
                }
                if let Some(p) = prev {
                    // Shorten only when the release is closer than
                    // `retrigger_gap` to the next press. Assigning the next
                    // start unconditionally would *stretch* every short note
                    // up to the following one on the same key; clamping to the
                    // note's own min-hold floor keeps it from ever getting too
                    // short to register.
                    let deadline = events[i].0 - retrigger_gap;
                    if events[p].2 > deadline {
                        let floor = events[p].0 + config.min_hold_ms;
                        events[p].2 = deadline.max(floor);
                    }
                }
                prev = Some(i);
            }
        }

        let mut k = 0usize;
        events.retain(|_| {
            let keep = !drop[k];
            k += 1;
            keep
        });
        if !shifted {
            break;
        }
    }

    let tight_retriggers = count_tight_retriggers(&events, retrigger_gap);
    Schedule {
        events,
        physical_drops,
        multiplexed,
        tight_retriggers,
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
    /// Notes dropped because a *different* pitch needed the same physical key
    /// at the very same instant (only when multiplexing is disabled).
    physical_drops: usize,
    /// Different pitches on one physical key that were serialized into two
    /// consecutive presses instead of being dropped.
    multiplexed: usize,
    /// Release/press pairs on one physical key with no real key-up window.
    /// Reported at the end of a run instead of silently losing the note.
    tight_retriggers: usize,
    /// Which output path this player drives.
    mode: OutputMode,
    /// Key events are delivered into Roblox's event queue (`CGEventPostToPid`)
    /// rather than the system-wide HID tap. See [`Player::set_targeted`].
    targeted: bool,
    /// Events actually emitted this run (used by the trace summary). Dry-run
    /// presses count too — they are the schedule, just not posted to the OS.
    presses: usize,
    releases: usize,
    /// Open MIDI connection; `None` when playing to the keyboard, or when
    /// previewing MIDI in dry-run mode.
    midi: Option<MidiOut>,
}

/// Extra margin needed when events are queued into the target process instead
/// of being applied to the HID system immediately.
///
/// `CGEventPostToPid` delivers every event (verified 1:1 with an AppKit
/// receiver, even when the target app is not frontmost), but the receiving run
/// loop drains them in frame-sized batches. The Virtual Piano polls
/// `IsKeyDown` once per frame, so a key-up and the next key-down that are less
/// than one drain apart are both invisible — the game only ever sees "still
/// down" and the note is skipped. On the HID path a post changes the state
/// synchronously, so the stock 20/30 ms margins are enough; over a queue they
/// are marginal. Widening only affects *releases* (and the rare same-tick
/// collision), never note onsets, so musical timing is preserved.
///
/// Set `PlayerConfig::targeted_extra_gap_ms = 0` to turn the widening off for
/// an A/B comparison against the stock margins.
const TARGETED_RETRIGGER_GAP_MS: f64 = 50.0;
const TARGETED_SAME_KEY_GAP_MS: f64 = 60.0;

/// Resolve the timing config the scheduler should use.
///
/// Only targeted keyboard delivery changes anything: add
/// `targeted_extra_gap_ms` to the key-up window, with floors that cover a
/// couple of 60 Hz frames of queue-drain latency. `build_schedule` already
/// raises the multiplex gap to `min_hold_ms + retrigger_gap_ms`, so two
/// pitches sharing one physical key inherit the wider window automatically.
/// Note onsets are never touched by any of this.
///
/// This is a free function as well as [`Player::effective_config`] so the CLI
/// can print the same numbers the scheduler is about to use.
pub fn schedule_config_for(
    config: &PlayerConfig,
    mode: OutputMode,
    targeted: bool,
) -> PlayerConfig {
    let mut cfg = config.clone();
    if targeted && mode == OutputMode::Keys && cfg.targeted_extra_gap_ms > 0.0 {
        cfg.retrigger_gap_ms =
            (cfg.retrigger_gap_ms + cfg.targeted_extra_gap_ms).max(TARGETED_RETRIGGER_GAP_MS);
        cfg.same_key_gap_ms = cfg.same_key_gap_ms.max(TARGETED_SAME_KEY_GAP_MS);
    }
    cfg
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
            physical_drops: 0,
            multiplexed: 0,
            tight_retriggers: 0,
            targeted: false,
            presses: 0,
            releases: 0,
        })
    }

    /// Tell the scheduler that key events are queued into Roblox's event loop
    /// (`CGEventPostToPid`) rather than applied to the HID system immediately.
    ///
    /// The wider key-up margins only apply to the keyboard path; MIDI and
    /// dry-run are unaffected. Call this right after [`Player::new`], before
    /// [`Player::play`].
    pub fn set_targeted(&mut self, targeted: bool) {
        self.targeted = targeted;
    }

    /// The config the scheduler actually uses. See [`schedule_config_for`].
    pub fn effective_config(&self) -> PlayerConfig {
        schedule_config_for(&self.config, self.mode, self.targeted)
    }

    /// Emit a log line — to the shared buffer if present, otherwise stdout.
    ///
    /// The line is also mirrored into the trace file. This is what makes GUI
    /// warnings visible: a bundle launched with `open` has stdout attached to
    /// `/dev/null`, so without this the `warning: N release/press pair(s)`
    /// diagnostics were produced and immediately thrown away.
    fn log(&self, msg: &str) {
        crate::trace::note(msg);
        match &self.logger {
            Some(buf) => buf.lock().unwrap().push(msg.to_string()),
            None => println!("{}", msg),
        }
    }

    /// One tab-separated row per scheduled key event.
    ///
    /// `scheduled` is the time the *schedule* asked for; `actual` is the wall
    /// clock reading when the loop got round to it. A growing drift is the
    /// signature of a playback thread being stalled (the old `pgrep` bug), and
    /// a small `RELEASE -> PRESS` gap on one physical key is the signature of
    /// a note the game's once-per-frame `IsKeyDown` poll can never see.
    fn trace_key(
        &self,
        scheduled: f64,
        actual: u64,
        kind: &str,
        target: &Target,
        held: Option<usize>,
    ) {
        if !crate::trace::enabled() {
            return;
        }
        #[cfg(target_os = "macos")]
        let dest = {
            let pid = crate::keycode::event_target_pid();
            if pid > 0 {
                format!("pid:{pid}")
            } else if self.targeted {
                // Dry-run preview of a Direct-mode schedule: nothing is posted,
                // but the margins are the ones CGEventPostToPid would get.
                "targeted(preview)".to_string()
            } else {
                "HID".to_string()
            }
        };
        #[cfg(not(target_os = "macos"))]
        let dest = "enigo".to_string();

        crate::trace::line(&format!(
            "E\t{:10.1}\t{:10}\t{:+8.1}\t{:<13}\t{:<12}\t{:<10}\theld={}\tdest={}",
            scheduled,
            actual,
            actual as f64 - scheduled,
            kind,
            target.id(),
            target.physical_id(),
            held.map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string()),
            dest
        ));
    }

    /// Close the trace with the counters for this run.
    fn finish_trace(&self) {
        crate::trace::end(&[
            format!(
                "summary: presses={} releases={} dropped={} physical_drops={} multiplexed={} tight_retriggers={}",
                self.presses,
                self.releases,
                self.dropped,
                self.physical_drops,
                self.multiplexed,
                self.tight_retriggers
            ),
            "counters describe the whole play() call, all loops included".to_string(),
        ]);
    }

    /// Whether the stop flag has been set (returns false when no flag exists).
    fn should_stop(&self) -> bool {
        self.stop.as_ref().is_some_and(|s| s.load(Ordering::SeqCst))
    }

    /// Play `song`, repeating `loops` times (minimum 1).
    pub fn play(&mut self, song: &Song, loops: u32, dry_run: bool) -> Result<()> {
        let loops = loops.max(1);
        // A Player instance can be reused (the GUI replays songs), so counters
        // must describe this run only.
        self.dropped = 0;
        self.physical_drops = 0;
        self.multiplexed = 0;
        self.tight_retriggers = 0;
        self.presses = 0;
        self.releases = 0;
        let mut base = song.clone();
        self.humanizer.apply(&mut base.notes);
        let song_len = base.length_ms.max(1.0);
        // Targeted delivery widens the key-up margins. Every log line below
        // must describe the values the scheduler really used, not the raw
        // config, or the warnings would be misleading.
        let effective = self.effective_config();

        // Start the trace before any log line so diagnostics land in the file.
        // The header records the *effective* margins, which in Direct mode are
        // wider than the raw config — otherwise a trace would be misleading.
        if let Ok(path) = crate::trace::begin(&[
            format!("mode={:?} targeted={} dry_run={}", self.mode, self.targeted, dry_run),
            format!(
                "effective: retrigger_gap_ms={:.0} same_key_gap_ms={:.0} min_hold_ms={:.0} max_hold_ms={:.0} tick_ms={}",
                effective.retrigger_gap_ms,
                effective.same_key_gap_ms,
                effective.min_hold_ms,
                effective.max_hold_ms,
                effective.tick_ms
            ),
            format!(
                "song: notes={} length_ms={:.1} loops={} humanize={}",
                base.notes.len(),
                song_len,
                loops,
                self.humanizer.level_pct()
            ),
        ]) {
            // The CLI already announces the path before loading the song; only
            // the GUI (stdout on /dev/null) needs it echoed into the log buffer.
            if crate::trace::enabled() && self.logger.is_some() {
                self.log(&format!("Trace       : {}", path.display()));
            }
        }

        if !dry_run {
            let who = if self.mode == OutputMode::Midi {
                "Notes are going out as MIDI"
            } else if self.targeted {
                "Keys are posted straight to the target process (no focus needed)"
            } else {
                "Focus the Roblox piano window now"
            };
            self.log(&format!(
                "{} — playback starts in {}s …",
                who,
                self.config.start_delay_ms as f64 / 1000.0
            ));
            if self.targeted && self.mode == OutputMode::Keys {
                self.log(&format!(
                    "Direct mode   : queued into the target's event loop — key-up {:.0} ms, \
                     shared keys {:.0} ms (onsets unchanged)",
                    effective.retrigger_gap_ms,
                    effective
                        .same_key_gap_ms
                        .max(effective.min_hold_ms + effective.retrigger_gap_ms.max(0.0))
                ));
            }
            sleep(Duration::from_millis(self.config.start_delay_ms));
            if self.should_stop() {
                self.release_all(dry_run)?;
                self.log("Stopped before playback started.");
                self.finish_trace();
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
            let mut ctrls = base.control_events.clone();
            let offset = lap as f64 * (song_len + self.config.loop_gap_ms as f64);
            for n in notes.iter_mut() {
                n.start_ms += offset;
            }
            for c in ctrls.iter_mut() {
                c.at_ms += offset;
            }
            if loops > 1 {
                self.log(&format!("── loop {}/{} ──", lap + 1, loops));
            }
            self.run(&notes, &ctrls, dry_run)?;
        }

        if self.dropped > 0 {
            self.log(&format!(
                "warning: {} note(s) could not be retriggered — the same key was \
                 already held at that instant.",
                self.dropped
            ));
        }
        if self.multiplexed > 0 {
            self.log(&format!(
                "note: {} pitch(es) shared a physical key with another note on \
                 the same tick; they were played as separate presses \
                 {} ms apart (two key-downs instead of one).",
                self.multiplexed,
                effective
                    .same_key_gap_ms
                    .max(effective.min_hold_ms + effective.retrigger_gap_ms.max(0.0))
                    .round() as u64
            ));
        }
        if self.tight_retriggers > 0 && effective.retrigger_gap_ms > 0.0 {
            self.log(&format!(
                "warning: {} release/press pair(s) on one physical key still have \
                 less than {:.0} ms of key-up time — there was not enough room \
                 between the two onsets to keep min_hold_ms({:.0}). Lower \
                 min_hold_ms or retrigger_gap_ms if these notes are missing in game.",
                self.tight_retriggers, effective.retrigger_gap_ms, effective.min_hold_ms
            ));
        }
        if self.physical_drops > 0 {
            self.log(&format!(
                "note: {} note(s) shared a physical key with a different note on \
                 the same tick (e.g. C2 + C#2) and were dropped. Set \
                 player.same_key_gap_ms > 0 to play both instead.",
                self.physical_drops
            ));
        }

        // Safety: release anything still held.
        self.release_all(dry_run)?;
        self.finish_trace();
        Ok(())
    }

    /// Core scheduler.
    fn run(&mut self, notes: &[Note], controls: &[ControlEvent], dry_run: bool) -> Result<()> {
        let effective = self.effective_config();
        let schedule = build_schedule(notes, &effective, self.mode, &self.mapper);
        let events = schedule.events;
        self.physical_drops += schedule.physical_drops;
        self.multiplexed += schedule.multiplexed;
        self.tight_retriggers += schedule.tight_retriggers;

        // Group release events by (rounded) end time for efficient lookup.
        let mut releases: BTreeMap<u64, Vec<Target>> = BTreeMap::new();
        for (_, t, e) in &events {
            releases
                .entry((*e).round() as u64)
                .or_default()
                .push(t.clone());
        }

        // Control changes (sustain, volume, pan, …) only travel on the MIDI path;
        // a Roblox key window has no way to receive them. Build a time-ordered
        // schedule of the ones this song carries.
        let mut ctrl_events: Vec<(f64, u8, u8, u8)> = Vec::new();
        if self.mode == OutputMode::Midi {
            for c in controls {
                ctrl_events.push((c.at_ms, c.channel, c.controller, c.value));
            }
            ctrl_events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        }
        let mut ci = 0usize;

        let tick = Duration::from_millis(self.config.tick_ms.max(1));
        let start = std::time::Instant::now();
        let mut ei = 0usize;
        // In dry-run the schedule is printed, not played, so there is nothing to
        // wait for. A virtual clock that jumps straight to the next event turns
        // a four-minute preview into an instant dump you can diff.
        let mut virtual_now: u64 = 0;

        while ei < events.len() || !releases.is_empty() || ci < ctrl_events.len() {
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
            //
            // A note whose OWN start and end both become due inside one
            // scheduler pass (which happens whenever the loop runs slower than
            // the spacing between a note's start and end — routine in dense
            // passages) must not have its keyup consumed before its keydown is
            // sent. If it were, the keyup would be discarded, the keydown would
            // still go out, and the key would stick down forever — blocking every
            // later note on that key. So a keyup whose key is not yet held is
            // deferred to the next pass, where the keydown has already gone out
            // and the gap gives the game a real, registerable press.
            let due: Vec<(u64, Vec<Target>)> = releases
                .range(..=elapsed)
                .map(|(t, v)| (*t, v.clone()))
                .collect();
            let mut deferred: Vec<(u64, Target)> = Vec::new();
            for (t, targets) in &due {
                for target in targets {
                    if self.held.contains_key(&target.id()) {
                        self.release(target, dry_run, *t as f64, elapsed)?;
                    } else {
                        // A keyup that arrived before its own keydown. It gets
                        // retried next pass; tracing it matters because the
                        // retry can eat into the key-up window the game sees.
                        self.trace_key(*t as f64, elapsed, "DEFERRED", target, Some(0));
                        deferred.push((*t, target.clone()));
                    }
                }
            }
            for (t, _) in &due {
                releases.remove(t);
            }
            for (t, target) in deferred {
                releases.entry(t).or_default().push(target);
            }

            // Press notes whose start time has arrived.
            while ei < events.len() && (events[ei].0.round() as u64) <= elapsed {
                self.press(&events[ei].1, dry_run, events[ei].0, elapsed)?;
                ei += 1;
            }

            // Emit ControlChange events (sustain, volume, pan, …) on the MIDI
            // path. They run on the same wall clock so they land at the exact
            // tick the original file put them on.
            while ci < ctrl_events.len() && (ctrl_events[ci].0.round() as u64) <= elapsed {
                let (_at, ch, cc, val) = ctrl_events[ci];
                match self.mode {
                    OutputMode::Midi => {
                        if let Some(out) = self.midi.as_mut() {
                            out.control_change(ch, cc, val)?;
                        } else if dry_run {
                            self.log(&format!("[CC] ch{} cc{}={}", ch + 1, cc, val));
                        }
                    }
                    OutputMode::Keys => {}
                }
                ci += 1;
            }

            if dry_run {
                // Everything due at this instant has been emitted, so the next
                // event is strictly in the future — the clock always advances and
                // the loop terminates.
                let next_press = events.get(ei).map(|e| e.0.round() as u64);
                let next_release = releases.keys().next().copied();
                let next_ctrl = if ci < ctrl_events.len() {
                    Some(ctrl_events[ci].0.round() as u64)
                } else {
                    None
                };
                virtual_now = match (next_press, next_release, next_ctrl) {
                    (Some(a), Some(b), Some(c)) => a.min(b).min(c),
                    (Some(a), Some(b), None) => a.min(b),
                    (Some(a), None, Some(c)) => a.min(c),
                    (None, Some(b), Some(c)) => b.min(c),
                    (Some(a), None, None) => a,
                    (None, Some(b), None) => b,
                    (None, None, Some(c)) => c,
                    (None, None, None) => break,
                };
            } else {
                // Sleep until the next scheduled event instead of a fixed tick, so
                // a burst of notes is spread across real time rather than dumped
                // in a single instant (a burst the game's input sampler skips,
                // losing notes). Capped at 60 ms so we stay responsive to stop.
                let next_press = events.get(ei).map(|e| e.0.round() as u64);
                let next_release = releases.keys().next().copied();
                let next_ctrl = if ci < ctrl_events.len() {
                    Some(ctrl_events[ci].0.round() as u64)
                } else {
                    None
                };
                let next = [next_press, next_release, next_ctrl]
                    .iter()
                    .filter_map(|x| *x)
                    .min();
                match next {
                    Some(t) if t > elapsed => {
                        let wait = (t - elapsed).clamp(1, 60);
                        sleep(Duration::from_millis(wait));
                    }
                    _ => sleep(tick),
                }
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

    fn press(&mut self, target: &Target, dry_run: bool, at_ms: f64, elapsed: u64) -> Result<()> {
        let id = target.id();
        let count = {
            let slot = self
                .held
                .entry(id.clone())
                .or_insert_with(|| (target.clone(), 0));
            slot.1 += 1;
            slot.1
        };
        if count > 1 {
            self.dropped += 1;
            self.trace_key(at_ms, elapsed, "SUPPRESSED", target, Some(count));
        }
        if count == 1 {
            self.presses += 1;
            self.trace_key(at_ms, elapsed, "PRESS", target, Some(1));
            if dry_run {
                self.log(&format!("[PRESS] {}", target.describe_on()));
            } else {
                self.dispatch(target, Direction::Press)?;
            }
        }
        Ok(())
    }

    fn release(&mut self, target: &Target, dry_run: bool, at_ms: f64, elapsed: u64) -> Result<()> {
        let id = target.id();
        let count = match self.held.get_mut(&id) {
            Some((_, count)) => {
                *count = count.saturating_sub(1);
                *count
            }
            None => return Ok(()),
        };
        if count == 0 {
            self.held.remove(&id);
            self.releases += 1;
            self.trace_key(at_ms, elapsed, "RELEASE", target, Some(0));
            if dry_run {
                self.log(&format!("[RELEASE] {}", target.describe()));
            } else {
                self.dispatch(target, Direction::Release)?;
            }
        } else {
            // Another overlapping note still owns the key, so NO keyup was
            // posted. Exactly the case a skipped-note trace must expose.
            self.trace_key(at_ms, elapsed, "HELD_RELEASE", target, Some(count));
        }
        Ok(())
    }

    fn release_all(&mut self, dry_run: bool) -> Result<()> {
        let targets: Vec<Target> = self.held.drain().map(|(_, (target, _))| target).collect();
        for t in targets {
            crate::trace::note(&format!("tail release: {}", t.describe()));
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
        play_notes_cfg(notes, humanize, PlayerConfig::default())
    }

    fn play_notes_cfg(notes: Vec<Note>, humanize: u8, pc: PlayerConfig) -> Vec<String> {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let humanizer = Humanizer::new(humanize, 1.0, cfg.humanize.clone());
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut player = Player::new(
            mapper,
            humanizer,
            pc,
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
    fn a_sharp_while_its_white_key_is_held_trims_the_white_note() {
        // C2 is key "1", C#2 is Shift+"!" — the SAME physical key. A long C2
        // used to keep "1" down while the sharp was scheduled, so the OS sent
        // no second key-down and C#2 was silently lost. Grouping by physical
        // key trims C2's release to the sharp's onset.
        let notes = vec![note(36, 0.0, 1000.0), note(37, 500.0, 200.0)];
        let log = play_notes(notes, 0);
        assert_eq!(presses(&log), 2, "both pitches must get a key-down");
        let second_press = log
            .iter()
            .position(|l| l == "[PRESS] !")
            .expect("the sharp must be pressed");
        assert!(
            log[..second_press].iter().any(|l| l == "[RELEASE] 1"),
            "the white key must be released BEFORE the sharp is pressed"
        );
    }

    #[test]
    fn ctrl_layer_shares_the_same_physical_key() {
        // A0 is Ctrl+"1", C2 is "1". Same physical key, different modifiers.
        let notes = vec![note(21, 0.0, 1000.0), note(36, 400.0, 200.0)];
        let log = play_notes(notes, 0);
        assert_eq!(presses(&log), 2);
        let second = log
            .iter()
            .position(|l| l == "[PRESS] 1")
            .expect("plain C2 must be pressed");
        assert!(log[..second].iter().any(|l| l == "[RELEASE] ctrl+1"));
    }

    #[test]
    fn two_pitches_on_one_key_both_get_a_press() {
        // C2 ("1") + C#2 ("!") on one tick: physically one key, but two
        // key-downs separated by the configured gap means BOTH notes sound.
        let notes = vec![note(36, 0.0, 500.0), note(37, 0.0, 500.0)];
        let log = play_notes(notes, 0);
        assert_eq!(presses(&log), 2, "no pitch may be dropped");
        let first = log.iter().position(|l| l == "[PRESS] 1").unwrap();
        let second = log.iter().position(|l| l == "[PRESS] !").unwrap();
        assert!(first < second);
        assert!(
            log[first..second].iter().any(|l| l == "[RELEASE] 1"),
            "the first key must be released before the second key-down"
        );
    }

    #[test]
    fn gap_zero_still_collapses_to_one_press() {
        // Opting out restores the old behaviour for anyone who prefers it.
        let pc = PlayerConfig {
            same_key_gap_ms: 0.0,
            ..Default::default()
        };
        let notes = vec![note(36, 0.0, 500.0), note(37, 0.0, 500.0)];
        assert_eq!(presses(&play_notes_cfg(notes, 0, pc)), 1);
    }

    #[test]
    fn multiplexing_keeps_the_first_onset_and_delays_only_the_second() {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let pc = PlayerConfig::default();
        let notes = vec![note(36, 1000.0, 500.0), note(37, 1000.0, 500.0)];
        let s = build_schedule(&notes, &pc, OutputMode::Keys, &mapper);
        assert_eq!(s.events.len(), 2);
        assert_eq!(s.multiplexed, 1);
        assert_eq!(s.physical_drops, 0);
        assert_eq!(s.events[0].0, 1000.0, "first onset is untouched");
        // min_hold_ms (30) + retrigger_gap_ms (20): the first note gets a full
        // hold AND a real key-up window before the second goes down.
        assert_eq!(s.events[1].0, 1050.0, "second onset shifted by the gap");
        assert_eq!(
            s.events[0].2, 1030.0,
            "first release leaves 20 ms of key-up"
        );
        assert_eq!(s.events[1].2, 1550.0, "second keeps its duration");
    }

    #[test]
    fn release_leaves_a_real_key_up_window_before_the_next_press() {
        // The original bug: a note that outlasts the next one on the same
        // physical key had its release set to EXACTLY that next onset, so the
        // key-up and key-down were posted in one scheduler pass and Roblox saw
        // an unbroken hold. Default retrigger_gap_ms is 20 ms.
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let notes = vec![note(36, 0.0, 1000.0), note(37, 500.0, 200.0)];
        let s = build_schedule(&notes, &PlayerConfig::default(), OutputMode::Keys, &mapper);

        assert_eq!(s.events[0].0, 0.0, "onset must never move");
        assert_eq!(s.events[1].0, 500.0, "onset must never move");
        assert_eq!(s.events[0].2, 480.0, "release pulled back by the gap");
        assert_eq!(s.tight_retriggers, 0, "this pair now has a full gap");
    }

    #[test]
    fn zero_retrigger_gap_restores_back_to_back_releases() {
        // Proves the new behaviour is real: with the knob off, the release
        // lands exactly on the next onset again. This is also the A/B switch
        // for testing against the game.
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let pc = PlayerConfig {
            retrigger_gap_ms: 0.0,
            ..Default::default()
        };
        let notes = vec![note(36, 0.0, 1000.0), note(37, 500.0, 200.0)];
        let s = build_schedule(&notes, &pc, OutputMode::Keys, &mapper);
        assert_eq!(s.events[0].2, 500.0);
    }

    #[test]
    fn a_short_natural_gap_is_widened_to_the_retrigger_gap() {
        // Two notes 5 ms apart on one physical key: no *overlap*, but 5 ms is
        // still far below one 60 Hz frame. The release must move back anyway.
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let notes = vec![note(36, 0.0, 495.0), note(36, 500.0, 200.0)];
        let s = build_schedule(&notes, &PlayerConfig::default(), OutputMode::Keys, &mapper);
        assert_eq!(s.events[0].0, 0.0);
        assert_eq!(s.events[1].0, 500.0);
        assert_eq!(s.events[0].2, 480.0);
        assert_eq!(s.tight_retriggers, 0);
    }

    #[test]
    fn retrigger_gap_never_breaks_the_min_hold_floor() {
        // Onsets 30 ms apart cannot fit both a 30 ms hold and a 20 ms gap.
        // min_hold wins, and the schedule REPORTS the unresolvable pair instead
        // of silently pretending the note will sound.
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let notes = vec![note(36, 0.0, 1000.0), note(36, 30.0, 1000.0)];
        let s = build_schedule(&notes, &PlayerConfig::default(), OutputMode::Keys, &mapper);
        assert_eq!(s.events[1].0, 30.0, "onset is still untouched");
        assert_eq!(s.events[0].2, 30.0, "release clamped to start + min_hold");
        assert_eq!(s.tight_retriggers, 1);
    }

    #[test]
    fn retrigger_gap_does_not_apply_to_midi_output() {
        // MIDI carries note-offs and has no physical key to release. Two
        // neighbouring pitches overlap freely there, while the keyboard path
        // must pull the first release back because both share one key.
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let notes = vec![note(36, 0.0, 1000.0), note(37, 500.0, 200.0)];
        let pc = PlayerConfig::default();

        let midi = build_schedule(&notes, &pc, OutputMode::Midi, &mapper);
        assert_eq!(midi.events[0].2, 1000.0, "MIDI keeps the written duration");
        assert_eq!(midi.events[1].2, 700.0);
        assert_eq!(midi.tight_retriggers, 0);

        let keys = build_schedule(&notes, &pc, OutputMode::Keys, &mapper);
        assert_eq!(keys.events[0].2, 480.0, "keyboard leaves a key-up window");
    }

    #[test]
    fn dry_run_reports_tight_retriggers_instead_of_failing_quietly() {
        let pc = PlayerConfig {
            max_hold_ms: 30.0,
            ..Default::default()
        };
        let notes = vec![note(36, 0.0, 30.0), note(36, 30.0, 30.0)];
        let log = play_notes_cfg(notes, 0, pc);
        assert!(
            log.iter().any(|l| l.contains("release/press pair(s)")),
            "the unresolvable pair must be reported: {log:?}"
        );
    }

    /// Build a dry-run player so the tests can ask what the scheduler would
    /// actually do without touching any input device.
    fn targeted_player(pc: PlayerConfig, mode: OutputMode) -> Player {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let humanizer = Humanizer::new(0, 1.0, cfg.humanize.clone());
        Player::new(mapper, humanizer, pc, true, mode, None, None, None).expect("dry-run player")
    }

    #[test]
    fn targeted_mode_widens_key_up_windows_without_moving_onsets() {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        // A long C2 holds physical key "1" past the C#2 ("!") onset at 500 ms.
        let notes = vec![note(36, 0.0, 1000.0), note(37, 500.0, 200.0)];

        let plain = build_schedule(&notes, &PlayerConfig::default(), OutputMode::Keys, &mapper);
        let mut p = targeted_player(PlayerConfig::default(), OutputMode::Keys);
        p.set_targeted(true);
        let effective = p.effective_config();
        let direct = build_schedule(&notes, &effective, OutputMode::Keys, &mapper);

        assert_eq!(plain.events[1].0, 500.0);
        assert_eq!(direct.events[0].0, 0.0, "onsets never move");
        assert_eq!(direct.events[1].0, 500.0, "onsets never move");
        assert_eq!(plain.events[0].2, 480.0, "HID only needs a 20 ms key-up");
        assert_eq!(
            direct.events[0].2, 450.0,
            "queued input needs a 50 ms key-up"
        );
        assert_eq!(direct.tight_retriggers, 0);
        assert!(effective.same_key_gap_ms >= 60.0);
    }

    #[test]
    fn targeted_margin_zero_restores_the_stock_schedule() {
        // A/B switch: with the extra margin off, Direct mode must produce the
        // exact HID schedule, so any in-game difference is attributable to the
        // margin and not to something else in the targeted path.
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let notes = vec![note(36, 0.0, 1000.0), note(37, 500.0, 200.0)];
        let pc = PlayerConfig {
            targeted_extra_gap_ms: 0.0,
            ..Default::default()
        };
        let mut p = targeted_player(pc.clone(), OutputMode::Keys);
        p.set_targeted(true);
        assert_eq!(p.effective_config().retrigger_gap_ms, pc.retrigger_gap_ms);
        let direct = build_schedule(&notes, &p.effective_config(), OutputMode::Keys, &mapper);
        let plain = build_schedule(&notes, &pc, OutputMode::Keys, &mapper);
        assert_eq!(direct.events[0].2, plain.events[0].2);
        assert_eq!(direct.events[1].0, plain.events[1].0);
    }

    #[test]
    fn targeted_mode_never_changes_midi_output() {
        let mut p = targeted_player(PlayerConfig::default(), OutputMode::Midi);
        p.set_targeted(true);
        let effective = p.effective_config();
        assert_eq!(effective.retrigger_gap_ms, 20.0);
        assert_eq!(effective.same_key_gap_ms, 30.0);
    }

    #[test]
    fn three_pitches_on_one_key_all_get_a_press() {
        // Ctrl+1 (A0), 1 (C2), ! (C#2) — all the same physical key.
        let notes = vec![
            note(21, 0.0, 800.0),
            note(36, 0.0, 800.0),
            note(37, 0.0, 800.0),
        ];
        assert_eq!(presses(&play_notes(notes, 0)), 3);
    }

    #[test]
    fn hold_cap_shortens_ends_without_moving_any_start() {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let pc = PlayerConfig {
            max_hold_ms: 200.0,
            ..Default::default()
        };
        let notes = vec![note(60, 1000.0, 2000.0), note(62, 1200.0, 900.0)];
        let s = build_schedule(&notes, &pc, OutputMode::Keys, &mapper);
        assert_eq!(s.events.len(), 2);
        assert_eq!(s.events[0].0, 1000.0, "onset must be untouched");
        assert_eq!(s.events[1].0, 1200.0, "onset must be untouched");
        assert_eq!(s.events[0].2, 1200.0, "end pulled in to the 200 ms cap");
        assert_eq!(s.events[1].2, 1400.0, "end pulled in to the 200 ms cap");
    }

    #[test]
    fn hold_cap_never_applies_to_midi_output() {
        let cfg = Config::load(None).expect("builtin config");
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let pc = PlayerConfig {
            max_hold_ms: 200.0,
            ..Default::default()
        };
        let notes = vec![note(60, 1000.0, 2000.0)];
        let s = build_schedule(&notes, &pc, OutputMode::Midi, &mapper);
        assert_eq!(s.events[0].2, 3000.0, "MIDI keeps the written duration");
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
