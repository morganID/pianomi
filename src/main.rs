//! `pianomi` — play MIDI files into Roblox piano games with a
//! humanization layer, from the command line.

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::PathBuf;

use pianomi::config::Config;
use pianomi::humanizer::Humanizer;
use pianomi::key_mapper::KeyMapper;
use pianomi::midi_out;
use pianomi::midi_parser::parse_midi;
use pianomi::note_name;
use pianomi::player::{OutputMode, Player};

#[derive(Parser)]
#[command(name = "pianomi")]
#[command(version, about = "Play MIDI files into Roblox piano games with humanization", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Play a MIDI file into the focused Roblox piano window.
    Play {
        /// Path to the .mid / .midi file.
        #[arg(short, long)]
        file: PathBuf,

        /// Humanize level 0-100 (0 = exactly as written, 100 = max variation).
        /// Defaults to 0 so the file plays faithfully unless you opt in.
        #[arg(long, default_value_t = 0)]
        humanize: u8,

        /// Playback speed multiplier (e.g. 2.0 = twice as fast).
        #[arg(short = 's', long, default_value_t = 1.0)]
        speed: f32,

        /// Path to a TOML config file (defaults to the built-in config).
        #[arg(long)]
        config: Option<PathBuf>,

        /// Only play this MIDI channel (1-16).
        #[arg(short = 'c', long)]
        channel: Option<u8>,

        /// Transpose all notes by N semitones (overrides config).
        #[arg(short = 't', long, default_value_t = 0)]
        transpose: i8,

        /// Number of times to repeat the song.
        #[arg(short = 'l', long, default_value_t = 1)]
        loops: u32,

        /// How sharp (black) keys are sent: "key" (real Shift, needed by
        /// Roblox's IsKeyDown) or "char" (Shift flag + typed character only).
        #[arg(long, value_name = "MODE")]
        shift_mode: Option<String>,

        /// Keyboard only: shorten notes longer than N ms. Note START times are
        /// never changed, only the release. 0 (default) keeps written lengths.
        /// Useful for dense songs where long holds block later notes.
        #[arg(long, value_name = "MS")]
        max_hold_ms: Option<f64>,

        /// Keyboard only: gap in ms between two different pitches that need the
        /// same physical key on one tick (e.g. C2 "1" + C#2 "!"). 0 disables it
        /// and drops the second note. Default 30 (raised to min_hold_ms).
        #[arg(long, value_name = "MS")]
        same_key_gap_ms: Option<f64>,

        /// Keyboard only: real-time gap (ms) between a key-up and the next
        /// key-down on the SAME physical key. Roblox samples IsKeyDown once per
        /// frame; releasing exactly at the next note's start makes the game see
        /// one unbroken hold and lose the second note. Default 20 (just over one
        /// 60 Hz frame). Onsets never move — only the earlier release. 0 = old
        /// back-to-back behaviour.
        #[arg(long, value_name = "MS")]
        retrigger_gap_ms: Option<f64>,

        /// Keyboard only, Direct to Roblox: extra key-up margin (ms) added to
        /// --retrigger-gap-ms because CGEventPostToPid events are drained in
        /// frame-sized batches. Default 30 (so the key-up becomes 50 ms).
        /// Onsets never move. 0 disables the widening for an A/B comparison.
        #[arg(long, value_name = "MS")]
        targeted_extra_gap_ms: Option<f64>,

        /// macOS: send key events only to the running Roblox.app process
        /// (CGEventPostToPid) instead of the whole system. No focus stealing,
        /// no keystrokes leaking into other apps. Still keyboard events.
        #[arg(long)]
        target_roblox: bool,

        /// Debug: post key events to an arbitrary PID instead of the system-wide
        /// HID tap. Point this at a local event receiver to measure delivery
        /// timing without sending anything into Roblox. macOS only.
        #[arg(long, value_name = "PID", conflicts_with = "target_roblox")]
        target_pid: Option<i32>,

        /// Debug: write a per-event playback trace (scheduled vs actual time,
        /// key-up gaps, delivery target, warnings) that survives a hang.
        /// Also enabled by PIANOMI_TRACE=1.
        #[arg(long)]
        trace: bool,

        /// Debug: trace file destination. Defaults to
        /// ~/Library/Logs/Pianomi/trace.log (or PIANOMI_TRACE_LOG).
        #[arg(long, value_name = "PATH")]
        trace_log: Option<PathBuf>,

        /// Don't press any keys — just print the scheduled events.
        #[arg(long)]
        dry_run: bool,

        /// Send MIDI instead of keystrokes.
        ///
        /// Without a value, publish a virtual MIDI port other apps can select as
        /// an input. With a value, connect to that destination — or publish a
        /// virtual port with that name when nothing matches.
        #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
        midi_out: Option<String>,
    },

    /// Show the resolved key → note mapping from the config.
    ListMappings {
        /// Path to a TOML config file.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    /// Print information about a MIDI file (no playback).
    Info {
        /// Path to the .mid / .midi file.
        #[arg(short, long)]
        file: PathBuf,

        /// Also dump the full tempo / time-signature change tables.
        #[arg(short, long)]
        verbose: bool,

        /// Path to a TOML config file, used for the key-coverage report.
        #[arg(long)]
        config: Option<PathBuf>,
    },

    /// Print the built-in default config (useful as a starting point).
    PrintConfig,

    /// List the MIDI destinations this machine exposes (for `play --midi-out`).
    MidiPorts,
}

fn main() -> Result<()> {
    pianomi::trace::init_from_env();
    let cli = Cli::parse();
    match cli.command {
        Commands::Play {
            file,
            humanize,
            speed,
            config,
            channel,
            transpose,
            loops,
            shift_mode,
            max_hold_ms,
            same_key_gap_ms,
            retrigger_gap_ms,
            targeted_extra_gap_ms,
            target_roblox,
            target_pid,
            trace,
            trace_log,
            dry_run,
            midi_out,
        } => cmd_play(
            file,
            humanize,
            speed,
            config,
            channel,
            transpose,
            loops,
            shift_mode,
            max_hold_ms,
            same_key_gap_ms,
            retrigger_gap_ms,
            targeted_extra_gap_ms,
            target_roblox,
            target_pid,
            trace,
            trace_log,
            dry_run,
            midi_out,
        ),
        Commands::ListMappings { config } => cmd_list(config),
        Commands::Info {
            file,
            verbose,
            config,
        } => cmd_info(file, verbose, config),
        Commands::PrintConfig => {
            print!("{}", Config::builtin());
            Ok(())
        }
        Commands::MidiPorts => cmd_midi_ports(),
    }
}

/// PID of the running RobloxPlayer, used by `--target-roblox`.
fn roblox_pid() -> Result<i32> {
    let out = std::process::Command::new("pgrep")
        .arg("-x")
        .arg("RobloxPlayer")
        .output()
        .map_err(|e| anyhow::anyhow!("running pgrep: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<i32>().ok())
        .next()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "RobloxPlayer is not running. Launch /Applications/Roblox.app first, \
                 or drop --target-roblox to use system-wide key events."
            )
        })
}

fn cmd_midi_ports() -> Result<()> {
    let destinations = midi_out::list_destinations()?;
    println!("MIDI destinations: {}", destinations.len());
    if destinations.is_empty() {
        println!("  (none)");
        println!();
        println!("Nothing to connect to, but `play --midi-out` with no value still works:");
        println!("it publishes a virtual port that other apps can select as an input.");
    } else {
        for (i, name) in destinations.iter().enumerate() {
            println!("  {}. {}", i + 1, name);
        }
        println!();
        println!("Connect with: pianomi play -f song.mid --midi-out \"<name>\"");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_play(
    file: PathBuf,
    humanize: u8,
    speed: f32,
    config: Option<PathBuf>,
    channel: Option<u8>,
    transpose: i8,
    loops: u32,
    shift_mode: Option<String>,
    max_hold_ms: Option<f64>,
    same_key_gap_ms: Option<f64>,
    retrigger_gap_ms: Option<f64>,
    targeted_extra_gap_ms: Option<f64>,
    target_roblox: bool,
    target_pid: Option<i32>,
    trace: bool,
    trace_log: Option<PathBuf>,
    dry_run: bool,
    midi_out: Option<String>,
) -> Result<()> {
    // Debug plumbing is resolved before anything else so even a config/parse
    // failure leaves a readable trace behind.
    if trace {
        pianomi::trace::set_enabled(true);
    }
    if let Some(path) = &trace_log {
        std::env::set_var("PIANOMI_TRACE_LOG", path);
        pianomi::trace::set_enabled(true);
    }
    if pianomi::trace::enabled() {
        println!("Trace       : {}", pianomi::trace::log_path().display());
    }

    let mut cfg = Config::load(config.as_deref())?;
    if transpose != 0 {
        cfg.keyboard.transpose = transpose;
    }
    if let Some(mode) = shift_mode {
        // Validate through the parser so a typo does not silently fall back.
        let parsed = pianomi::keycode::parse_shift_mode(&mode);
        let canonical = match parsed {
            pianomi::keycode::ShiftMode::Key => "key",
            pianomi::keycode::ShiftMode::Char => "char",
        };
        if !mode.trim().eq_ignore_ascii_case(canonical) {
            anyhow::bail!("unknown --shift-mode {mode:?} (expected \"key\" or \"char\")");
        }
        cfg.player.shift_mode = canonical.to_string();
    }
    if let Some(ms) = max_hold_ms {
        if !ms.is_finite() || ms < 0.0 {
            anyhow::bail!("--max-hold-ms must be a non-negative number (0 disables the cap)");
        }
        cfg.player.max_hold_ms = ms;
    }
    if cfg.player.max_hold_ms > 0.0 && cfg.player.max_hold_ms < cfg.player.min_hold_ms {
        anyhow::bail!(
            "--max-hold-ms ({}) cannot be below min_hold_ms ({}) — notes would stop registering",
            cfg.player.max_hold_ms,
            cfg.player.min_hold_ms
        );
    }
    if let Some(gap) = same_key_gap_ms {
        if !gap.is_finite() || gap < 0.0 {
            anyhow::bail!("--same-key-gap-ms must be non-negative (0 disables multiplexing)");
        }
        cfg.player.same_key_gap_ms = gap;
    }
    if let Some(gap) = retrigger_gap_ms {
        if !gap.is_finite() || gap < 0.0 {
            anyhow::bail!("--retrigger-gap-ms must be non-negative (0 disables the key-up gap)");
        }
        cfg.player.retrigger_gap_ms = gap;
    }
    if let Some(extra) = targeted_extra_gap_ms {
        if !extra.is_finite() || extra < 0.0 {
            anyhow::bail!(
                "--targeted-extra-gap-ms must be non-negative (0 disables the Direct-mode widening)"
            );
        }
        cfg.player.targeted_extra_gap_ms = extra;
    }

    let mode = if midi_out.is_some() {
        OutputMode::Midi
    } else {
        OutputMode::Keys
    };

    if target_roblox {
        if mode == OutputMode::Midi {
            println!("note: --target-roblox has no effect in --midi-out mode");
        } else if dry_run {
            // Nothing is posted, so this is a pure schedule preview — and a
            // Direct-mode schedule is exactly what needs previewing, because
            // its key-up margins are wider than the HID defaults.
            println!(
                "note: --target-roblox in --dry-run previews the Direct-mode schedule \
                 (no events are posted)"
            );
        } else {
            #[cfg(target_os = "macos")]
            {
                let pid = roblox_pid()?;
                pianomi::keycode::set_event_target_roblox(true);
                println!(
                    "Event target  : RobloxPlayer pid {} (targeted, PID auto-recovery on)",
                    pid
                );
            }
            #[cfg(not(target_os = "macos"))]
            {
                anyhow::bail!("--target-roblox is only available on macOS");
            }
        }
    }

    // Debug transport: send to any process. Used to aim playback at a local
    // event receiver and measure delivery without involving Roblox at all.
    if let Some(pid) = target_pid {
        if mode == OutputMode::Midi {
            println!("note: --target-pid has no effect in --midi-out mode");
        } else if dry_run {
            println!(
                "note: --target-pid in --dry-run previews the Direct-mode schedule \
                 (no events are posted)"
            );
        } else {
            #[cfg(target_os = "macos")]
            {
                pianomi::keycode::set_event_target_pid(pid);
                println!("Event target  : pid {pid} (debug receiver, no PID tracking)");
            }
            #[cfg(not(target_os = "macos"))]
            {
                anyhow::bail!("--target-pid is only available on macOS");
            }
        }
    }

    // The scheduler needs to know which transport the events travel over:
    // a queued (PID-targeted) event needs a wider key-up window than one
    // applied synchronously to the HID tap.
    // Dry-run still counts as "targeted" for scheduling purposes: it posts
    // nothing, so previewing the wider Direct-mode margins is free and is the
    // only way to A/B the two schedules without touching the game.
    #[cfg(target_os = "macos")]
    let targeted = (target_roblox || target_pid.is_some()) && mode == OutputMode::Keys;
    #[cfg(not(target_os = "macos"))]
    let targeted = false;
    let timing = pianomi::player::schedule_config_for(&cfg.player, mode, targeted);

    // Open the port before anything else so a bad name fails fast, but never in
    // dry-run: previewing must not publish or touch a device.
    let midi = match (&midi_out, dry_run) {
        (Some(spec), false) => Some(midi_out::open(Some(spec))?),
        _ => None,
    };

    if mode == OutputMode::Midi {
        match &midi {
            Some(out) => println!("MIDI output  : {}", out.describe()),
            None => println!("MIDI output  : (dry run — no port opened)"),
        }
    } else {
        println!("Game profile : {}", cfg.game.name);
        println!(
            "Target window: {} (focus it before playback)",
            cfg.game.window_title
        );
        println!("Config keys  : {} mapped", cfg.keyboard.keys.len());
        let sm = pianomi::keycode::parse_shift_mode(&cfg.player.shift_mode);
        let what = match sm {
            pianomi::keycode::ShiftMode::Key => "real Shift per note (IsKeyDown)",
            pianomi::keycode::ShiftMode::Char => "Shift flag + typed char only",
        };
        println!("Sharp keys   : {} — {}", cfg.player.shift_mode, what);
        if cfg.player.max_hold_ms > 0.0 {
            println!(
                "Hold cap     : {:.0} ms (starts unchanged, releases pulled in)",
                cfg.player.max_hold_ms
            );
        }
        if mode == OutputMode::Keys {
            // Print the *effective* values, not the raw config: Direct mode
            // widens these margins inside the scheduler.
            println!(
                "Retrigger    : {:.0} ms of real key-up before the next press on one \
                 physical key (onsets unchanged)",
                timing.retrigger_gap_ms
            );
            if timing.same_key_gap_ms > 0.0 {
                let g = timing
                    .same_key_gap_ms
                    .max(timing.min_hold_ms + timing.retrigger_gap_ms);
                println!(
                    "Shared keys  : 2 pitches / key played {:.0} ms apart (no note dropped)",
                    g
                );
            } else {
                println!("Shared keys  : multiplexing off (second pitch dropped)");
            }
        }
        if targeted {
            println!(
                "Direct mode  : +{:.0} ms key-up margin for Roblox's queued events",
                cfg.player.targeted_extra_gap_ms
            );
        }
    }

    let mut song = parse_midi(&file)?;
    if let Some(ch) = channel {
        song.filter_channel(ch);
        println!("Channel      : {}", ch);
    }
    println!(
        "Song         : {} notes, {:.1}s, channels {}",
        song.notes.len(),
        song.length_ms / 1000.0,
        ch_list(&song.channels_used)
    );

    if song.notes.is_empty() {
        anyhow::bail!("no playable notes found (empty file or filtered channel)");
    }

    let mapper = KeyMapper::from_config(&cfg.keyboard);
    let humanizer = Humanizer::new(humanize, speed as f64, cfg.humanize.clone());
    let mut player = Player::new(
        mapper,
        humanizer,
        cfg.player.clone(),
        dry_run,
        mode,
        midi,
        None,
        None,
    )?;
    // Must match the transport chosen above: queued events need wider key-up
    // margins than synchronous HID posts.
    player.set_targeted(targeted);
    player.play(&song, loops, dry_run)?;
    println!("Done.");
    Ok(())
}

/// Report how much of the song the configured keymap can actually play.
///
/// A file can parse perfectly and still be unplayable: notes below A0 or above
/// C8 have no key on the in-game piano. This turns that into a number instead of
/// notes silently vanishing during playback.
fn print_coverage(song: &pianomi::midi_parser::Song, cfg: &Config, verbose: bool) {
    let mapper = KeyMapper::from_config(&cfg.keyboard);
    let (lo, hi) = mapper.mapped_range();

    let (mut plain, mut shifted, mut ctrl) = (0usize, 0usize, 0usize);
    let mut missing: BTreeMap<u8, usize> = BTreeMap::new();
    for n in &song.notes {
        match mapper.key_for_note(n.note) {
            Some(b) => {
                if b.ctrl {
                    ctrl += 1;
                } else if pianomi::keycode::label_is_shifted(&b.label) {
                    shifted += 1;
                } else {
                    plain += 1;
                }
            }
            None => *missing.entry(n.note).or_insert(0) += 1,
        }
    }

    let total = song.notes.len();
    let unmapped: usize = missing.values().sum();
    let playable = total.saturating_sub(unmapped);

    println!();
    println!(
        "Mapped range    : {} ({}) .. {} ({})",
        lo,
        note_name(lo),
        hi,
        note_name(hi)
    );
    println!(
        "Key coverage    : {}/{} notes ({:.1}%)",
        playable,
        total,
        if total > 0 {
            playable as f64 / total as f64 * 100.0
        } else {
            100.0
        }
    );
    println!(
        "Modifiers       : {} plain, {} shift, {} ctrl",
        plain, shifted, ctrl
    );

    if !missing.is_empty() {
        println!(
            "Unmapped        : {} note{} on {} pitch{} outside the mapped range",
            unmapped,
            if unmapped == 1 { "" } else { "s" },
            missing.len(),
            if missing.len() == 1 { "" } else { "es" }
        );
        if verbose {
            for (note, count) in &missing {
                println!("    {} ({}) x{}", note, note_name(*note), count);
            }
        } else {
            let lo_n = *missing.keys().next().unwrap();
            let hi_n = *missing.keys().last().unwrap();
            println!(
                "                  lowest {} ({}), highest {} ({})",
                lo_n,
                note_name(lo_n),
                hi_n,
                note_name(hi_n)
            );
        }
    }
}

fn cmd_list(config: Option<PathBuf>) -> Result<()> {
    let cfg = Config::load(config.as_deref())?;
    let mapper = KeyMapper::from_config(&cfg.keyboard);
    let (min, max) = mapper.mapped_range();
    println!("Game profile : {}", cfg.game.name);
    println!(
        "Mapped range : {} ({}) .. {} ({})",
        min,
        note_name(min),
        max,
        note_name(max)
    );
    println!();
    println!("{:<6} {:<8} KEY", "NOTE", "NAME");
    println!("{}", "-".repeat(22));
    let mut entries: Vec<_> = cfg.keyboard.keys.iter().collect();
    entries.sort_by_key(|k| k.note);
    for km in entries {
        let label = if km.uses_ctrl() {
            format!("ctrl+{}", km.key)
        } else {
            km.key.clone()
        };
        println!("{:<6} {:<8} {}", km.note, note_name(km.note), label);
    }
    Ok(())
}

/// Render MIDI channel indices as the 1-based numbers the CLI accepts.
fn ch_list(chs: &[u8]) -> String {
    if chs.is_empty() {
        return "-".to_string();
    }
    let parts: Vec<String> = chs.iter().map(|c| (c + 1).to_string()).collect();
    format!("[{}]", parts.join(", "))
}

fn cmd_info(file: PathBuf, verbose: bool, config: Option<PathBuf>) -> Result<()> {
    let song = parse_midi(&file)?;

    let format_label = match song.format {
        0 => "0 (single track)".to_string(),
        1 => "1 (parallel tracks)".to_string(),
        2 => "2 (independent sequences)".to_string(),
        n => format!("{n}"),
    };

    println!("File            : {}", file.display());
    println!("Format          : {}", format_label);
    println!("Timing          : {}", song.timing);

    println!("Tracks          : {}", song.tracks.len());
    for t in &song.tracks {
        println!(
            "  #{:<2} {:<34} note-ons {:<6} notes {:<6} ch {}",
            t.index + 1,
            t.display_name(),
            t.note_events,
            t.notes,
            ch_list(&t.channels),
        );
        if let Some(inst) = &t.instrument {
            println!("      instrument: {inst}");
        }
    }

    println!(
        "Notes           : {} ({} note-on events)",
        song.notes.len(),
        song.total_note_events()
    );
    println!("Channels used   : {}", ch_list(&song.channels_used));
    if !song.control_events.is_empty() {
        let sustain = song
            .control_events
            .iter()
            .filter(|c| c.controller == 64)
            .count();
        println!(
            "Control changes : {} (sustain cc64 x{}, channel-volume cc7 x{}, pan cc10 x{})",
            song.control_events.len(),
            sustain,
            song.control_events
                .iter()
                .filter(|c| c.controller == 7)
                .count(),
            song.control_events
                .iter()
                .filter(|c| c.controller == 10)
                .count(),
        );
    }

    // ---- tempo -------------------------------------------------------------
    println!(
        "Tempo events    : {}{}",
        song.tempo_events.len(),
        if song.tempo_events.is_empty() {
            " (none declared, SMF default applies)"
        } else {
            ""
        }
    );
    println!("Initial tempo   : {:.2} BPM", song.initial_bpm);
    if song.tempo_events.is_empty() {
        // nothing more to say
    } else if (song.max_bpm - song.min_bpm).abs() < 1e-9 {
        println!("Tempo range     : {:.2} BPM (constant)", song.min_bpm);
    } else {
        // Two decimals is what people quote, but a tempo stored as e.g.
        // 3000003 µs/beat is 19.99998 BPM — sites that truncate will call that
        // "19". Showing the unrounded pair makes the discrepancy explainable
        // instead of looking like a parse error.
        println!(
            "Tempo range     : {:.2} .. {:.2} BPM (unrounded: {:.5} .. {:.5})",
            song.min_bpm, song.max_bpm, song.min_bpm, song.max_bpm
        );
    }

    // ---- metre / key -------------------------------------------------------
    let sigs = song.distinct_time_signatures();
    if sigs.is_empty() {
        println!("Time signature  : not declared (4/4 implied)");
    } else {
        let list: Vec<String> = sigs.iter().map(|(n, d)| format!("{n}/{d}")).collect();
        let changes = song.time_signatures.len();
        if changes <= 1 {
            println!("Time signature  : {} (constant)", list[0]);
        } else {
            println!(
                "Time signature  : {} ({} changes)",
                list.join(", "),
                changes
            );
        }
    }

    match song.initial_key() {
        Some(k) => {
            let accidental = match k.sharps {
                0 => "no accidentals".to_string(),
                n if n > 0 => format!("{} sharp{}", n, if n == 1 { "" } else { "s" }),
                n => format!("{} flat{}", -n, if n == -1 { "" } else { "s" }),
            };
            println!("Key signature   : {} ({})", k.label(), accidental);
            // A file may restate the same key on several tracks; only mention a
            // change count when the key genuinely changes at some point.
            let distinct: Vec<(i8, bool)> = {
                let mut v: Vec<(i8, bool)> = Vec::new();
                for k in &song.key_signatures {
                    if !v.contains(&(k.sharps, k.minor)) {
                        v.push((k.sharps, k.minor));
                    }
                }
                v
            };
            if distinct.len() > 1 {
                println!(
                    "                  {} changes, see --verbose",
                    song.key_signatures.len()
                );
            }
        }
        None => println!("Key signature   : not declared"),
    }

    // ---- extent ------------------------------------------------------------
    let secs = song.length_ms / 1000.0;
    println!("Length          : {:.2}s ({:.2} min)", secs, secs / 60.0);
    if !song.notes.is_empty() {
        let min_note = song.notes.iter().map(|n| n.note).min().unwrap();
        let max_note = song.notes.iter().map(|n| n.note).max().unwrap();
        println!("Lowest note     : {} ({})", min_note, note_name(min_note));
        println!("Highest note    : {} ({})", max_note, note_name(max_note));
        if secs > 0.0 {
            println!(
                "Density         : {:.2} notes/s",
                song.notes.len() as f64 / secs
            );
        }
    }

    // ---- can the configured keymap actually play this? ----------------------
    let cfg = Config::load(config.as_deref())?;
    print_coverage(&song, &cfg, verbose);

    if verbose {
        if !song.tempo_events.is_empty() {
            println!();
            println!(
                "{:<10} {:>10} {:>9} {:>10}",
                "TICK", "TIME", "BPM", "US/BEAT"
            );
            println!("{}", "-".repeat(42));
            for ev in &song.tempo_events {
                println!(
                    "{:<10} {:>9.3}s {:>9.2} {:>10}",
                    ev.tick,
                    ev.at_ms / 1000.0,
                    ev.bpm(),
                    ev.micros_per_beat
                );
            }
        }
        if song.time_signatures.len() > 1 {
            println!();
            println!("{:<10} {:>10} {:>8}", "TICK", "TIME", "SIGNATURE");
            println!("{}", "-".repeat(30));
            for s in &song.time_signatures {
                println!("{:<10} {:>9.3}s {:>8}", s.tick, s.at_ms / 1000.0, s.label());
            }
        }
        if song.key_signatures.len() > 1 {
            println!();
            println!("{:<10} {:>10} {:>12}", "TICK", "TIME", "KEY");
            println!("{}", "-".repeat(34));
            for k in &song.key_signatures {
                println!(
                    "{:<10} {:>9.3}s {:>12}",
                    k.tick,
                    k.at_ms / 1000.0,
                    k.label()
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default must stay 0: a fresh playback has to reproduce the MIDI
    /// file exactly, with humanization being an explicit opt-in.
    #[test]
    fn humanize_defaults_to_zero() {
        let cli = Cli::try_parse_from(["pianomi", "play", "-f", "song.mid"]).unwrap();
        match cli.command {
            Commands::Play { humanize, .. } => assert_eq!(humanize, 0),
            _ => panic!("expected the play subcommand"),
        }
    }

    #[test]
    fn humanize_can_still_be_opted_into() {
        let cli =
            Cli::try_parse_from(["pianomi", "play", "-f", "song.mid", "--humanize", "50"]).unwrap();
        match cli.command {
            Commands::Play { humanize, .. } => assert_eq!(humanize, 50),
            _ => panic!("expected the play subcommand"),
        }
    }

    #[test]
    fn direct_mode_margin_is_opt_in_via_config() {
        let cli = Cli::try_parse_from(["pianomi", "play", "-f", "song.mid"]).unwrap();
        match cli.command {
            Commands::Play {
                targeted_extra_gap_ms,
                ..
            } => assert_eq!(targeted_extra_gap_ms, None),
            _ => panic!("expected the play subcommand"),
        }
    }

    /// `0` is the A/B switch that restores the stock key-up margins. Clap must
    /// not swallow it as "no value".
    #[test]
    fn direct_mode_margin_zero_is_parseable() {
        let cli = Cli::try_parse_from([
            "pianomi",
            "play",
            "-f",
            "song.mid",
            "--targeted-extra-gap-ms",
            "0",
        ])
        .unwrap();
        match cli.command {
            Commands::Play {
                targeted_extra_gap_ms,
                ..
            } => assert_eq!(targeted_extra_gap_ms, Some(0.0)),
            _ => panic!("expected the play subcommand"),
        }
    }
}
