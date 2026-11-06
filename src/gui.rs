//! `pianomi-gui` — a macOS **menu bar** app (status item) for
//! `pianomi`.
//!
//! Instead of a normal window it lives in the menu bar (top-right). Click the
//! piano icon to get a dropdown with:
//!   * Open MIDI… (native file dialog)
//!   * Library (play random, or browse a folder of songs)
//!   * Song / status info
//!   * Play / Stop (one toggle; playback runs on a worker thread)
//!   * Dry run toggle
//!   * Settings ▸ (humanize, speed, transpose, loops, sharp keys, output
//!     routing, debug)
//!   * Quit
//!
//! The Dock icon is hidden via `LSUIElement` + the Accessory activation policy,
//! so it behaves like a real menu bar utility.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

use pianomi::config::Config;
use pianomi::humanizer::Humanizer;
use pianomi::key_mapper::KeyMapper;
use pianomi::midi_out;
use pianomi::midi_parser;
use pianomi::player::{OutputMode, Player};
use pianomi::Song;

/// Where the notes are sent, as chosen in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OutChoice {
    /// Simulated keystrokes in the focused window.
    Keys,
    /// Publish a virtual MIDI port (empty spec = default name).
    Virtual,
    /// Connect to a named MIDI destination.
    Destination(String),
}

impl OutChoice {
    fn mode(&self) -> OutputMode {
        match self {
            OutChoice::Keys => OutputMode::Keys,
            _ => OutputMode::Midi,
        }
    }

    /// The value handed to `midi_out::open`; `None` means "keyboard".
    fn spec(&self) -> Option<String> {
        match self {
            OutChoice::Keys => None,
            OutChoice::Virtual => Some(String::new()),
            OutChoice::Destination(name) => Some(name.clone()),
        }
    }
}

/// State shared with the playback worker thread.
struct Shared {
    playing: bool,
    status: String,
}

struct Ui {
    cfg: Config,
    song: Option<Song>,
    file_name: String,
    /// Path of the song in `song`, so "Play random" can avoid repeating it.
    current_path: Option<PathBuf>,

    humanize: u8,
    speed: f32,
    transpose: i8,
    loops: u32,
    dry_run: bool,

    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,

    // Menu item handles (muda items are Arc-backed, so these stay in sync
    // with the copies living inside the tray menu).
    _tray: TrayIcon,
    m_song: MenuItem,
    m_status: MenuItem,
    /// One item that reads "Play" when idle and "Stop" while a song runs.
    m_toggle: MenuItem,
    m_dry: CheckMenuItem,
    m_humanize: Vec<(u8, CheckMenuItem)>,
    m_speed: Vec<(f32, CheckMenuItem)>,
    m_transpose: Vec<(i8, CheckMenuItem)>,
    m_loops: Vec<(u32, CheckMenuItem)>,
    m_sharp: Vec<(String, CheckMenuItem)>,
    /// Every song offered by the Library menu. Item ids are `libs<index>` into
    /// this vector, so the path never has to be encoded in a menu id.
    library: Vec<PathBuf>,
    /// The Library menu itself, so groups can be appended after start-up.
    sub_library: Submenu,
    /// Groups still waiting to be turned into submenus, drained a few per
    /// event-loop tick. See `refresh` for why this is not done up front.
    library_fill: std::vec::IntoIter<(String, Vec<usize>)>,
    /// A song that should start as soon as the previous one has stopped.
    pending_play: Option<PathBuf>,
    /// Output routing.
    out: OutChoice,
    m_out_keys: CheckMenuItem,
    m_out_virt: CheckMenuItem,
    /// Live list of MIDI destinations, rebuilt by "Rescan".
    m_out_dests: Vec<(String, CheckMenuItem)>,
    /// Monotonic id counter for destination menu items.
    out_seq: usize,
    sub_out: Submenu,
    /// Debug ▸ Log playback to file. Writes every scheduled key event plus all
    /// warnings to `~/Library/Logs/Pianomi/trace.log`, because a bundle started
    /// with `open` has stdout on /dev/null.
    m_trace: CheckMenuItem,
    /// Set by a menu action that legitimately wants to keep focus elsewhere
    /// (Reveal Debug Log opens Finder). The event loop skips its focus restore
    /// for that one action.
    suppress_restore: bool,
}

impl Ui {
    fn set_status(&self, msg: &str) {
        if let Ok(mut s) = self.shared.lock() {
            s.status = msg.to_string();
        }
        self.m_status.set_text(msg);
    }

    fn refresh(&mut self) {
        let (playing, status) = {
            let s = self.shared.lock().unwrap();
            (s.playing, s.status.clone())
        };
        self.m_status.set_text(&status);
        // One item, two jobs: the label is the action you can take right now.
        self.m_toggle
            .set_text(if playing { "Stop" } else { "Play" });
        self.m_toggle.set_enabled(playing || self.song.is_some());

        // A library pick that arrived mid-song waits here until the worker
        // reports idle, so "choose another song" never needs two clicks.
        if !playing {
            if let Some(path) = self.pending_play.take() {
                if self.load(path) {
                    self.start_play();
                }
            }
        }

        // Building the whole Library up front costs about a second for a few
        // thousand songs (measured: 973 ms release / 1.46 s debug for 2255
        // items) and would delay the status item appearing. A dozen groups per
        // tick keeps each pass under ~100 ms and the menu is complete within a
        // second of launch.
        for _ in 0..12 {
            let Some((key, indices)) = self.library_fill.next() else {
                break;
            };
            let sub = Submenu::with_id(format!("libg{}", key), group_label(&key), true);
            for i in indices {
                let item = MenuItem::with_id(
                    format!("libs{}", i),
                    song_label(&self.library[i], &key),
                    true,
                    None,
                );
                if let Err(e) = sub.append(&item) {
                    eprintln!("library menu: {e}");
                }
            }
            if let Err(e) = self.sub_library.append(&sub) {
                eprintln!("library menu: {e}");
            }
        }
    }

    fn pick_file(&mut self) {
        // `rfd` runs a synchronous NSOpenPanel, which activates this process.
        // Its FocusManager restores `NSApp.keyWindow`, but we have no windows,
        // so focus would stay stolen. The event loop hands focus back to the
        // previously frontmost app on the next tick (after NSMenu closes).
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("MIDI files", &["mid", "midi"])
            .pick_file()
        {
            self.load(path);
        }
    }

    /// Parse `path` into `self.song`. Returns `false` on a parse error, so a
    /// caller that meant to play it can leave the previous song alone.
    fn load(&mut self, path: PathBuf) -> bool {
        match midi_parser::parse_midi(&path) {
            Ok(song) => {
                self.file_name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let n = song.notes.len();
                self.song = Some(song);
                self.current_path = Some(path);
                self.m_song.set_text(format!(
                    "Song: {} ({} notes)",
                    elide(&self.file_name, 44),
                    n
                ));
                self.m_toggle.set_enabled(true);
                self.set_status(&format!("Loaded {}", self.file_name));
                true
            }
            Err(e) => {
                self.set_status(&format!("Parse error: {}", e));
                false
            }
        }
    }

    /// Load `path` and start it. If a song is still winding down, remember the
    /// request and let `refresh` start it once the worker reports idle.
    fn request_play(&mut self, path: PathBuf) {
        let playing = self.shared.lock().map(|s| s.playing).unwrap_or(false);
        if !self.load(path.clone()) {
            return;
        }
        if playing {
            self.stop.store(true, Ordering::SeqCst);
            self.pending_play = Some(path);
            self.set_status("Stopping, then playing the new song…");
        } else {
            self.start_play();
        }
    }

    /// Pick a song from the library uniformly at random, skipping whatever is
    /// already loaded so the button always visibly does something.
    fn play_random(&mut self) {
        if self.library.is_empty() {
            self.set_status("Library is empty");
            return;
        }
        let mut idx = rand::random::<usize>() % self.library.len();
        if self.library.len() > 1 {
            // Bounded retry rather than a filtered draw: with one file loaded
            // out of N, a few tries is cheaper than building a second vector.
            for _ in 0..8 {
                if Some(&self.library[idx]) != self.current_path.as_ref() {
                    break;
                }
                idx = rand::random::<usize>() % self.library.len();
            }
        }
        self.request_play(self.library[idx].clone());
    }

    fn start_play(&mut self) {
        let song = match &self.song {
            Some(s) => s.clone(),
            None => {
                self.set_status("No song loaded");
                return;
            }
        };
        {
            let mut s = self.shared.lock().unwrap();
            if s.playing {
                return;
            }
            s.playing = true;
            s.status = "Playing…".to_string();
        }

        let mut cfg = self.cfg.clone();
        if self.transpose != 0 {
            cfg.keyboard.transpose = self.transpose;
        }
        let mapper = KeyMapper::from_config(&cfg.keyboard);
        let humanizer = Humanizer::new(self.humanize, self.speed as f64, cfg.humanize.clone());
        let player_cfg = cfg.player.clone();
        let dry = self.dry_run;
        let loops = self.loops;
        let stop = self.stop.clone();
        let shared = self.shared.clone();
        let mode = self.out.mode();
        let spec = self.out.spec();
        stop.store(false, Ordering::SeqCst);

        // Direct-to-Roblox is not a choice any more: keyboard playback always
        // scopes its events to the Roblox process when it is running, and falls
        // back to the focused window otherwise. `targeted` below is the
        // *resolved* truth, because the scheduler must widen its key-up margins
        // only when events really travel through CGEventPostToPid. Getting this
        // wrong either loses notes in Direct mode or needlessly shortens notes
        // on the HID path.
        #[cfg(target_os = "macos")]
        let targeted = {
            use pianomi::keycode;
            if mode == OutputMode::Keys && !dry {
                keycode::set_event_target_roblox(true);
                if keycode::event_target_pid() <= 0 {
                    keycode::set_event_target_roblox(false);
                    keycode::set_event_target_pid(0);
                    self.set_status("Roblox not running — keys go to focused window");
                    false
                } else {
                    true
                }
            } else {
                keycode::set_event_target_roblox(false);
                keycode::set_event_target_pid(0);
                false
            }
        };
        #[cfg(not(target_os = "macos"))]
        let targeted = false;

        thread::spawn(move || {
            // The MIDI port is opened inside the worker: CoreMIDI connections
            // are not Send, so the handle must never leave the thread that made
            // it.
            let result = (|| -> anyhow::Result<()> {
                let midi = match (&spec, dry) {
                    (Some(s), false) => Some(midi_out::open(Some(s))?),
                    _ => None,
                };
                let mut p = Player::new(
                    mapper,
                    humanizer,
                    player_cfg,
                    dry,
                    mode,
                    midi,
                    None,
                    Some(stop),
                )?;
                p.set_targeted(targeted);
                p.play(&song, loops, dry)
            })();
            let mut s = shared.lock().unwrap();
            s.playing = false;
            s.status = match result {
                Ok(_) => "Done".to_string(),
                Err(e) => format!("Error: {}", e),
            };
        });
    }

    /// Re-read the machine's MIDI destinations and rebuild that part of the menu.
    fn rescan_midi(&mut self) {
        for (_, item) in self.m_out_dests.drain(..) {
            let _ = self.sub_out.remove(&item);
        }

        let names = midi_out::list_destinations().unwrap_or_default();
        if names.is_empty() {
            let it = CheckMenuItem::with_id(
                "odnone",
                "(no MIDI destinations found)",
                false,
                false,
                None,
            );
            let _ = self.sub_out.append(&it);
            self.m_out_dests.push((String::new(), it));
        } else {
            for name in names {
                // Ids come from a monotonic counter, so a rescan never reuses an
                // id that a previously removed item already had.
                self.out_seq += 1;
                let checked = matches!(&self.out, OutChoice::Destination(n) if *n == name);
                let it = CheckMenuItem::with_id(
                    format!("od{}", self.out_seq),
                    &name,
                    true,
                    checked,
                    None,
                );
                let _ = self.sub_out.append(&it);
                self.m_out_dests.push((name, it));
            }
        }
        self.sync_out_checks();
        self.set_status(&format!("MIDI destinations: {}", self.m_out_dests.len()));
    }

    /// Make the check marks match `self.out`.
    fn sync_out_checks(&self) {
        self.m_out_keys
            .set_checked(matches!(self.out, OutChoice::Keys));
        self.m_out_virt
            .set_checked(matches!(self.out, OutChoice::Virtual));
        for (name, item) in &self.m_out_dests {
            let on = matches!(&self.out, OutChoice::Destination(n) if n == name);
            item.set_checked(on);
        }
    }

    fn on_menu(&mut self, id: &str) {
        match id {
            "open" => self.pick_file(),
            "toggle" => {
                let playing = self.shared.lock().map(|s| s.playing).unwrap_or(false);
                if playing {
                    self.stop.store(true, Ordering::SeqCst);
                    self.set_status("Stopping…");
                } else {
                    self.start_play();
                }
            }
            "librnd" => self.play_random(),
            "dry" => {
                self.dry_run = self.m_dry.is_checked();
            }
            "smchar" | "smkey" => {
                let mode = if id == "smkey" { "key" } else { "char" };
                self.cfg.player.shift_mode = mode.to_string();
                for (val, it) in &self.m_sharp {
                    it.set_checked(val == mode);
                }
                self.set_status(&format!("Sharp keys: {}", mode));
            }
            "outkeys" => {
                self.out = OutChoice::Keys;
                self.sync_out_checks();
                self.set_status("Output: keyboard");
            }
            "outvirt" => {
                self.out = OutChoice::Virtual;
                self.sync_out_checks();
                self.set_status(&format!(
                    "Output: virtual MIDI port \"{}\"",
                    midi_out::DEFAULT_VIRTUAL_NAME
                ));
            }
            "outrescan" => self.rescan_midi(),
            "dbgtrace" => {
                let on = self.m_trace.is_checked();
                pianomi::trace::set_enabled(on);
                self.set_status(if on {
                    "Debug logging ON — written on the next playback"
                } else {
                    "Debug logging off"
                });
            }
            "dbgreveal" => {
                let path = pianomi::trace::log_path();
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                // `open -R` needs the file to exist, and creating it here also
                // gives the user an empty log to inspect before playing.
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path);
                match std::process::Command::new("open")
                    .arg("-R")
                    .arg(&path)
                    .spawn()
                {
                    Ok(_) => {
                        // Finder is *supposed* to come forward here; skip the
                        // focus hand-back for this one action.
                        self.suppress_restore = true;
                        self.set_status(&format!("Revealed {}", path.display()));
                    }
                    Err(e) => self.set_status(&format!("Could not reveal log: {e}")),
                }
            }
            _ => {
                // A song picked from the Library menu: the id is the index into
                // `library`, so the path never has to survive a round trip
                // through a menu id.
                if let Some(v) = id.strip_prefix("libs") {
                    if let Ok(i) = v.parse::<usize>() {
                        if let Some(path) = self.library.get(i).cloned() {
                            self.request_play(path);
                        }
                    }
                    return;
                }
                // A MIDI destination picked from the rescan list. Looked up by
                // the item's own id, so a rescan can renumber freely.
                if id.starts_with("od") {
                    let picked = self
                        .m_out_dests
                        .iter()
                        .find(|(_, it)| it.id().0 == id)
                        .map(|(name, _)| name.clone());
                    if let Some(name) = picked {
                        if !name.is_empty() {
                            self.out = OutChoice::Destination(name.clone());
                            self.sync_out_checks();
                            self.set_status(&format!("Output: MIDI \"{}\"", name));
                        }
                    }
                    return;
                }
                if let Some(v) = id.strip_prefix("h") {
                    if let Ok(n) = v.parse::<u8>() {
                        self.humanize = n;
                        for (val, it) in &self.m_humanize {
                            it.set_checked(*val == n);
                        }
                    }
                } else if let Some(v) = id.strip_prefix("sp") {
                    if let Ok(n) = v.parse::<u32>() {
                        let f = n as f32 / 100.0;
                        self.speed = f;
                        for (val, it) in &self.m_speed {
                            it.set_checked((*val - f).abs() < 1e-4);
                        }
                    }
                } else if let Some(v) = id.strip_prefix("tr") {
                    if let Ok(n) = v.parse::<i8>() {
                        self.transpose = n;
                        for (val, it) in &self.m_transpose {
                            it.set_checked(*val == n);
                        }
                    }
                } else if let Some(v) = id.strip_prefix("lp") {
                    if let Ok(n) = v.parse::<u32>() {
                        self.loops = n;
                        for (val, it) in &self.m_loops {
                            it.set_checked(*val == n);
                        }
                    }
                }
            }
        }
    }
}

/// Distance from a point to a line segment (used to draw the flag stroke).
fn seg_dist(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        ((px - x1) * dx + (py - y1) * dy) / len2
    };
    let t = t.clamp(0.0, 1.0);
    let qx = x1 + t * dx;
    let qy = y1 + t * dy;
    ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
}

/// Whether a point falls inside the eighth-note glyph (head + stem + flag).
fn note_inside(px: f64, py: f64) -> bool {
    // Note head: rotated filled ellipse.
    let (cx, cy) = (7.6, 15.4);
    let ang: f64 = -0.35;
    let (rx, ry) = (3.9, 3.0);
    let dx = px - cx;
    let dy = py - cy;
    let (s, c) = (ang.sin(), ang.cos());
    let ex = dx * c + dy * s;
    let ey = -dx * s + dy * c;
    if (ex / rx).powi(2) + (ey / ry).powi(2) <= 1.0 {
        return true;
    }
    // Stem.
    if (10.5..=11.7).contains(&px) && (3.4..=15.6).contains(&py) {
        return true;
    }
    // Flag: thick curved stroke.
    let pts = [(11.1, 3.4), (13.6, 4.3), (15.2, 6.7), (14.3, 9.4)];
    for w in pts.windows(2) {
        if seg_dist(px, py, w[0].0, w[0].1, w[1].0, w[1].1) <= 0.85 {
            return true;
        }
    }
    false
}

/// Shorten `s` to at most `max` characters, appending an ellipsis when it had
/// to be cut. Used for menu labels: macOS clips long items anyway, and a
/// clipped label loses the part that identifies the file.
fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut)
}

/// Every `.mid` / `.midi` file directly inside `dir`, sorted by filename.
/// A missing or unreadable folder yields an empty list rather than an error:
/// the Library menu is a convenience, not a requirement.
fn scan_library(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "mid" || ext == "midi" {
            out.push(path);
        }
    }
    out.sort_by(|a, b| {
        a.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .cmp(
                &b.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase(),
            )
    });
    out
}

/// Grouping key for a song: the first `_`-separated word of its filename.
/// The library is named `<work or composer>_<rest>`, so this is what makes
/// "chopin_ballade_1" land under Chopin.
fn group_key(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    stem.split('_').next().unwrap_or(&stem).to_ascii_lowercase()
}

/// Menu label for a group key: just the first letter capitalised.
fn group_label(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::from("?"),
    }
}

/// Menu label for a song: filename without extension, underscores as spaces.
/// The group name is stripped when the filename repeats it, because the
/// submenu heading already says it ("Chopin ▸ ballade 3", not
/// "Chopin ▸ chopin ballade 3").
fn song_label(path: &std::path::Path, group: &str) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut title = stem.replace('_', " ");
    let prefix = format!("{} ", group.trim().to_lowercase());
    if let Some(head) = title.get(..prefix.len()) {
        if head.eq_ignore_ascii_case(&prefix) {
            title = title[prefix.len()..].to_string();
        }
    }
    elide(&title, 58)
}

/// Group used for songs that are not in the curated list's composer columns —
/// i.e. the ones the user added themselves. Always sorted first.
const MINE_GROUP: &str = "Mine";

/// Read a curated `<Group>\t<filename>` list.
///
/// Returns `None` when the file is missing, so a typo in the config falls back
/// to scanning the folder rather than silently showing an empty menu. Lines
/// that are blank, start with `#`, or name a file that no longer exists are
/// skipped.
fn read_allowlist(list: &std::path::Path, dir: &std::path::Path) -> Option<Vec<(String, PathBuf)>> {
    let text = std::fs::read_to_string(list).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        // Tabs, not spaces: filenames are full of spaces.
        let Some((group, name)) = line.split_once('\t') else {
            continue;
        };
        let group = group.trim();
        let name = name.trim();
        if group.is_empty() || name.is_empty() {
            continue;
        }
        let path = dir.join(name);
        if path.is_file() {
            out.push((group.to_string(), path));
        }
    }
    Some(out)
}

/// Build a native-style (template) music-note icon for the menu bar.
/// Rendered black with anti-aliased alpha; macOS recolors it automatically
/// when `with_icon_as_template(true)` is set.
fn make_icon() -> Icon {
    let size = 22u32;
    let ss = 4u32; // supersampling factor per axis
    let mut buf = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0u32;
            for sy in 0..ss {
                for sx in 0..ss {
                    let px = x as f64 + (sx as f64 + 0.5) / ss as f64;
                    let py = y as f64 + (sy as f64 + 0.5) / ss as f64;
                    if note_inside(px, py) {
                        hits += 1;
                    }
                }
            }
            let cov = hits as f64 / (ss * ss) as f64;
            let a = (cov * 255.0).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            buf[i] = 0;
            buf[i + 1] = 0;
            buf[i + 2] = 0;
            buf[i + 3] = a;
        }
    }
    Icon::from_rgba(buf, size, size).expect("valid icon")
}

fn main() -> anyhow::Result<()> {
    // `pianomi-gui --version` / `-V` prints the version and exits.
    // Used by build-app.sh to confirm the freshly built binary landed.
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && (args[1] == "--version" || args[1] == "-V") {
        println!("pianomi-gui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // `--check-perm` reports whether *this bundle* is trusted for Accessibility.
    // TCC attributes the question to the responsible process, so the answer is
    // only meaningful when the app is launched as itself (`open -n ... --args
    // --check-perm`) rather than from a terminal. The verdict is written to a
    // file because `open` discards the app's stdout.
    if args.len() > 1 && args[1] == "--check-perm" {
        let ok = pianomi::perm::accessibility_trusted();
        let path = std::env::temp_dir().join("pianomi-perm.txt");
        let _ = std::fs::write(&path, if ok { "trusted" } else { "denied" });
        println!("accessibility: {}", if ok { "trusted" } else { "denied" });
        return Ok(());
    }

    // Debug logging is opt-in via PIANOMI_TRACE=1 (or the Debug menu once the
    // app is running). The menu starts checked if the env var asked for it.
    pianomi::trace::init_from_env();

    // A one-line banner so it is obvious which build is running: the crash fix
    // changed how key events are dispatched, and an old binary looks identical
    // from the outside. If this line is missing, you are on a stale build.
    eprintln!(
        "pianomi-gui {} — key dispatch: {}",
        env!("CARGO_PKG_VERSION"),
        pianomi::keycode::dispatch_mode()
    );

    // Ask for Accessibility up front: without it every keystroke is silently
    // dropped, and the menu would only ever say "Error". The prompt also
    // registers the app in System Settings so it is one click to enable.
    if !pianomi::perm::accessibility_trusted() {
        eprintln!("Accessibility not granted — requesting the system prompt…");
        pianomi::perm::request_accessibility_prompt();
    }

    let cfg = Config::load(None)?;

    // --- menu construction ------------------------------------------------
    let title = MenuItem::with_id("title", "Pianomi", false, None);
    let m_open = MenuItem::with_id("open", "Open MIDI…", true, None);
    let m_song = MenuItem::with_id("song", "Song: (none)", false, None);
    let m_status = MenuItem::with_id("status", "Ready", false, None);
    let m_toggle = MenuItem::with_id("toggle", "Play", false, None);
    let m_dry = CheckMenuItem::with_id("dry", "Dry run (no keys)", true, false, None);

    // --- Library ----------------------------------------------------------
    // Two sources. A curated `<Group>\t<filename>` list wins, because the
    // Kunstderfuge dump is thousands of files of mostly obscure repertoire and
    // its filenames start with the *work* (`preludes_…`, `sonata_…`), so a raw
    // scan can only group by that first word. Without a list we fall back to
    // scanning the folder and grouping by filename prefix.
    let library_dir = cfg.library.resolved_dir();
    let curated = cfg
        .library
        .resolved_allowlist()
        .and_then(|list| read_allowlist(&list, &library_dir));

    let (library, library_fill) = match curated {
        Some(entries) => {
            let mut library = Vec::new();
            let mut by_group: std::collections::BTreeMap<String, Vec<usize>> =
                std::collections::BTreeMap::new();
            for (group, path) in entries {
                let i = library.len();
                library.push(path);
                by_group.entry(group).or_default().push(i);
            }
            let mut fill: Vec<(String, Vec<usize>)> = by_group.into_iter().collect();
            // "Mine" first: the songs you added yourself are the ones you are
            // most likely to want, and they are the smallest group.
            fill.sort_by_key(|(g, _)| (g != MINE_GROUP, g.clone()));
            (library, fill)
        }
        None => {
            let library = scan_library(&library_dir);
            let mut by_group: std::collections::BTreeMap<String, Vec<usize>> =
                std::collections::BTreeMap::new();
            for (i, path) in library.iter().enumerate() {
                by_group.entry(group_key(path)).or_default().push(i);
            }
            (library, by_group.into_iter().collect())
        }
    };

    let sub_library = Submenu::with_id("sub_library", "Library", true);
    let m_random = MenuItem::with_id("librnd", "Play random", !library.is_empty(), None);
    sub_library.append(&m_random)?;
    sub_library.append(&PredefinedMenuItem::separator())?;
    if library.is_empty() {
        sub_library.append(&MenuItem::with_id(
            "libnone",
            format!(
                "(no .mid files in {})",
                elide(&library_dir.display().to_string(), 48)
            ),
            false,
            None,
        ))?;
    }

    let sub_h = Submenu::with_id("sub_h", "Humanize", true);
    let mut m_humanize = Vec::new();
    for v in [0u8, 25, 50, 75, 100] {
        let it = CheckMenuItem::with_id(format!("h{}", v), format!("{}%", v), true, v == 0, None);
        sub_h.append(&it)?;
        m_humanize.push((v, it));
    }

    let sub_s = Submenu::with_id("sub_s", "Speed", true);
    let mut m_speed = Vec::new();
    for v in [0.5f32, 0.75, 1.0, 1.25, 1.5, 2.0] {
        let enc = (v * 100.0).round() as u32;
        let it = CheckMenuItem::with_id(
            format!("sp{}", enc),
            format!("{:.2}x", v),
            true,
            (v - 1.0).abs() < 1e-4,
            None,
        );
        sub_s.append(&it)?;
        m_speed.push((v, it));
    }

    let sub_t = Submenu::with_id("sub_t", "Transpose", true);
    let mut m_transpose = Vec::new();
    for v in [-24i8, -12, 0, 12, 24] {
        let label = if v == 0 {
            "0 (original)".to_string()
        } else {
            format!("{:+}", v)
        };
        let it = CheckMenuItem::with_id(format!("tr{}", v), label, true, v == 0, None);
        sub_t.append(&it)?;
        m_transpose.push((v, it));
    }

    let sub_l = Submenu::with_id("sub_l", "Loops", true);
    let mut m_loops = Vec::new();
    for v in [1u32, 2, 3, 5] {
        let it = CheckMenuItem::with_id(format!("lp{}", v), format!("{}x", v), true, v == 1, None);
        sub_l.append(&it)?;
        m_loops.push((v, it));
    }

    let sub_sh = Submenu::with_id("sub_sh", "Sharp keys", true);
    let mut m_sharp = Vec::new();
    let current_sharp = cfg.player.shift_mode.clone();
    for (val, label) in [
        ("key", "Real Shift key (Roblox)"),
        ("char", "Character only (no Shift)"),
    ] {
        let checked = val == current_sharp;
        let it = CheckMenuItem::with_id(format!("sm{}", val), label, true, checked, None);
        sub_sh.append(&it)?;
        m_sharp.push((val.to_string(), it));
    }

    // Output routing. Keyboard playback always targets the Roblox process (see
    // `start_play`), so there is nothing to toggle here.
    let sub_out = Submenu::with_id("sub_out", "Output", true);
    let m_out_keys = CheckMenuItem::with_id("outkeys", "Keyboard (Roblox piano)", true, true, None);
    let m_out_virt = CheckMenuItem::with_id(
        "outvirt",
        format!("Virtual MIDI port \"{}\"", midi_out::DEFAULT_VIRTUAL_NAME),
        true,
        false,
        None,
    );
    sub_out.append(&m_out_keys)?;
    sub_out.append(&PredefinedMenuItem::separator())?;
    sub_out.append(&m_out_virt)?;
    sub_out.append(&PredefinedMenuItem::separator())?;

    // Destinations are discovered once here and refreshed by "Rescan MIDI ports".
    let mut m_out_dests: Vec<(String, CheckMenuItem)> = Vec::new();
    let mut out_seq = 0usize;
    match midi_out::list_destinations() {
        Ok(names) if !names.is_empty() => {
            for name in names {
                out_seq += 1;
                let it = CheckMenuItem::with_id(format!("od{}", out_seq), &name, true, false, None);
                sub_out.append(&it)?;
                m_out_dests.push((name, it));
            }
        }
        _ => {
            let it = CheckMenuItem::with_id(
                "odnone",
                "(no MIDI destinations found)",
                false,
                false,
                None,
            );
            sub_out.append(&it)?;
            m_out_dests.push((String::new(), it));
        }
    }
    sub_out.append(&PredefinedMenuItem::separator())?;
    sub_out.append(&MenuItem::with_id(
        "outrescan",
        "Rescan MIDI ports",
        true,
        None,
    ))?;

    // Debug lives in its own submenu so the everyday menu stays short.
    let sub_dbg = Submenu::with_id("sub_dbg", "Debug", true);
    let m_trace = CheckMenuItem::with_id(
        "dbgtrace",
        "Log playback to file",
        true,
        pianomi::trace::enabled(),
        None,
    );
    let m_reveal = MenuItem::with_id("dbgreveal", "Reveal Debug Log", true, None);
    sub_dbg.append(&m_trace)?;
    sub_dbg.append(&m_reveal)?;

    // Everything that shapes playback or routing lives one level down, so the
    // menu itself stays six items tall: open, library, play, dry run, settings,
    // quit. Inside Settings the groups are: how notes are shaped, where they
    // go, and debugging.
    let sub_settings = Submenu::with_id("sub_settings", "Settings", true);
    sub_settings.append_items(&[
        &sub_h,
        &sub_s,
        &sub_t,
        &sub_l,
        &PredefinedMenuItem::separator(),
        &sub_sh,
        &sub_out,
        &PredefinedMenuItem::separator(),
        &sub_dbg,
    ])?;

    let menu = Menu::new();
    menu.append_items(&[
        &title,
        &PredefinedMenuItem::separator(),
        &m_open,
        &sub_library,
        &m_song,
        &m_status,
        &PredefinedMenuItem::separator(),
        &m_toggle,
        &m_dry,
        &PredefinedMenuItem::separator(),
        &sub_settings,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(Some("Quit Pianomi")),
    ])?;

    let mut tray_builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(concat!("Pianomi v", env!("CARGO_PKG_VERSION")))
        .with_icon(make_icon());
    // Render as a macOS template image so it adapts to the menu bar theme.
    #[cfg(target_os = "macos")]
    {
        tray_builder = tray_builder.with_icon_as_template(true);
    }
    let tray = tray_builder.build()?;

    let mut ui = Ui {
        cfg,
        song: None,
        file_name: String::new(),
        current_path: None,
        // 0 = play the file exactly as written. Humanization is opt-in: the
        // default must not silently alter timing/velocity before the user has
        // even heard the song once.
        humanize: 0,
        speed: 1.0,
        transpose: 0,
        loops: 1,
        dry_run: false,
        stop: Arc::new(AtomicBool::new(false)),
        shared: Arc::new(Mutex::new(Shared {
            playing: false,
            status: "Ready".to_string(),
        })),
        _tray: tray,
        m_song,
        m_status,
        m_toggle,
        m_dry,
        m_humanize,
        m_speed,
        m_transpose,
        m_loops,
        m_sharp,
        library,
        sub_library,
        library_fill: library_fill.into_iter(),
        pending_play: None,
        out: OutChoice::Keys,
        m_out_keys,
        m_out_virt,
        m_out_dests,
        out_seq,
        sub_out,
        m_trace,
        suppress_restore: false,
    };

    // Optional: `pianomi-gui <file.mid>` preloads a song.
    if let Some(p) = std::env::args().nth(1).map(PathBuf::from) {
        ui.load(p);
    }
    ui.refresh();

    // --- event loop -------------------------------------------------------
    let mut event_loop = EventLoopBuilder::new().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        // tao calls `activateIgnoringOtherApps:YES` in
        // applicationDidFinishLaunching by default. For a menu bar utility that
        // means it rips focus away from Roblox the moment it starts. Turning it
        // off is the primary fix for "the GUI steals focus".
        event_loop.set_activate_ignoring_other_apps(false);
    }

    // Remember who owned the frontmost slot before our UI appeared. A status
    // item click and rfd's NSOpenPanel can still activate us, so after every
    // menu action we hand focus back on the NEXT tick — AppKit ignores
    // activation requests while NSMenu is still tracking and would close the
    // menu if we did it immediately.
    let self_pid = std::process::id() as i32;
    let mut last_external_pid = {
        let p = pianomi::focus::frontmost_pid();
        if p > 0 && p != self_pid {
            p
        } else {
            0
        }
    };
    // Also covers the launch activation itself, on the very first tick.
    let mut restore_pending = true;

    event_loop.run(move |_event, _elwt, control_flow| {
        // Poll periodically so menu clicks (delivered via muda's channel) and
        // playback-thread status updates are picked up promptly.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(150));

        let front = pianomi::focus::frontmost_pid();
        if front > 0 && front != self_pid {
            last_external_pid = front;
        }
        if restore_pending {
            restore_pending = false;
            if front == self_pid && last_external_pid > 0 {
                pianomi::focus::activate_pid(last_external_pid);
            }
        }

        let mut handled = false;
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            ui.on_menu(ev.id.0.as_str());
            handled = true;
        }
        while let Ok(_ev) = TrayIconEvent::receiver().try_recv() {}
        ui.refresh();
        // An action that deliberately hands focus somewhere else (Reveal Debug
        // Log) must not be undone by the generic restore on the next tick.
        let suppress = std::mem::take(&mut ui.suppress_restore);
        if handled && !suppress {
            restore_pending = true;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn elide_leaves_short_strings_alone() {
        assert_eq!(elide("abc", 10), "abc");
        assert_eq!(elide("abcdefghij", 10), "abcdefghij");
    }

    #[test]
    fn elide_cuts_to_the_limit_and_marks_it() {
        let out = elide("abcdefghijklmno", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'), "got {out:?}");
    }

    #[test]
    fn group_key_is_the_first_underscore_word() {
        assert_eq!(group_key(Path::new("/x/chopin_ballade_1.mid")), "chopin");
        assert_eq!(group_key(Path::new("/x/Nocturne_2.MID")), "nocturne");
        // No underscore at all: the whole stem is the key.
        assert_eq!(group_key(Path::new("/x/Zambra.mid")), "zambra");
    }

    #[test]
    fn group_label_capitalises_only_the_first_letter() {
        assert_eq!(group_label("chopin"), "Chopin");
        assert_eq!(group_label("abba - the winner"), "Abba - the winner");
        assert_eq!(group_label(""), "?");
    }

    #[test]
    fn song_label_reads_like_a_title() {
        assert_eq!(
            song_label(
                Path::new("/x/dussek_sonata_45_2_1_(c)iscenko.mid"),
                "Dussek"
            ),
            "sonata 45 2 1 (c)iscenko"
        );
        // The group is only stripped when the filename actually repeats it.
        assert_eq!(
            song_label(Path::new("/x/ABBA - The Winner.mid"), "Mine"),
            "ABBA - The Winner"
        );
    }

    #[test]
    fn allowlist_parses_groups_and_skips_junk() {
        let dir = std::env::temp_dir().join(format!("pianomi-allow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a_song.mid"), b"x").unwrap();
        std::fs::write(dir.join("b song.mid"), b"x").unwrap();

        let list = dir.join("list.txt");
        std::fs::write(
            &list,
            "# comment\n\nChopin\ta_song.mid\nMine\tb song.mid\nChopin\tmissing.mid\nbogus line\n",
        )
        .unwrap();

        let got = read_allowlist(&list, &dir).unwrap();
        let flat: Vec<(String, String)> = got
            .iter()
            .map(|(g, p)| {
                (
                    g.clone(),
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            flat,
            vec![
                ("Chopin".to_string(), "a_song.mid".to_string()),
                // A filename with spaces must survive, and a listed-but-missing
                // file must be dropped rather than shown as a dead menu item.
                ("Mine".to_string(), "b song.mid".to_string()),
            ]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_allowlist_falls_back_to_scanning() {
        let dir = std::env::temp_dir().join("pianomi-no-such-list-xyz");
        assert!(read_allowlist(&dir.join("nope.txt"), &dir).is_none());
    }

    #[test]
    fn scan_library_takes_only_midi_files() {
        let dir = std::env::temp_dir().join(format!("pianomi-lib-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["b_song.mid", "a_song.MIDI", "notes.txt", "cover.png"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        // A directory named like a MIDI file must not be listed.
        std::fs::create_dir_all(dir.join("c_trap.mid")).unwrap();

        let found: Vec<String> = scan_library(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(found, vec!["a_song.MIDI", "b_song.mid"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_library_tolerates_a_missing_folder() {
        let missing = std::env::temp_dir().join("pianomi-does-not-exist-xyz");
        assert!(scan_library(&missing).is_empty());
    }
}
