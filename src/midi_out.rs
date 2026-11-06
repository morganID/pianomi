//! MIDI output: replay a song as MIDI messages instead of keystrokes.
//!
//! This is the second way to get notes out of the app. The default path types
//! characters into whatever window has focus, which is what a Roblox piano
//! needs — but it is lossy by construction: the in-game piano only has 88 keys
//! reachable through three modifier layers, timing is quantised by the OS key
//! repeat behaviour, and nothing downstream can tell which note was which.
//!
//! Sending real MIDI has none of those limits. Two ways to reach a receiver:
//!
//! * **Virtual source** — we publish a port that other apps see as an *input
//!   device* (CoreMIDI on macOS). Nothing else has to exist first: open the app
//!   that should receive the notes, pick the port from its MIDI input list, then
//!   press play here.
//! * **Destination** — connect straight to a port someone else published (an
//!   IAC bus, a synth, a DAW input, a loopback to another tool).
//!
//! Either way the receiver gets ordinary note-on/note-off with the file's own
//! channel, velocity and timing — no keymap, no Shift, no range limit.

use anyhow::{anyhow, Result};
use midir::{MidiOutput, MidiOutputConnection};

/// Name published when `--midi-out` is used without a value.
pub const DEFAULT_VIRTUAL_NAME: &str = "Pianomi";

/// Client name shown to CoreMIDI; also what other apps see in their logs.
const CLIENT_NAME: &str = "pianomi";

/// A live MIDI connection that can send note events.
pub struct MidiOut {
    conn: MidiOutputConnection,
    /// The port name this connection is attached to.
    pub name: String,
    /// `true` when we published the port ourselves, `false` when we connected
    /// to a destination that already existed.
    pub virtual_port: bool,
    /// Bitmask of channels we have sent notes on, so the panic message only
    /// goes where it is needed.
    used_channels: u16,
}

impl MidiOut {
    /// One-line description for logs.
    pub fn describe(&self) -> String {
        if self.virtual_port {
            format!(
                "virtual MIDI source \"{}\" (select it in the receiving app)",
                self.name
            )
        } else {
            format!("MIDI destination \"{}\"", self.name)
        }
    }

    /// Note on. Velocity is clamped to 1..=127 — a note-on with velocity 0 is
    /// a note-off by definition, so it can never be used to start a note.
    pub fn note_on(&mut self, channel: u8, note: u8, velocity: u8) -> Result<()> {
        self.used_channels |= 1 << (channel & 0x0F);
        self.send(&note_on_msg(channel, note, velocity))
    }

    /// Note off, sent as a real `0x80` message rather than note-on with
    /// velocity 0: both are legal, but some receivers only implement one.
    pub fn note_off(&mut self, channel: u8, note: u8) -> Result<()> {
        self.send(&note_off_msg(channel, note))
    }

    /// Control change (e.g. sustain pedal CC64, channel volume CC7, pan CC10).
    ///
    /// Forwarding these is what makes a performance sound like the original
    /// instead of a dry, pedal-less run of notes. `used_channels` is updated so
    /// the panic button still silences exactly the channels we touched.
    pub fn control_change(&mut self, channel: u8, controller: u8, value: u8) -> Result<()> {
        self.used_channels |= 1 << (channel & 0x0F);
        self.send(&[0xB0 | (channel & 0x0F), controller & 0x7F, value & 0x7F])
    }

    /// Panic button: silence every channel we have used. Sent on stop and at
    /// the end of playback so a receiver can never be left with a stuck note if
    /// we were killed mid-chord.
    ///
    /// Only the channels that actually carried notes are touched — spraying
    /// All Sound Off over all 16 channels is legal but rude to a DAW that has
    /// its own material on the others.
    pub fn all_notes_off(&mut self) -> Result<()> {
        if self.used_channels == 0 {
            return Ok(());
        }
        for ch in 0..16u8 {
            if self.used_channels & (1 << ch) == 0 {
                continue;
            }
            // CC 123 = All Notes Off, CC 120 = All Sound Off.
            self.send(&[0xB0 | ch, 123, 0])?;
            self.send(&[0xB0 | ch, 120, 0])?;
        }
        self.used_channels = 0;
        Ok(())
    }

    fn send(&mut self, msg: &[u8]) -> Result<()> {
        self.conn
            .send(msg)
            .map_err(|e| anyhow!("MIDI send failed: {e}"))
    }
}

/// Every MIDI destination this machine currently exposes.
pub fn list_destinations() -> Result<Vec<String>> {
    let out = MidiOutput::new(CLIENT_NAME)
        .map_err(|e| anyhow!("could not initialise MIDI output: {e}"))?;
    Ok(out
        .ports()
        .iter()
        .filter_map(|p| out.port_name(p).ok())
        .collect())
}

/// Open the output described by `spec`.
///
/// * `None` or an empty string → publish a virtual source called
///   [`DEFAULT_VIRTUAL_NAME`].
/// * A name that matches a destination (exact first, then case-insensitive
///   substring) → connect to it.
/// * Anything else → publish a virtual source with that name, so
///   `--midi-out "My Piano"` is a way to name the port you publish.
pub fn open(spec: Option<&str>) -> Result<MidiOut> {
    let want = spec.map(str::trim).unwrap_or("");
    if want.is_empty() {
        return open_virtual(DEFAULT_VIRTUAL_NAME);
    }

    let destinations = list_destinations().unwrap_or_default();
    match pick_destination(want, &destinations) {
        Some(name) => open_destination(name),
        None => open_virtual(want),
    }
}

/// Choose a destination for `want`: an exact name match first, then the first
/// case-insensitive substring match.
///
/// Exact-first matters because "IAC Driver Bus 1" is a prefix of
/// "IAC Driver Bus 10"; a plain `contains` would pick the wrong one.
fn pick_destination<'a>(want: &str, destinations: &'a [String]) -> Option<&'a String> {
    if let Some(exact) = destinations.iter().find(|d| d.eq_ignore_ascii_case(want)) {
        return Some(exact);
    }
    let lower = want.to_lowercase();
    destinations
        .iter()
        .find(|d| d.to_lowercase().contains(&lower))
}

/// Connect to an existing destination by exact name.
fn open_destination(name: &str) -> Result<MidiOut> {
    let out = MidiOutput::new(CLIENT_NAME)
        .map_err(|e| anyhow!("could not initialise MIDI output: {e}"))?;

    let ports = out.ports();
    let names: Vec<String> = ports.iter().filter_map(|p| out.port_name(p).ok()).collect();
    let index = names.iter().position(|n| n == name).ok_or_else(|| {
        anyhow!(
            "no MIDI destination named {name:?} — available: {}",
            if names.is_empty() {
                "(none)".to_string()
            } else {
                names.join(", ")
            }
        )
    })?;

    let conn = out
        .connect(&ports[index], CLIENT_NAME)
        .map_err(|e| anyhow!("could not connect to {name:?}: {e}"))?;

    Ok(MidiOut {
        conn,
        name: name.to_string(),
        virtual_port: false,
        used_channels: 0,
    })
}

/// Publish a virtual source other applications can connect to.
fn open_virtual(name: &str) -> Result<MidiOut> {
    let out = MidiOutput::new(CLIENT_NAME)
        .map_err(|e| anyhow!("could not initialise MIDI output: {e}"))?;

    #[cfg(unix)]
    {
        use midir::os::unix::VirtualOutput;
        let conn = out
            .create_virtual(name)
            .map_err(|e| anyhow!("could not publish virtual MIDI source {name:?}: {e}"))?;
        Ok(MidiOut {
            conn,
            name: name.to_string(),
            virtual_port: true,
            used_channels: 0,
        })
    }

    #[cfg(not(unix))]
    {
        let _ = out;
        Err(anyhow!(
            "this platform cannot publish a virtual MIDI port — pass the name of an \
             existing destination instead (see `midi-ports`)"
        ))
    }
}

/// Encode a note-on. Status nibble `0x90` plus the channel, then note and
/// velocity as 7-bit values.
fn note_on_msg(channel: u8, note: u8, velocity: u8) -> [u8; 3] {
    [0x90 | (channel & 0x0F), note & 0x7F, velocity.clamp(1, 127)]
}

/// Encode a note-off (`0x80`). Release velocity is fixed at 64 — it has no
/// audible effect on any common receiver.
fn note_off_msg(channel: u8, note: u8) -> [u8; 3] {
    [0x80 | (channel & 0x0F), note & 0x7F, 0x40]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_messages_carry_status_byte_channel_and_payload() {
        assert_eq!(note_on_msg(0, 60, 100), [0x90, 60, 100]);
        assert_eq!(note_on_msg(3, 61, 100), [0x93, 61, 100]);
        assert_eq!(note_off_msg(0, 60), [0x80, 60, 0x40]);
        assert_eq!(note_off_msg(15, 127), [0x8F, 127, 0x40]);
    }

    #[test]
    fn velocity_zero_is_raised_to_one() {
        // 0x90 with velocity 0 is a note-off, i.e. silence instead of a note.
        assert_eq!(note_on_msg(0, 60, 0), [0x90, 60, 1]);
    }

    #[test]
    fn out_of_range_values_are_masked() {
        assert_eq!(note_on_msg(0x1F, 200, 200), [0x9F, 200 & 0x7F, 127]);
    }

    fn dests(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_destination_wins_over_a_longer_prefix_match() {
        let d = dests(&["IAC Driver Bus 1", "IAC Driver Bus 10"]);
        assert_eq!(
            pick_destination("IAC Driver Bus 1", &d).map(String::as_str),
            Some("IAC Driver Bus 1")
        );
    }

    #[test]
    fn matching_is_case_insensitive_and_falls_back_to_substring() {
        let d = dests(&["IAC Driver Bus 1", "Test Sink"]);
        assert_eq!(
            pick_destination("test sink", &d).map(String::as_str),
            Some("Test Sink")
        );
        assert_eq!(
            pick_destination("sink", &d).map(String::as_str),
            Some("Test Sink")
        );
    }

    #[test]
    fn no_match_means_publish_a_virtual_port_instead() {
        let d = dests(&["Test Sink"]);
        assert!(pick_destination("My Piano", &d).is_none());
        assert!(pick_destination("My Piano", &[]).is_none());
    }
}
