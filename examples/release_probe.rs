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
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField,
};

use pianomi::keycode::{post_key, ShiftMode};

#[derive(Clone, Copy)]
struct Seen {
    down: bool,
    /// Modifier presses arrive as `FlagsChanged`, not key-down.
    is_flags: bool,
    /// The event's own Shift flag (set in both modes).
    shift_flag: bool,
    /// The event's own Control flag.
    ctrl_flag: bool,
}

fn main() {
    let seen: Arc<Mutex<Vec<(i64, Seen)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let (tx, rx) = std::sync::mpsc::channel::<()>();

    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        let mode = match std::env::args().nth(1).as_deref() {
            Some("char") => ShiftMode::Char,
            _ => ShiftMode::Key,
        };
        println!("mode = {mode:?}");

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
        vec![
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
        ],
        |_proxy, kind, event| {
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
            let flags = event.get_flags();
            let rec = Seen {
                down: matches!(kind, CGEventType::KeyDown | CGEventType::FlagsChanged),
                is_flags: matches!(kind, CGEventType::FlagsChanged),
                shift_flag: flags.contains(CGEventFlags::CGEventFlagShift),
                ctrl_flag: flags.contains(CGEventFlags::CGEventFlagControl),
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

    if std::env::var_os("PIANOMI_TRACE").is_some() {
        println!("\nfirst 40 events as the tap saw them:");
        for (i, (code, rec)) in events.iter().take(40).enumerate() {
            let kind = if rec.is_flags {
                "FLAGS"
            } else if rec.down {
                "DOWN "
            } else {
                "UP   "
            };
            println!(
                "  {i:>3}  0x{code:<4X} {kind} shift={} ctrl={}",
                rec.shift_flag, rec.ctrl_flag
            );
        }
    }

    // Tally downs and ups per keycode.
    let mut tally: BTreeMap<i64, (usize, usize)> = BTreeMap::new();
    for (code, rec) in &events {
        // Modifier presses are FlagsChanged and pair up differently; the tally
        // below is about ordinary keys, so skip them.
        if rec.is_flags {
            continue;
        }
        let e = tally.entry(*code).or_insert((0, 0));
        if rec.down {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }

    // Walk the event stream in order. A real modifier press shows up as
    // `FlagsChanged` with the Shift bit set, so that is the global state a game
    // asking `IsKeyDown(LeftShift)` would observe. An ordinary key-down also
    // carries its own Shift flag (both modes set that), which a game reading
    // event flags would observe.
    let mut global_shift = false;
    let mut global_ctrl = false;
    let mut with_real_ctrl = 0usize;
    let mut with_real_shift = 0usize;
    let mut with_flag_only = 0usize;
    let mut with_nothing = 0usize;
    let mut flag_events = 0usize;
    for (code, rec) in &events {
        if rec.is_flags {
            flag_events += 1;
            if *code == 0x38 {
                global_shift = rec.shift_flag;
            }
            if *code == 0x3B {
                global_ctrl = rec.ctrl_flag;
            }
            continue;
        }
        if !rec.down {
            continue;
        }
        if global_ctrl {
            with_real_ctrl += 1;
        }
        if global_shift {
            with_real_shift += 1;
        } else if rec.shift_flag {
            with_flag_only += 1;
        } else {
            with_nothing += 1;
        }
    }
    println!("\nmodifier (FlagsChanged) events seen: {flag_events}");    println!("key-downs with a real Shift held:    {with_real_shift}   <- IsKeyDown sees these");
    println!("key-downs with the Shift flag only:  {with_flag_only}");
    println!("key-downs with no Shift at all:      {with_nothing}");
    println!("key-downs with a real Ctrl held:     {with_real_ctrl}   <- IsKeyDown sees these");

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
