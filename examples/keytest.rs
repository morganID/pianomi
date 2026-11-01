//! Verification harness for the main-thread crash.
//!
//! Run on a **worker thread** (exactly like the GUI does):
//!
//! ```text
//! cargo run --release --example keytest            # new path: must succeed
//! cargo run --release --example keytest -- unicode  # old path: must SIGTRAP
//! ```
//!
//! Only `Direction::Release` is used, so nothing is actually typed anywhere.

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use pianomi::keycode::key_for_label;

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
    println!("mode: {mode} — dispatching from a worker thread…");

    let h = std::thread::spawn(move || {
        let mut e = Enigo::new(&Settings::default()).expect("Enigo::new");
        let t = std::time::Instant::now();
        for label in LABELS {
            let key = if legacy {
                Key::Unicode(label.to_lowercase().chars().next().unwrap())
            } else {
                key_for_label(label).expect("label should map to a keycode")
            };
            e.key(key, Direction::Release).expect("dispatch");
        }
        t.elapsed()
    });

    let elapsed = h.join().expect("worker thread panicked");
    println!("OK: {} releases in {:?} — no crash", LABELS.len(), elapsed);
}
