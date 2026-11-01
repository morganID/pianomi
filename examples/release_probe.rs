//! Does every posted key-down get a matching key-up?
//!
//! Installs an event tap, posts a series of notes through the same
//! [`post_key`] path playback uses, and then tallies what the OS actually saw.
//! A keycode with more downs than ups is a stuck key — the in-game piano would
//! keep sounding that note forever.
//!
//! ```text
//! cargo run --release --example release_probe
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use core_foundation::runloop::CFRunLoop;
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult, EventField,
};

use pianomi::keycode::{post_key, ShiftMode};

#[derive(Clone, Copy)]
struct Seen {
    down: bool,
}

fn main() {
    let seen: Arc<Mutex<Vec<(i64, Seen)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let (tx, rx) = std::sync::mpsc::channel::<()>();

    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        let mode = ShiftMode::Char;

        // 1. Plain notes, held then released.
        for label in ["s", "q", "8"] {
            post_key(label, true, mode, false).expect("down");
            thread::sleep(Duration::from_millis(40));
            post_key(label, false, mode, false).expect("up");
            thread::sleep(Duration::from_millis(40));
        }

        // 2. Sharp notes: key-down carries the Shift flag and the character.
        for label in ["S", "Q", "*"] {
            post_key(label, true, mode, false).expect("down");
            thread::sleep(Duration::from_millis(40));
            post_key(label, false, mode, false).expect("up");
            thread::sleep(Duration::from_millis(40));
        }

        // 3. Ctrl-layer notes from the extended octaves.
        for label in ["1", "w", "y"] {
            post_key(label, true, mode, true).expect("down");
            thread::sleep(Duration::from_millis(40));
            post_key(label, false, mode, true).expect("up");
            thread::sleep(Duration::from_millis(40));
        }

        // 4. A natural and its sharp on the SAME physical key, overlapping —
        //    the case where the two bindings share a keycode.
        post_key("s", true, mode, false).expect("down");
        post_key("S", true, mode, false).expect("down");
        thread::sleep(Duration::from_millis(40));
        post_key("S", false, mode, false).expect("up");
        post_key("s", false, mode, false).expect("up");
        thread::sleep(Duration::from_millis(80));

        // 5. Retrigger with ZERO gap: key-up immediately followed by key-down
        //    for the same keycode. This is what a trimmed legato note looks
        //    like, and both events get the same CGEvent timestamp.
        post_key("s", true, mode, false).expect("down");
        thread::sleep(Duration::from_millis(30));
        post_key("s", false, mode, false).expect("up");
        post_key("s", true, mode, false).expect("down");
        thread::sleep(Duration::from_millis(30));
        post_key("s", false, mode, false).expect("up");
        thread::sleep(Duration::from_millis(80));

        // 6. A ctrl key pressed and released twice in a row.
        for _ in 0..2 {
            post_key("1", true, mode, true).expect("down");
            post_key("1", false, mode, true).expect("up");
            thread::sleep(Duration::from_millis(40));
        }

        thread::sleep(Duration::from_millis(200));
        tx.send(()).ok();
    });

    let tap = CGEventTap::with_enabled(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![CGEventType::KeyDown, CGEventType::KeyUp],
        |_proxy, kind, event| {
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            let rec = Seen {
                down: matches!(kind, CGEventType::KeyDown),
            };
            if let Ok(mut v) = sink.lock() {
                v.push((code, rec));
            }
            CallbackResult::Keep
        },
        || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                CFRunLoop::run_in_mode(
                    unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                    Duration::from_millis(100),
                    false,
                );
                if rx.try_recv().is_ok() {
                    break;
                }
            }
        },
    );

    if tap.is_err() {
        println!("could not install an event tap — grant Accessibility / Input Monitoring");
        worker.join().ok();
        return;
    }
    worker.join().ok();

    let events = seen.lock().map(|v| v.clone()).unwrap_or_default();
    if events.is_empty() {
        println!("no events observed — the tap saw nothing");
        return;
    }

    // Tally downs and ups per keycode.
    let mut tally: BTreeMap<i64, (usize, usize)> = BTreeMap::new();
    for (code, rec) in &events {
        let e = tally.entry(*code).or_insert((0, 0));
        if rec.down {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }

    println!("\n{:<10} {:>6} {:>6}  STATE", "KEYCODE", "DOWN", "UP");
    let mut stuck = 0;
    for (code, (downs, ups)) in &tally {
        let state = if downs == ups {
            "ok"
        } else {
            stuck += 1;
            "STUCK"
        };
        println!("0x{:<8X} {:>6} {:>6}  {}", code, downs, ups, state);
    }

    println!();
    if stuck == 0 {
        println!("balanced: every key-down had a matching key-up");
    } else {
        println!("{stuck} keycode(s) left held down");
    }
}
