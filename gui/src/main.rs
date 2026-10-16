//! lghub-lite-gui: a small skinnable battery widget + tray front end over the lghub-lite library.
//! Not affiliated with or endorsed by Logitech.
//!
//! Threads: the UI thread runs eframe; one worker thread owns the `watch::Monitor` and does all
//! HID I/O (requests can block for seconds while a wireless device wakes up), so the UI never
//! blocks. Commands go UI -> worker over a channel plus `hid::Waker`; updates come back over a
//! channel plus `request_repaint`. While the window is hidden nothing is repainted and the
//! worker sleeps in the same WaitForMultipleObjects as `lghub-lite watch`.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod demo;
mod fonts;
mod glow;
mod history;
mod icons;
mod os;
mod skin;
mod tray;
mod view;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use history::History;
use lghub_lite::device::{Event, MODE_ONBOARD};
use lghub_lite::hid::Waker;
use lghub_lite::watch::{Cmd, Monitor, Snapshot, Update};
use lghub_lite::{autostart, battery, config, hidpp, platform};
use skin::{Settings, Skin};

const TITLE: &str = "lghub-lite";
/// Messages under the controls disappear after this long.
const MESSAGE_TIME: Duration = Duration::from_secs(8);

enum ToWorker {
    Exec(String, Cmd),
    ApplyPresets(bool),
}

enum FromWorker {
    Update(Update),
    Snapshot(String, Result<Snapshot, String>),
    Fatal(String),
}

type IconState = (Option<icons::Level>, Option<icons::Level>, Option<icons::Level>);

struct Row {
    sel: String,
    name: String,
    battery: Option<String>,
    percent: Option<u8>,
    online: bool,
    protocol: Option<(u8, u8)>,
    history: History,
    /// The low-battery alert fired and the level has not risen above the threshold since.
    alerted: bool,
    /// Demo mode: fixed (time left, drain, status) texts.
    demo: Option<[&'static str; 3]>,
}

/// The charge state word of a battery line like `95% level=full discharging external=none`:
/// "discharging", "charging" (also "slow charging"), "full" or "error". Matched as a whole word,
/// so `level=full` (the level range) is not taken for a full charge.
fn charge_state(battery: &str) -> Option<&'static str> {
    battery.split_whitespace().find_map(|w| match w {
        "discharging" => Some("Discharging"),
        "charging" => Some("Charging"),
        "full" => Some("Full"),
        "error" => Some("Charge error"),
        _ => None,
    })
}

impl Row {
    /// On external power: charging or charged.
    fn charging(&self) -> bool {
        self.battery
            .as_deref()
            .and_then(charge_state)
            .is_some_and(|s| s == "Charging" || s == "Full")
    }
}

/// Flags set by tray callbacks (which run outside `logic`) and consumed in `logic`.
#[derive(Default)]
struct TrayFlags {
    show: AtomicBool,
    skins: AtomicBool,
    quit: AtomicBool,
}

struct App {
    tx: Sender<ToWorker>,
    rx: Receiver<FromWorker>,
    waker: Arc<Waker>,
    rows: Vec<Row>,
    selected: Option<String>,
    snap: Option<Snapshot>,
    pending: usize,
    dpi_input: String,
    /// (is_error, text, when)
    message: Option<(bool, String, Instant)>,
    fatal: Option<String>,
    tray: Option<tray::Tray>,
    tray_tip: String,
    /// What the window and tray icons show now: (lowest, first device, second device).
    icon_state: Option<IconState>,
    /// Our window, for setting its icons directly (0 if unknown).
    hwnd: isize,
    flags: Arc<TrayFlags>,
    quitting: bool,
    autostart_gui: bool,
    bg_running: bool,
    /// Folder of the preset file: battery curves, history, skins and app settings live here.
    dir: PathBuf,
    /// For a voltage-only device on the estimate: (model, G HUB curve file if found).
    ghub_curve: Option<(u16, Option<PathBuf>)>,
    /// After a DPI/report-rate change succeeds, save the values as the model's preset.
    save_preset: bool,
    skins: Vec<Skin>,
    settings: Settings,
    drawer: bool,
    show_skins: bool,
    skin_msg: Option<(bool, String)>,
    /// A custom skin was edited; saved once editing pauses.
    skin_dirty: Option<Instant>,
    /// `--demo`: the design's sample devices, no hardware access.
    demo: Option<Vec<Snapshot>>,
    /// Height of the skin manager panel at the last frame.
    manager_h: f32,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let start_hidden = args.iter().any(|a| a == "--tray");
    let cfg = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .or_else(config::default_path)
        .unwrap_or_else(|| PathBuf::from("presets.txt"));

    let demo = demo::Options::parse(&args);
    // One app per session; the lock is held until the process exits.
    let _instance = match demo {
        Some(_) => None,
        None => match platform::instance_lock("gui") {
            Some(lock) => Some(lock),
            None => {
                os::focus_existing(TITLE);
                return;
            }
        },
    };
    let bg_running = demo.is_none() && platform::instance_running("bg");

    let big = os::icon_sizes().1;
    let (w, h, rgba) = (big, big, icons::app(big, None));
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if demo.is_some() { "lghub-lite demo" } else { TITLE })
            .with_app_id("lghub-lite") // matches packaging/linux/lghub-lite.desktop
            .with_inner_size([view::WIDGET_W, 420.0])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_drag_and_drop(true)
            .with_icon(egui::IconData {
                rgba,
                width: w,
                height: h,
            })
            .with_visible(!start_hidden),
        ..Default::default()
    };
    let _ = eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc, cfg, bg_running, demo.is_some());
            if let Some(d) = demo {
                d.apply(&mut app);
            }
            Ok(Box::new(app))
        }),
    );
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: PathBuf, bg_running: bool, demo: bool) -> App {
        let ctx = cc.egui_ctx.clone();
        fonts::install(&ctx);
        let mut hwnd = 0isize;
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = cc.window_handle()
                && let RawWindowHandle::Win32(w) = h.as_raw()
            {
                hwnd = w.hwnd.get();
                os::full_client_area(hwnd as _);
            }
        }
        // demo mode keeps its settings and skins away from the real ones
        let dir = if demo {
            std::env::temp_dir().join("lghub-lite-demo")
        } else {
            cfg.parent().map(PathBuf::from).unwrap_or_default()
        };
        let (tx, worker_rx) = channel();
        let (worker_tx, rx) = channel();
        let waker = Arc::new(Waker::new().expect("cannot create event"));
        if !demo {
            let (waker, ctx, cfg) = (waker.clone(), ctx.clone(), cfg.clone());
            std::thread::spawn(move || worker(cfg, worker_rx, worker_tx, waker, ctx, !bg_running));
        }
        let flags = Arc::new(TrayFlags::default());
        if !demo {
            let (flags, ctx) = (flags.clone(), ctx.clone());
            os::serve_show_requests(Box::new(move || {
                flags.show.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }));
        }
        let tray = tray::build(&ctx, flags.clone());
        let autostart_gui = autostart::status()
            .ok()
            .flatten()
            .is_some_and(|c| c.contains("lghub-lite-gui"));
        let mut skins = skin::builtins();
        skins.extend(skin::load_custom(&dir.join("skins")));
        let settings = Settings::load(&dir.join("app.txt"));
        App {
            tx,
            rx,
            waker,
            rows: Vec::new(),
            selected: None,
            snap: None,
            pending: 0,
            dpi_input: String::new(),
            message: None,
            fatal: None,
            tray,
            tray_tip: String::new(),
            icon_state: None,
            hwnd,
            flags,
            quitting: false,
            autostart_gui,
            bg_running,
            dir,
            ghub_curve: None,
            save_preset: false,
            skins,
            settings,
            drawer: false,
            show_skins: false,
            skin_msg: None,
            skin_dirty: None,
            demo: None,
            manager_h: 720.0,
        }
    }

    fn skin(&self) -> &Skin {
        self.skins
            .iter()
            .find(|s| s.id == self.settings.skin)
            .unwrap_or(&self.skins[0])
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.dir.join("app.txt")) {
            self.say(true, format!("settings not saved: {e}"));
        }
    }

    fn say(&mut self, err: bool, text: String) {
        self.message = Some((err, text, Instant::now()));
    }

    /// Alert level for a device: saved, else 15 % for voltage-only headsets and 20 % otherwise.
    fn alert_level(&self, sel: &str) -> u8 {
        self.settings.alert(sel).unwrap_or_else(|| {
            let voltage_only = self
                .snap
                .as_ref()
                .is_some_and(|s| s.sel == sel && s.voltage_mv.is_some())
                || self
                    .rows
                    .iter()
                    .any(|r| r.sel == sel && r.battery.as_deref().is_some_and(|b| b.contains(" mV")));
            if voltage_only { 15 } else { 20 }
        })
    }

    fn send(&mut self, sel: &str, cmd: Cmd) {
        if let Some(snaps) = &self.demo {
            self.snap = snaps.iter().find(|s| s.sel == sel).cloned();
            return;
        }
        self.pending += 1;
        let _ = self.tx.send(ToWorker::Exec(sel.to_string(), cmd));
        self.waker.wake();
    }

    fn refresh_selected(&mut self) {
        if let Some(sel) = self.selected.clone() {
            self.send(&sel, Cmd::Refresh);
        }
    }

    fn select(&mut self, sel: String) {
        if self.selected.as_deref() != Some(sel.as_str()) {
            self.selected = Some(sel);
            self.snap = None;
            self.message = None;
            self.refresh_selected();
        }
    }

    /// New battery reading for a row: remember it, add it to the history, check the alert.
    fn set_battery(&mut self, sel: &str, text: Option<String>, percent: Option<u8>) {
        let level = self.alert_level(sel);
        let Some(r) = self.rows.iter_mut().find(|r| r.sel == sel) else {
            return;
        };
        r.battery = text;
        r.percent = percent;
        let Some(p) = percent else { return };
        r.history.add(history::now(), p);
        if p > level {
            r.alerted = false;
        } else if !r.alerted && !r.charging() {
            r.alerted = true;
            let msg = format!("{} battery low: {p}%", r.name);
            self.say(true, msg);
            self.flags.show.store(true, Ordering::Relaxed); // bring the widget up once
        }
    }

    fn on_update(&mut self, u: Update) {
        let mut refresh = false;
        match u {
            Update::Added {
                sel,
                name,
                protocol,
                battery,
                battery_percent,
            } => {
                refresh = self.selected.as_deref() == Some(sel.as_str());
                match self.rows.iter_mut().find(|r| r.sel == sel) {
                    Some(r) => {
                        r.name = name;
                        r.online = true;
                        r.protocol = Some(protocol);
                    }
                    None => {
                        let history = History::load(&self.dir, &sel);
                        self.rows.push(Row {
                            sel: sel.clone(),
                            name,
                            battery: None,
                            percent: None,
                            online: true,
                            protocol: Some(protocol),
                            history,
                            alerted: false,
                            demo: None,
                        })
                    }
                }
                self.set_battery(&sel, battery, battery_percent);
            }
            Update::Offline { sel, .. } => {
                if let Some(r) = self.rows.iter_mut().find(|r| r.sel == sel) {
                    r.online = false;
                }
            }
            Update::Removed { sels, .. } => {
                self.rows.retain(|r| !sels.contains(&r.sel));
                if self.selected.as_ref().is_some_and(|s| sels.contains(s)) {
                    self.selected = None;
                    self.snap = None;
                }
            }
            Update::Battery { sel, text, percent } => {
                refresh = self.selected.as_deref() == Some(sel.as_str());
                self.set_battery(&sel, Some(text), percent);
            }
            Update::Event { sel, event } => {
                match &event {
                    Event::Connection { online: false, .. } => {
                        if let Some(r) = self.rows.iter_mut().find(|r| r.sel == sel) {
                            r.online = false;
                        }
                    }
                    Event::Battery(b) => self.set_battery(&sel, Some(b.to_string()), b.percent()),
                    _ => {}
                }
                refresh = self.selected.as_deref() == Some(sel.as_str())
                    && matches!(event, Event::Dpi { .. } | Event::ReportRate { .. });
            }
            Update::Preset { sel, .. } => refresh = self.selected.as_deref() == Some(sel.as_str()),
            Update::Line(_) | Update::Receiver { .. } => {}
        }
        if self.selected.is_none()
            && let Some(r) = self.rows.iter().find(|r| r.online)
        {
            self.selected = Some(r.sel.clone());
            refresh = true;
        }
        if refresh {
            self.refresh_selected();
        }
    }

    fn on_snapshot(&mut self, sel: String, r: Result<Snapshot, String>) {
        self.pending = self.pending.saturating_sub(1);
        match r {
            Ok(s) => {
                // A DPI/rate change went through: keep it as the model's preset, so it comes
                // back whenever the mouse connects (host-mode settings do not survive on it).
                if std::mem::take(&mut self.save_preset) && s.mode != Some(MODE_ONBOARD) {
                    let (dpi, rate) = (s.dpi.as_ref().map(|d| d.current), s.rate.as_ref().map(|r| r.0));
                    if s.preset.as_ref().is_none_or(|p| p.dpi != dpi || p.rate != rate) {
                        self.send(&sel, Cmd::SavePreset { dpi, rate });
                    }
                }
                if self.selected.as_deref() != Some(sel.as_str()) {
                    return;
                }
                if let Some(d) = &s.dpi {
                    self.dpi_input = d.current.to_string();
                }
                if s.needs_curve
                    && let Some(m) = s.model
                    && self.ghub_curve.as_ref().is_none_or(|g| g.0 != m)
                {
                    self.ghub_curve = Some((m, battery::find_ghub_curve(m)));
                }
                if let Some(row) = self.rows.iter_mut().find(|r| r.sel == sel) {
                    row.battery = s.battery.clone();
                    row.percent = s.battery_percent;
                }
                self.snap = Some(s);
            }
            Err(e) => {
                self.save_preset = false;
                self.say(true, e);
            }
        }
    }

    fn update_tray(&mut self, ctx: &egui::Context) {
        let parts: Vec<String> = self
            .rows
            .iter()
            .filter(|r| r.online)
            .map(|r| match r.percent {
                Some(p) => format!("{} {p}%", r.name),
                None => r.name.clone(),
            })
            .collect();
        let tip = if parts.is_empty() {
            TITLE.to_string()
        } else {
            format!("{TITLE}\n{}", parts.join("\n"))
        };
        if let Some(t) = &self.tray
            && tip != self.tray_tip
        {
            t.set_tooltip(&tip);
        }
        self.tray_tip = tip;

        // Icons: the window/taskbar battery shows the lowest device, the tray one bar each
        // for the first two devices (in the order of the list).
        let levels: Vec<Option<icons::Level>> = self
            .rows
            .iter()
            .map(|r| {
                let pct = r.percent.filter(|_| r.online)?;
                Some(icons::Level {
                    pct,
                    low: pct <= self.alert_level(&r.sel),
                })
            })
            .collect();
        let lowest = levels.iter().flatten().min_by_key(|l| l.pct).copied();
        let state = (
            lowest,
            levels.first().copied().flatten(),
            levels.get(1).copied().flatten(),
        );
        if self.icon_state != Some(state) {
            self.icon_state = Some(state);
            let (small, big) = os::icon_sizes();
            if let Some(t) = &self.tray {
                t.set_levels(state.1, state.2);
            }
            // winit only updates the small icon at run time; the taskbar shows the big one
            if self.hwnd != 0 {
                os::set_window_icons(
                    self.hwnd as _,
                    &icons::app(small, lowest),
                    small,
                    &icons::app(big, lowest),
                    big,
                );
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(egui::IconData {
                    rgba: icons::app(big, lowest),
                    width: big,
                    height: big,
                }))));
            }
        }
    }

    fn set_autostart(&mut self, on: bool) {
        let exe = std::env::current_exe().unwrap_or_default();
        let r = if on {
            autostart::enable(&format!("\"{}\" --tray", exe.display())).and_then(|_| {
                // The GUI now takes over from the background watcher.
                if self.bg_running {
                    autostart::stop_running(&platform::exe("lghub-lite-bg"))?;
                    self.bg_running = false;
                    let _ = self.tx.send(ToWorker::ApplyPresets(true));
                    self.waker.wake();
                }
                Ok(())
            })
        } else {
            autostart::disable()
        };
        match r {
            Ok(()) => self.autostart_gui = on,
            Err(e) => self.say(true, format!("start at login: {e}")),
        }
    }
}

fn worker(
    cfg: PathBuf,
    rx: Receiver<ToWorker>,
    tx: Sender<FromWorker>,
    waker: Arc<Waker>,
    ctx: egui::Context,
    apply_presets: bool,
) {
    let send = |m: FromWorker| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    let (mut mon, first) = match Monitor::new(hidpp::WATCH_SWID, false, &cfg) {
        Ok(v) => v,
        Err(e) => return send(FromWorker::Fatal(e.to_string())),
    };
    mon.set_apply_presets(apply_presets);
    for u in first {
        send(FromWorker::Update(u));
    }
    loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                ToWorker::Exec(sel, cmd) => {
                    let r = mon.exec(&sel, cmd).map_err(|e| e.to_string());
                    send(FromWorker::Snapshot(sel, r));
                }
                ToWorker::ApplyPresets(on) => mon.set_apply_presets(on),
            }
        }
        match mon.next(Some(&waker)) {
            Ok(updates) => updates.into_iter().for_each(|u| send(FromWorker::Update(u))),
            Err(e) => {
                send(FromWorker::Update(Update::Line(format!("error: {e}"))));
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

impl eframe::App for App {
    /// Runs before every frame, and also while the window is hidden whenever a repaint is
    /// requested (worker updates, tray clicks), so the tray works with the window hidden.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                FromWorker::Update(u) => self.on_update(u),
                FromWorker::Snapshot(sel, r) => self.on_snapshot(sel, r),
                FromWorker::Fatal(e) => self.fatal = Some(e),
            }
        }
        self.update_tray(ctx);
        if self.message.as_ref().is_some_and(|m| m.2.elapsed() > MESSAGE_TIME) {
            self.message = None;
        }
        if let Some(t) = self.skin_dirty {
            if t.elapsed() > Duration::from_millis(600) {
                self.skin_dirty = None;
                let (dir, k) = (self.dir.join("skins"), self.skin().clone());
                if let Err(e) = skin::save(&dir, &k) {
                    self.skin_msg = Some((false, format!("Skin not saved: {e}")));
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(650));
            }
        }

        if self.flags.skins.swap(false, Ordering::Relaxed) {
            self.show_skins = true;
            self.flags.show.store(true, Ordering::Relaxed);
        }
        if self.flags.show.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.flags.quit.swap(false, Ordering::Relaxed) {
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // Closing the window hides it to the tray; Quit (tray menu) really exits.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4] // transparent: the widget paints its own rounded frame
    }
}

#[cfg(test)]
mod tests {
    use super::charge_state;

    #[test]
    fn level_full_is_not_a_charge_state() {
        assert_eq!(
            charge_state("95% level=full discharging external=none"),
            Some("Discharging")
        );
        assert_eq!(
            charge_state("40% level=good slow charging external=wired"),
            Some("Charging")
        );
        assert_eq!(charge_state("100% level=full full external=wired"), Some("Full"));
        assert_eq!(
            charge_state("≈62%  (3858 mV, flags=0x01, generic Li-ion estimate)"),
            None
        );
    }
}
