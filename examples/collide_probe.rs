//! Temporary diagnostic: quantify physical-key collisions in real songs.
//! `cargo run --release --example collide_probe -- song.mid [more.mid ...]`

use pianomi::config::Config;
use pianomi::key_mapper::KeyMapper;
use pianomi::midi_parser::parse_midi;
use std::collections::HashMap;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let cfg: Config = toml::from_str(Config::builtin())?;
    let mapper = KeyMapper::from_config(&cfg.keyboard);

    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    for path in paths {
        let song = parse_midi(&path)?;
        // (start rounded to ms, physical key) -> distinct pitches
        let mut same_tick: HashMap<(u64, String), Vec<u8>> = HashMap::new();
        let mut overlap_pairs = 0usize;
        let mut overlaps_len_ms = 0.0f64;

        // per physical key, notes sorted by start
        let mut by_key: HashMap<String, Vec<(f64, f64, u8)>> = HashMap::new();
        for n in &song.notes {
            if let Some(b) = mapper.key_for_note(n.note) {
                by_key.entry(b.physical_id()).or_default().push((
                    n.start_ms,
                    n.start_ms + n.duration_ms.max(cfg.player.min_hold_ms),
                    n.note,
                ));
            }
        }
        for (phys, mut list) in by_key {
            list.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for i in 0..list.len() {
                // truly simultaneous = same 1 ms tick, different pitch
                for j in (i + 1)..list.len() {
                    if (list[j].0 - list[i].0).round() as i64 != 0 {
                        break;
                    }
                    if list[j].2 != list[i].2 {
                        same_tick
                            .entry((list[i].0.round() as u64, phys.clone()))
                            .or_default()
                            .push(list[j].2);
                    }
                }
                // overlapping durations (the real problem)
                for j in (i + 1)..list.len() {
                    if list[j].0 >= list[i].1 {
                        break;
                    }
                    overlap_pairs += 1;
                    overlaps_len_ms += list[i].1 - list[j].0;
                }
            }
        }

        let simultaneous: usize = same_tick.values().map(|v| v.len()).sum();
        println!(
            "{}\n  notes={}  simultaneous-same-key-different-pitch={}  overlapping-hold-pairs={}  total-overlap={:.1}s",
            path.file_name().unwrap_or_default().to_string_lossy(),
            song.notes.len(),
            simultaneous,
            overlap_pairs,
            overlaps_len_ms / 1000.0,
        );
    }
    Ok(())
}
