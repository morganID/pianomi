//! End-to-end test of the exact path the menu bar app uses on Play.
//!
//! Builds a tao event loop (which creates `NSApplication` on the main thread,
//! just like the GUI) and then runs the real [`Player`] on a worker thread —
//! the configuration that used to abort with `dispatch_assert_queue` / SIGTRAP.
//!
//! ```text
//! cargo run --release --example e2e
//! ```
//!
//! Expects `playback finished OK`. A `Trace/BPT trap` here means the crash is
//! back. Keystrokes are delivered to whatever window is focused; they are not
//! read by this process, so they are harmless.

use pianomi::config::Config;
use pianomi::humanizer::Humanizer;
use pianomi::key_mapper::KeyMapper;
use pianomi::midi_parser::{Note, Song};
use pianomi::player::{OutputMode, Player};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tao::event_loop::{ControlFlow, EventLoopBuilder};

fn main() {
    // Same first call the menu bar app makes.
    let event_loop = EventLoopBuilder::new().build();
    println!("NSApplication created; starting playback on a worker thread…");

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let cfg = Config::load(None).expect("load config");
        let mut player_cfg = cfg.player.clone();
        player_cfg.start_delay_ms = 300; // keep the test short

        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let humanizer = Humanizer::new(0, 1.0, cfg.humanize.clone());

        // C4 E4 G4 C#4 — whites and a black key, so the keycode table is hit
        // for both branches.
        let song = Song {
            notes: [60u8, 64, 67, 61]
                .iter()
                .enumerate()
                .map(|(i, &n)| Note {
                    note: n,
                    channel: 0,
                    start_ms: i as f64 * 180.0,
                    duration_ms: 200.0,
                    velocity: 90,
                })
                .collect(),
            ticks_per_beat: 480,
            length_ms: 800.0,
            channels_used: vec![0],
            ..Default::default()
        };

        let result = Player::new(
            mapper,
            humanizer,
            player_cfg,
            false,
            OutputMode::Keys,
            None,
            None,
            None,
        )
        .and_then(|mut p| p.play(&song, 1, false));
        let msg = match result {
            Ok(()) => "playback finished OK".to_string(),
            Err(e) => format!("ERROR: {e}"),
        };
        let _ = tx.send(msg);
    });

    let deadline = Instant::now() + Duration::from_secs(15);
    event_loop.run(move |_event, _target, control_flow| {
        if let Ok(msg) = rx.try_recv() {
            println!("{msg}");
            *control_flow = ControlFlow::Exit;
            return;
        }
        if Instant::now() > deadline {
            println!("TIMEOUT — worker never reported back");
            *control_flow = ControlFlow::Exit;
            return;
        }
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50));
    });
}
