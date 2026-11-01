//! Faithful reproduction of the GUI crash.
//!
//! A plain CLI binary never trips the Text Services Manager's main-queue
//! assertion, because no `NSApplication` exists. The menu bar app *does* create
//! one (via `tao`), and that is what arms the assertion. This example does the
//! same: build the event loop on the main thread, then dispatch keys from a
//! worker thread.
//!
//! ```text
//! cargo run --release --example keytest_gui            # new path: OK
//! cargo run --release --example keytest_gui -- unicode  # old path: SIGTRAP
//! ```
//!
//! Only `Direction::Release` is used, so nothing is typed anywhere.

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use pianomi::keycode::key_for_label;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tao::event_loop::{ControlFlow, EventLoopBuilder};

const LABELS: [&str; 27] = [
    "A", "S", "D", "F", "G", "H", "J", "K", "L", ";", "Q", "W", "E", "R", "T", "Y", "U", "I", "O",
    "P", "Z", "X", "C", "V", "B", "N", "M",
];

fn main() {
    let legacy = std::env::args().any(|a| a == "unicode");
    let mode = if legacy {
        "Key::Unicode (old)"
    } else {
        "Key::Other (new)"
    };
    println!("mode: {mode}");

    // Same call the menu bar app makes first: this creates the NSApplication.
    let event_loop = EventLoopBuilder::new().build();
    println!("NSApplication created — now dispatching from a worker thread…");

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let mut e = Enigo::new(&Settings::default()).expect("Enigo::new");
        let t = Instant::now();
        for label in LABELS {
            let key = if legacy {
                Key::Unicode(label.to_lowercase().chars().next().unwrap())
            } else {
                key_for_label(label).expect("label should map to a keycode")
            };
            if let Err(err) = e.key(key, Direction::Release) {
                let _ = tx.send(format!("ERR: {err:?}"));
                return;
            }
        }
        let _ = tx.send(format!(
            "OK: {} releases in {:?} — no crash",
            LABELS.len(),
            t.elapsed()
        ));
    });

    let deadline = Instant::now() + Duration::from_secs(8);
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
