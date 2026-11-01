//! Convert an Online Sequencer note dump into a Standard MIDI File.
//!
//! ```text
//! cargo run --release --example oscheck                       # os_data.txt -> Spring_Thief.mid @125
//! cargo run --release --example oscheck -- in.txt out.mid 125
//! ```

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().unwrap_or_else(|| "os_data.txt".into()));
    let output = PathBuf::from(args.next().unwrap_or_else(|| "Spring_Thief.mid".into()));
    let bpm: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(125.0);

    let raw = std::fs::read_to_string(&input)?;
    let n = pianomi::os2midi::write_os_midi(&raw, &output, bpm)?;
    println!(
        "converted {} -> {} ({bpm} BPM, {n} bytes)",
        input.display(),
        output.display()
    );
    Ok(())
}
