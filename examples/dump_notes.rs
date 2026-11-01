//! Dump parsed notes as CSV so the parser can be diffed against an independent
//! implementation: `cargo run --release --example dump_notes -- song.mid`.

use pianomi::midi_parser::parse_midi;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("Spring_Thief.mid"));
    let song = parse_midi(&path)?;
    println!("start_ms,duration_ms,note,velocity");
    for n in &song.notes {
        println!(
            "{:.3},{:.3},{},{}",
            n.start_ms, n.duration_ms, n.note, n.velocity
        );
    }
    eprintln!(
        "# {} notes, {:.2}s, {:.2} BPM",
        song.notes.len(),
        song.length_ms / 1000.0,
        song.initial_bpm
    );
    Ok(())
}
