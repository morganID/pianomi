//! Publish a virtual MIDI **destination** and print what arrives on it.
//!
//! The mirror of `midi_probe`: that one connects to a source someone else
//! published, this one publishes a destination so `play --midi-out "<name>"`
//! has something to connect to. Useful for testing the destination code path on
//! a machine with no hardware MIDI ports.
//!
//! ```text
//! cargo run --release --example midi_sink -- "Test Sink" 10
//! ```

use std::time::{Duration, Instant};

use midir::os::unix::VirtualInput;
use midir::{Ignore, MidiInput};

fn main() -> anyhow::Result<()> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Test Sink".to_string());
    let seconds: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let mut input = MidiInput::new("midi-sink")?;
    input.ignore(Ignore::None);

    let start = Instant::now();
    let _conn = input.create_virtual(
        &name,
        move |_stamp, message, _| {
            println!(
                "{:>8.1}ms  {:02X?}",
                start.elapsed().as_secs_f64() * 1000.0,
                message
            );
        },
        (),
    )?;

    println!("published destination {name:?} — waiting {seconds}s");
    std::thread::sleep(Duration::from_secs(seconds));
    Ok(())
}
