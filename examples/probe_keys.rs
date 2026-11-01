//! Runtime check for [`post_key`]: prove that a black key is delivered as the
//! **shifted character** without the physical Shift key ever being pressed.
//!
//! The harness installs an active event tap (needs Accessibility, same as
//! playback), posts a handful of labels, and prints what the OS actually
//! received — keycode, modifier flags and typed characters.
//!
//! ```text
//! cargo run --release --example probe_keys
//! ```
//!
//! Expected: `8` arrives with no flags and characters "8"; `*` arrives with the
//! Shift flag set and characters "*", and **no** event for keycode 0x38 (the
//! physical Shift key) appears in between.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use core_foundation::runloop::CFRunLoop;
use core_graphics::event::{
    CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
    CallbackResult,
};

use pianomi::keycode::{post_key, ShiftMode};

fn main() {
    let labels = ["8", "*", "s", "S", "q", "Q"];
    // Ctrl layer of the extended octaves.
    let ctrl_labels = ["1", "w", "y", "f"];
    let (tx, rx) = mpsc::channel::<String>();

    let worker = thread::spawn(move || {
        thread::sleep(Duration::from_millis(400));
        for label in labels {
            let ok = post_key(label, true, ShiftMode::Char, false).expect("post key down");
            assert!(ok, "{label} should resolve to a keycode");
            post_key(label, false, ShiftMode::Char, false).expect("post key up");
            thread::sleep(Duration::from_millis(120));
        }
        for label in ctrl_labels {
            post_key(label, true, ShiftMode::Char, true).expect("post ctrl key down");
            post_key(label, false, ShiftMode::Char, true).expect("post ctrl key up");
            thread::sleep(Duration::from_millis(120));
        }
        thread::sleep(Duration::from_millis(200));
        tx.send("done".into()).ok();
    });

    let tap = CGEventTap::with_enabled(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        vec![CGEventType::KeyDown, CGEventType::KeyUp],
        |_proxy, kind, event| {
            let code = event
                .get_integer_value_field(core_graphics::event::EventField::KEYBOARD_EVENT_KEYCODE);
            let flags = event.get_flags();
            let shift = flags.contains(core_graphics::event::CGEventFlags::CGEventFlagShift);
            let ctrl = flags.contains(core_graphics::event::CGEventFlags::CGEventFlagControl);
            println!(
                "{:>4} keycode=0x{:02X} shift={} ctrl={}",
                if matches!(kind, CGEventType::KeyDown) {
                    "DOWN"
                } else {
                    "UP"
                },
                code,
                if shift { "y" } else { "-" },
                if ctrl { "y" } else { "-" },
            );
            CallbackResult::Keep
        },
        || {
            // Pump the run loop until the poster is done.
            let deadline = std::time::Instant::now() + Duration::from_secs(6);
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

    match tap {
        Ok(()) => {
            worker.join().ok();
            println!("\nkeycode 0x38 (physical Shift) must NOT appear above.");
        }
        Err(_) => {
            println!("could not install an event tap — grant Accessibility/Input Monitoring");
            worker.join().ok();
        }
    }
}
