//! Listen on a MIDI input and print every message it receives.
//!
//! Used to verify `play --midi-out` end to end: start a playback that publishes
//! a virtual source, then run this example, which waits for that port to appear,
//! connects to it and dumps what actually arrives.
//!
//! ```text
//! cargo run --release --example midi_probe -- "Pianomi" 8
//! ```

use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput};

fn main() -> anyhow::Result<()> {
    let want = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Pianomi".to_string());
    let seconds: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let mut input = MidiInput::new("midi-probe")?;
    input.ignore(Ignore::None);

    // The virtual source only exists while the player is running, so wait for it.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_report = Instant::now();
    let port = loop {
        let ports = input.ports();
        let found = ports
            .iter()
            .find(|p| {
                input
                    .port_name(p)
                    .map(|n| n.contains(&want))
                    .unwrap_or(false)
            })
            .cloned();
        if let Some(p) = found {
            break p;
        }
        if std::env::var_os("MIDI_PROBE_VERBOSE").is_some() && last_report.elapsed().as_secs() >= 1
        {
            let names: Vec<String> = ports
                .iter()
                .filter_map(|p| input.port_name(p).ok())
                .collect();
            eprintln!("waiting… {} source(s): {:?}", names.len(), names);
            last_report = Instant::now();
        }
        if Instant::now() > deadline {
            let names: Vec<String> = ports
                .iter()
                .filter_map(|p| input.port_name(p).ok())
                .collect();
            anyhow::bail!(
                "no MIDI input matching {want:?} appeared — inputs now: {:?}",
                names
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let name = input.port_name(&port)?;
    println!("connected to {name:?}");

    let start = Instant::now();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count_in = count.clone();
    let mut _conn = input.connect(
        &port,
        "midi-probe",
        move |_stamp, message, _| {
            count_in.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let kind = match message.first().map(|b| b & 0xF0) {
                Some(0x90) if message.get(2).copied().unwrap_or(0) > 0 => "note-on ",
                Some(0x80) => "note-off",
                Some(0x90) => "note-off",
                Some(0xB0) => "control ",
                _ => "other   ",
            };
            println!(
                "{:>8.1}ms  {}  {:02X?}",
                start.elapsed().as_secs_f64() * 1000.0,
                kind,
                message
            );
        },
        (),
    )?;

    std::thread::sleep(Duration::from_secs(seconds));
    println!(
        "received {} messages",
        count.load(std::sync::atomic::Ordering::Relaxed)
    );
    Ok(())
}
