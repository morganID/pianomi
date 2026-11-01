//! `pianomi-gui` — a macOS **menu bar** app (status item) for
//! `pianomi`.
//!
//! Instead of a normal window it lives in the menu bar (top-right). Click the
//! piano icon to get a dropdown with:
//!   * Open MIDI…            (native file dialog)
//!   * Song / status info
//!   * Play / Stop           (playback on a worker thread, stoppable)
//!   * Dry run toggle
//!   * Humanize / Speed / Transpose / Loops presets
//!   * Output                 (keyboard, virtual MIDI port, or a destination)
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
    m_play: MenuItem,
    m_stop: MenuItem,
    m_dry: CheckMenuItem,
    m_humanize: Vec<(u8, CheckMenuItem)>,
    m_speed: Vec<(f32, CheckMenuItem)>,
    m_transpose: Vec<(i8, CheckMenuItem)>,
    m_loops: Vec<(u32, CheckMenuItem)>,
    m_sharp: Vec<(String, CheckMenuItem)>,
    /// Output routing.
    out: OutChoice,
    m_out_keys: CheckMenuItem,
    m_out_virt: CheckMenuItem,
    /// Live list of MIDI destinations, rebuilt by "Rescan".
    m_out_dests: Vec<(String, CheckMenuItem)>,
    /// Monotonic id counter for destination menu items.
    out_seq: usize,
    sub_out: Submenu,
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
        self.m_play.set_enabled(!playing && self.song.is_some());
        self.m_stop.set_enabled(playing);
    }

    fn pick_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("MIDI files", &["mid", "midi"])
            .pick_file()
        {
            self.load(path);
        }
    }

    fn load(&mut self, path: PathBuf) {
        match midi_parser::parse_midi(&path) {
            Ok(song) => {
                self.file_name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let n = song.notes.len();
                self.song = Some(song);
                self.m_song
                    .set_text(format!("Song: {} ({} notes)", self.file_name, n));
                self.m_play.set_enabled(true);
                self.set_status(&format!("Loaded {}", self.file_name));
            }
            Err(e) => {
                self.set_status(&format!("Parse error: {}", e));
            }
        }
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
                let it =
                    CheckMenuItem::with_id(format!("od{}", self.out_seq), &name, true, checked, None);
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
            "play" => self.start_play(),
            "stop" => {
                self.stop.store(true, Ordering::SeqCst);
                self.set_status("Stopping…");
            }
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
            _ => {
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
    let m_play = MenuItem::with_id("play", "Play", false, None);
    let m_stop = MenuItem::with_id("stop", "Stop", false, None);
    let m_dry = CheckMenuItem::with_id("dry", "Dry run (no keys)", true, false, None);

    let sub_h = Submenu::with_id("sub_h", "Humanize", true);
    let mut m_humanize = Vec::new();
    for v in [0u8, 25, 50, 75, 100] {
        let it = CheckMenuItem::with_id(format!("h{}", v), format!("{}%", v), true, v == 50, None);
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
    sub_out.append(&m_out_virt)?;
    sub_out.append(&PredefinedMenuItem::separator())?;

    // Destinations are discovered once here and refreshed by "Rescan MIDI ports".
    let mut m_out_dests: Vec<(String, CheckMenuItem)> = Vec::new();
    let mut out_seq = 0usize;
    match midi_out::list_destinations() {
        Ok(names) if !names.is_empty() => {
            for name in names {
                out_seq += 1;
                let it =
                    CheckMenuItem::with_id(format!("od{}", out_seq), &name, true, false, None);
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

    let menu = Menu::new();
    menu.append_items(&[
        &title,
        &PredefinedMenuItem::separator(),
        &m_open,
        &m_song,
        &m_status,
        &PredefinedMenuItem::separator(),
        &m_play,
        &m_stop,
        &m_dry,
        &PredefinedMenuItem::separator(),
        &sub_h,
        &sub_s,
        &sub_t,
        &sub_l,
        &sub_sh,
        &sub_out,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(Some("Quit Pianomi")),
    ])?;

    let mut tray_builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(concat!(
            "Pianomi v",
            env!("CARGO_PKG_VERSION")
        ))
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
        humanize: 50,
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
        m_play,
        m_stop,
        m_dry,
        m_humanize,
        m_speed,
        m_transpose,
        m_loops,
        m_sharp,
        out: OutChoice::Keys,
        m_out_keys,
        m_out_virt,
        m_out_dests,
        out_seq,
        sub_out,
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
    }

    event_loop.run(move |_event, _elwt, control_flow| {
        // Poll periodically so menu clicks (delivered via muda's channel) and
        // playback-thread status updates are picked up promptly.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(150));
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            ui.on_menu(ev.id.0.as_str());
        }
        while let Ok(_ev) = TrayIconEvent::receiver().try_recv() {}
        ui.refresh();
    });
}
