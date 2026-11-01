//! Translate a configured key label (`"A"`, `"z"`, `";"`, `"Space"`) into an
//! [`enigo::Key`] that can be dispatched from **any** thread.
//!
//! ## Why not `Key::Unicode`?
//!
//! On macOS `enigo` turns a character into a virtual keycode by walking every
//! keycode from 0 to 127 twice (once plain, once shifted) through
//! `UCKeyTranslate`, which needs the Text Services Manager. Two problems:
//!
//! 1. **It crashes off the main thread.** `TSMGetInputSourceProperty` asserts
//!    it is running on the main dispatch queue. Our GUI plays music on a worker
//!    thread, so the assertion aborts the process:
//!
//!    ```text
//!    _dispatch_assert_queue_fail
//!      dispatch_assert_queue
//!        HIToolbox islGetInputSourceListWithAdditions
//!          TSMGetInputSourceProperty
//!            enigo::keycode_to_string
//!              Enigo::key  <- Player::press  <- thread::spawn
//!    zsh: trace trap
//!    ```
//!
//! 2. **It is slow.** ~256 layout lookups for *every single keystroke*, which
//!    adds milliseconds of jitter to notes that should be sub-millisecond.
//!
//! `Key::Other(code)` maps straight onto a `CGKeyCode` and posts the event with
//! no layout work at all. So on macOS we keep a static table of US-ANSI virtual
//! keycodes and never touch the Text Services Manager. Other platforms keep
//! using `Key::Unicode`, which is cheap and thread-safe there.
//!
//! ## Verification
//!
//! The table below was diffed against the live layout of this machine
//! (`com.apple.keylayout.ABC`, i.e. US QWERTY) by walking keycodes 0..127
//! through `UCKeyTranslate` on the main thread: **50/50 entries match**.
//! (The one apparent mismatch, Return, is only cosmetic — macOS reports that
//! key as `'\r'` while the table is keyed on `'\n'`; both map to `0x24`.)
//!
//! If you ever support a non-QWERTY layout, the character→keycode mapping
//! changes, but the *physical* positions do not — so Roblox, which binds by
//! physical key, would still behave sensibly.

use enigo::Key;

/// macOS virtual keycode for the left Shift key (`kVK_Shift` in HIToolbox).
pub const SHIFT_VK: u32 = 0x38;

/// macOS virtual keycode for the left Control key (`kVK_Control`).
/// The extended octaves of the in-game piano are reached with Ctrl.
pub const CTRL_VK: u32 = 0x3B;

#[cfg(target_os = "macos")]
use core_graphics::event::CGEventFlags;

/// The Shift bit in a `CGEvent`'s flags field.
#[cfg(target_os = "macos")]
pub const SHIFT_FLAG: CGEventFlags = CGEventFlags::CGEventFlagShift;

/// The Control bit in a `CGEvent`'s flags field.
#[cfg(target_os = "macos")]
pub const CTRL_FLAG: CGEventFlags = CGEventFlags::CGEventFlagControl;

/// Human-readable description of how keys are dispatched on this platform.
/// Printed at startup so it is obvious whether a build contains the crash fix.
pub fn dispatch_mode() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "raw virtual keycodes (Text Services Manager bypassed)"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "enigo Key::Unicode"
    }
}

/// Resolve a key label from the config into an [`enigo::Key`].
///
/// Returns `None` for labels we cannot map to a physical key (on macOS this
/// means "not in the US-ANSI table"); callers should warn instead of falling
/// back to `Key::Unicode`, which would reintroduce the main-thread crash.
pub fn key_for_label(label: &str) -> Option<Key> {
    let lower = label.trim().to_ascii_lowercase();

    if let Some(k) = named_key(&lower) {
        return Some(k);
    }

    let c = lower.chars().next()?;
    char_key(c)
}

/// Whether typing `label` needs Shift on a US layout.
///
/// Used by the `info` coverage report, which must give the same answer on every
/// platform — [`resolve_key`] only knows about modifiers on macOS.
pub fn label_is_shifted(label: &str) -> bool {
    let lower = label.trim().to_ascii_lowercase();
    if named_key(&lower).is_some() {
        return false;
    }
    match label.trim().chars().next() {
        Some(c) => !(c.is_ascii_lowercase() || c.is_ascii_digit()),
        None => false,
    }
}

/// Spelled-out keys, so `key = "Space"` works as well as `key = " "`.
fn named_key(name: &str) -> Option<Key> {
    Some(match name {
        "space" | "spacebar" => char_key(' ')?,
        "tab" => char_key('\t')?,
        "enter" | "return" => char_key('\n')?,
        "minus" => char_key('-')?,
        "equal" | "equals" => char_key('=')?,
        "comma" => char_key(',')?,
        "period" | "dot" => char_key('.')?,
        "slash" => char_key('/')?,
        "semicolon" => char_key(';')?,
        "quote" | "apostrophe" => char_key('\'')?,
        "backslash" => char_key('\\')?,
        "bracketleft" => char_key('[')?,
        "bracketright" => char_key(']')?,
        "backquote" | "grave" => char_key('`')?,
        _ => return None,
    })
}

/// Resolve a configured key label into the base [`enigo::Key`] plus a flag that
/// says whether the left Shift modifier must be held while pressing it.
///
/// This is what makes the **Shift-based black-key layout** work: the Roblox
/// Virtual Piano maps a sharp to the *same physical key* as the white note one
/// semitone below, played with Shift held (e.g. `!` = Shift+`1`, `Q` = Shift+`q`).
/// The config therefore stores the literal character (`"!"`, `"Q"`, …) and we
/// derive the modifier here.
///
/// On macOS the base key is a raw `CGKeyCode` (via [`macos_keycode`]) so it is
/// safe off the main thread. On other platforms we just emit the character with
/// `Key::Unicode`, which already carries the shift for uppercase/symbols.
/// Note that the `shift` flag is **not** meant to be implemented by pressing
/// the real Shift key — see [`post_key`], which stamps the modifier onto the
/// key event itself.
pub fn resolve_key(label: &str) -> Option<(Key, bool)> {
    let c = label.trim().chars().next()?;

    #[cfg(target_os = "macos")]
    {
        let (lower, shift) = match c {
            // shifted digit symbols -> base digit + Shift
            '!' => ('1', true),
            '@' => ('2', true),
            '#' => ('3', true),
            '$' => ('4', true),
            '%' => ('5', true),
            '^' => ('6', true),
            '&' => ('7', true),
            '*' => ('8', true),
            '(' => ('9', true),
            ')' => ('0', true),
            // any other uppercase letter -> its lowercase base + Shift
            c if c.is_ascii_uppercase() => (c.to_ascii_lowercase(), true),
            // lowercase letter or digit -> no shift
            c if c.is_ascii_lowercase() || c.is_ascii_digit() => (c, false),
            _ => return None,
        };
        let base = macos_keycode(lower)?;
        Some((Key::Other(u32::from(base)), shift))
    }
    #[cfg(not(target_os = "macos"))]
    {
        // `Key::Unicode` carries the shift for uppercase/symbol chars itself.
        Some((Key::Unicode(c), false))
    }
}

// --- macOS: US-ANSI virtual keycodes ---------------------------------------

#[cfg(target_os = "macos")]
fn char_key(c: char) -> Option<Key> {
    macos_keycode(c).map(|k| Key::Other(u32::from(k)))
}

/// `kVK_*` constants from `<HIToolbox/Events.h>`. These are *physical* key
/// positions, so pressing them reproduces exactly what a US keyboard would send
/// and Roblox sees the same `KeyCode` a human would produce.
#[cfg(target_os = "macos")]
fn macos_keycode(c: char) -> Option<u16> {
    Some(match c {
        // letters
        'a' => 0x00,
        's' => 0x01,
        'd' => 0x02,
        'f' => 0x03,
        'h' => 0x04,
        'g' => 0x05,
        'z' => 0x06,
        'x' => 0x07,
        'c' => 0x08,
        'v' => 0x09,
        'b' => 0x0B,
        'q' => 0x0C,
        'w' => 0x0D,
        'e' => 0x0E,
        'r' => 0x0F,
        'y' => 0x10,
        't' => 0x11,
        'o' => 0x1F,
        'u' => 0x20,
        'i' => 0x22,
        'p' => 0x23,
        'l' => 0x25,
        'j' => 0x26,
        'k' => 0x28,
        'n' => 0x2D,
        'm' => 0x2E,
        // digits
        '1' => 0x12,
        '2' => 0x13,
        '3' => 0x14,
        '4' => 0x15,
        '5' => 0x17,
        '6' => 0x16,
        '7' => 0x1A,
        '8' => 0x1C,
        '9' => 0x19,
        '0' => 0x1D,
        // punctuation (unshifted)
        '=' => 0x18,
        '-' => 0x1B,
        ']' => 0x1E,
        '[' => 0x21,
        '\'' => 0x27,
        ';' => 0x29,
        '\\' => 0x2A,
        ',' => 0x2B,
        '/' => 0x2C,
        '.' => 0x2F,
        '`' => 0x32,
        // whitespace / control
        '\t' => 0x30,
        ' ' => 0x31,
        '\n' | '\r' => 0x24,
        _ => return None,
    })
}

// --- everything else: characters are fine here ------------------------------

#[cfg(not(target_os = "macos"))]
fn char_key(c: char) -> Option<Key> {
    Some(Key::Unicode(c))
}

// --- macOS: post the character, never the Shift key ------------------------

/// Post one key event for `label` on macOS, carrying the character's own
/// modifier flag instead of touching the physical Shift key.
///
/// ## Why not press Shift?
///
/// The obvious implementation of a black key is "hold Shift, tap `8`, release
/// Shift". It is also the wrong one for playback:
///
/// * **Races at speed.** Three events per black note (Shift↓, key↓, Shift↑)
///   means the modifier can still be down when the *next* note's key-down is
///   posted, or its release can land after the following Shift↓. At 16th-note
///   tempos with chords the two streams interleave and notes come out as the
///   wrong pitch.
/// * **It leaks into everything else.** Any white key pressed while Shift
///   happens to be down is delivered as its sharp (`8` → `*`, `s` → `S`).
/// * **It fights the user.** Real hardware Shift state is global: it affects
///   the focused app, not just the piano.
///
/// Instead we build a `CGEvent` for the *base* keycode and stamp
/// `kCGEventFlagMaskShift` onto the event, plus the exact character via
/// `CGEventKeyboardSetUnicodeString`. The receiving app sees a normal `*`
/// keystroke — keycode, modifier flags and typed character all correct — while
/// the system-wide Shift state is never modified. One event per note, no
/// ordering hazards.
///
/// If the experience asks Roblox's own input state instead of reading the
/// event (`UserInputService:IsKeyDown(LeftShift)`), the flag is invisible to it
/// and [`ShiftMode::Key`] is needed: real Shift, but pressed and released
/// around this single event rather than held for the whole note. `Key` also
/// stamps the flag and the typed character, so it is a superset of `Char` and
/// is the default — Roblox's Virtual Piano reads `IsKeyDown`, so `Char` alone
/// delivers every sharp as the natural below it.
///
/// Returns `Ok(false)` when the label has no keycode on this layout.
#[cfg(target_os = "macos")]
pub fn post_key(label: &str, down: bool, mode: ShiftMode, ctrl: bool) -> anyhow::Result<bool> {
    let Some((key, shifted)) = resolve_key(label) else {
        return Ok(false);
    };
    let Key::Other(vk) = key else {
        return Ok(false);
    };
    let vk = vk as u16;
    // A Ctrl combination produces a control character, not a glyph, so there is
    // no text to attach to those events.
    let ch = if ctrl {
        None
    } else {
        label.trim().chars().next()
    };

    if !shifted && !ctrl {
        mac_post::post(vk, down, false, false, ch)?;
        return Ok(true);
    }

    match mode {
        ShiftMode::Char => mac_post::post(vk, down, shifted, ctrl, ch)?,
        ShiftMode::Key => {
            // Real modifiers, but scoped to this one event: down, key, up.
            // Never held across the note, so they cannot bleed into a later key.
            if ctrl {
                mac_post::post_modifier(CTRL_VK as u16, CTRL_FLAG, true)?;
            }
            if shifted {
                mac_post::post_modifier(SHIFT_VK as u16, SHIFT_FLAG, true)?;
            }
            // The key event also carries the flag and the typed character.
            // Roblox's Virtual Piano asks `IsKeyDown(LeftShift)` and only sees
            // the real key above; any game that reads the modifier flag or the
            // typed character instead sees those here. One event serves both.
            mac_post::post(vk, down, shifted, ctrl, ch)?;
            if shifted {
                mac_post::post_modifier(SHIFT_VK as u16, SHIFT_FLAG, false)?;
            }
            if ctrl {
                mac_post::post_modifier(CTRL_VK as u16, CTRL_FLAG, false)?;
            }
        }
    }
    Ok(true)
}

/// How a sharp (`!`, `Q`, `S`, …) should be delivered.
///
/// The game has to learn that a sharp was intended, and there are two ways it
/// can do that. Which one works depends on how the experience reads input, so
/// both are implemented and the choice is a config value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftMode {
    /// One event: the base keycode with the Shift **flag** and the literal
    /// character attached. Nothing global changes. Works when the game reads
    /// the typed character or the event's modifier flags.
    Char,
    /// Three events: real Shift down, key, Shift up — the same thing a human
    /// types. Needed when the game asks "is Shift held?" through its own input
    /// state (Roblox `UserInputService:IsKeyDown`), which never sees a flag.
    /// The key event still carries the flag and the typed character, so this
    /// also covers anything `Char` covers.
    Key,
}

/// Parse `player.shift_mode` from the config; anything unrecognised falls back
/// to [`ShiftMode::Key`], which is the one Roblox's Virtual Piano needs.
pub fn parse_shift_mode(s: &str) -> ShiftMode {
    match s.trim().to_ascii_lowercase().as_str() {
        "char" | "flag" | "unicode" => ShiftMode::Char,
        _ => ShiftMode::Key,
    }
}

#[cfg(target_os = "macos")]
mod mac_post {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use std::cell::RefCell;

    thread_local! {
        /// One event source per thread: creating it is a syscall and playback
        /// posts thousands of events.
        static SOURCE: RefCell<Option<CGEventSource>> = const { RefCell::new(None) };
    }

    fn source() -> anyhow::Result<CGEventSource> {
        SOURCE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(
                    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                        .map_err(|_| anyhow::anyhow!("could not create a CGEventSource"))?,
                );
            }
            slot.clone()
                .ok_or_else(|| anyhow::anyhow!("CGEventSource unavailable"))
        })
    }

    pub fn post(
        vk: u16,
        down: bool,
        shifted: bool,
        ctrl: bool,
        ch: Option<char>,
    ) -> anyhow::Result<()> {
        let ev = CGEvent::new_keyboard_event(source()?, vk, down)
            .map_err(|_| anyhow::anyhow!("could not create a CGEvent"))?;

        let mut flags = CGEventFlags::empty();
        if shifted {
            flags |= CGEventFlags::CGEventFlagShift;
        }
        if ctrl {
            flags |= CGEventFlags::CGEventFlagControl;
        }
        ev.set_flags(flags);

        // Only the key-down carries text; a key-up with a string attached makes
        // some apps treat it as a second text input.
        if down {
            if let Some(c) = ch {
                ev.set_string(&c.to_string());
            }
        }

        ev.post(CGEventTapLocation::HID);
        Ok(())
    }

    /// Press or release a modifier key itself.
    ///
    /// A modifier key event carries the *new* modifier state in its flags
    /// field, and that field is what the system uses to update the global
    /// modifier state. Posting `0x38` with empty flags is therefore a Shift
    /// *release*, not a press: the event is delivered, but `IsKeyDown` never
    /// changes. So the flag has to be set on the down event.
    pub fn post_modifier(vk: u16, flag: CGEventFlags, down: bool) -> anyhow::Result<()> {
        let ev = CGEvent::new_keyboard_event(source()?, vk, down)
            .map_err(|_| anyhow::anyhow!("could not create a CGEvent"))?;
        ev.set_flags(if down { flag } else { CGEventFlags::empty() });
        ev.post(CGEventTapLocation::HID);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roblox's Virtual Piano asks `IsKeyDown(LeftShift)`, which never sees an
    /// event flag, so a sharp only plays correctly when a real Shift goes down.
    /// Anything unrecognised must therefore land on `Key`, not `Char`.
    #[test]
    fn shift_mode_defaults_to_the_one_roblox_needs() {
        assert_eq!(parse_shift_mode("key"), ShiftMode::Key);
        assert_eq!(parse_shift_mode("KEY"), ShiftMode::Key);
        assert_eq!(parse_shift_mode(""), ShiftMode::Key);
        assert_eq!(parse_shift_mode("nonsense"), ShiftMode::Key);
    }

    #[test]
    fn char_mode_is_still_reachable() {
        assert_eq!(parse_shift_mode("char"), ShiftMode::Char);
        assert_eq!(parse_shift_mode(" char "), ShiftMode::Char);
    }

    /// The shipped config must match the default the parser assumes; a stale
    /// `shift_mode = "char"` in the file would silently undo the fix.
    #[test]
    fn builtin_config_uses_key_mode() {
        let cfg = crate::config::Config::load(None).expect("builtin config");
        assert_eq!(parse_shift_mode(&cfg.player.shift_mode), ShiftMode::Key);
    }
}
