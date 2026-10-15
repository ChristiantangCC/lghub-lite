//! lghub-lite-gui: a small skinnable battery widget + tray front end over the lghub-lite library.
//! Not affiliated with or endorsed by Logitech.
//!
//! Threads: the UI thread runs eframe; one worker thread owns the `watch::Monitor` and does all
//! HID I/O (requests can block for seconds while a wireless device wakes up), so the UI never
//! blocks. Commands go UI -> worker over a channel plus `hid::Waker`; updates come back over a
//! channel plus `request_repaint`. While the window is hidden nothing is repainted and the
//! worker sleeps in the same WaitForMultipleObjects as `lghub-lite watch`.
#![windows_subsystem = "windows"]

mod demo;
mod fonts;
mod history;
mod skin;
mod view;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use history::History;
use lghub_lite::device::{Event, MODE_ONBOARD};
use lghub_lite::hid::Waker;
use lghub_lite::watch::{Cmd, Monitor, Snapshot, Update};
use lghub_lite::{autostart, battery, config, hidpp};
use skin::{Settings, Skin};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

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

impl Row {
    fn charging(&self) -> bool {
        self.battery
            .as_deref()
            .is_some_and(|b| b.contains("charging") && !b.contains("discharging") || b.contains("full"))
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
    tray: Option<TrayIcon>,
    tray_tip: String,
    tray_low: bool,
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
    if demo.is_none() && !win::single_instance() {
        win::focus_existing(TITLE);
        return;
    }
    let bg_running = demo.is_none() && win::mutex_exists("Local\\lghub-lite-bg");

    let (w, h, rgba) = icon_rgba(false);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(if demo.is_some() { "lghub-lite demo" } else { TITLE })
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
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = cc.window_handle()
                && let RawWindowHandle::Win32(w) = h.as_raw()
            {
                win::full_client_area(w.hwnd.get() as _);
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
        let tray = build_tray(&ctx, flags.clone());
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
            tray_low: false,
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

    fn update_tray(&mut self) {
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
        let low = self.rows.iter().any(|r| r.online && r.alerted);
        if let Some(t) = &self.tray {
            if tip != self.tray_tip {
                let _ = t.set_tooltip(Some(&tip));
            }
            if low != self.tray_low {
                let (w, h, rgba) = icon_rgba(low);
                let _ = t.set_icon(tray_icon::Icon::from_rgba(rgba, w, h).ok());
            }
        }
        self.tray_tip = tip;
        self.tray_low = low;
    }

    fn set_autostart(&mut self, on: bool) {
        let exe = std::env::current_exe().unwrap_or_default();
        let r = if on {
            autostart::enable(&format!("\"{}\" --tray", exe.display())).and_then(|_| {
                // The GUI now takes over from the background watcher.
                if self.bg_running {
                    autostart::stop_running("lghub-lite-bg.exe")?;
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
    let (mut mon, first) = match Monitor::new(hidpp::DEFAULT_SWID, false, &cfg) {
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
        self.update_tray();
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

fn build_tray(ctx: &egui::Context, flags: Arc<TrayFlags>) -> Option<TrayIcon> {
    let open = MenuItem::new("Open", true, None);
    let skins = MenuItem::new("Skins…", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::with_items(&[&open, &skins, &quit]).ok()?;
    let (w, h, rgba) = icon_rgba(false);
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).ok()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(TITLE)
        .with_icon(icon)
        .build()
        .ok()?;
    let (open_id, skins_id, quit_id) = (open.id().clone(), skins.id().clone(), quit.id().clone());
    {
        let (ctx, flags) = (ctx.clone(), flags.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == open_id {
                flags.show.store(true, Ordering::Relaxed);
            } else if e.id == skins_id {
                flags.skins.store(true, Ordering::Relaxed);
            } else if e.id == quit_id {
                flags.quit.store(true, Ordering::Relaxed);
            }
            ctx.request_repaint();
        }));
    }
    let ctx = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        let left_click = matches!(
            e,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } | TrayIconEvent::DoubleClick { .. }
        );
        if left_click {
            flags.show.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        }
    }));
    Some(tray)
}

/// 32×32 RGBA app/tray icon drawn in code (a mouse outline), so no image assets are needed.
/// `low` draws it red, for a device under its alert level.
fn icon_rgba(low: bool) -> (u32, u32, Vec<u8>) {
    static ICONS: Mutex<[Option<Vec<u8>>; 2]> = Mutex::new([None, None]);
    let mut cache = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    let px = cache[low as usize].get_or_insert_with(|| {
        let body: [u8; 4] = if low { [235, 64, 52, 255] } else { [38, 132, 255, 255] };
        let mut px = vec![0u8; 32 * 32 * 4];
        for y in 0..32 {
            for x in 0..32 {
                let (dx, dy) = ((x as f32 - 15.5) / 10.5, (y as f32 - 16.0) / 14.5);
                let d = dx * dx + dy * dy;
                let i = (y * 32 + x) * 4;
                let line = (x == 15 || x == 16) && y < 15 || y == 14 && (6..26).contains(&x);
                let color: [u8; 4] = if d > 1.0 {
                    [0, 0, 0, 0]
                } else if line && d < 0.92 {
                    [255, 255, 255, 255]
                } else {
                    body
                };
                px[i..i + 4].copy_from_slice(&color);
            }
        }
        px
    });
    (32, 32, px.clone())
}

/// Small Win32 helpers: single instance, bringing an existing window forward, file dialogs.
mod win {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OPENFILENAMEW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_SHOW, SetForegroundWindow, ShowWindow};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// True if this is the only GUI instance. The mutex lives until the process exits.
    pub fn single_instance() -> bool {
        let name = wide("Local\\lghub-lite-gui");
        // SAFETY: NUL-terminated name, default security.
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        // SAFETY: thread-local error right after the call.
        h.is_null() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
    }

    /// True if a named mutex exists (used to detect a running lghub-lite-bg).
    pub fn mutex_exists(name: &str) -> bool {
        let n = wide(name);
        // SAFETY: NUL-terminated name; handle closed right away.
        let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, n.as_ptr()) };
        if h.is_null() {
            return false;
        }
        unsafe { CloseHandle(h) };
        true
    }

    pub fn focus_existing(title: &str) {
        let t = wide(title);
        // SAFETY: NUL-terminated title; the window belongs to our other instance.
        unsafe {
            let w = FindWindowW(std::ptr::null(), t.as_ptr());
            if !w.is_null() {
                ShowWindow(w, SW_SHOW);
                SetForegroundWindow(w);
            }
        }
    }

    /// winit gives borderless windows a 1 px top strip for the system drop shadow, which
    /// shifts the content down by a pixel and draws a dark line. Take the whole window as
    /// client area instead (the widget paints its own edge).
    pub fn full_client_area(hwnd: windows_sys::Win32::Foundation::HWND) {
        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, WM_NCCALCSIZE,
        };

        unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM, _: usize, _: usize) -> LRESULT {
            if msg == WM_NCCALCSIZE && w != 0 {
                return 0; // client rect = window rect
            }
            // SAFETY: forwarding the message unchanged to the next handler.
            unsafe { DefSubclassProc(h, msg, w, l) }
        }
        // SAFETY: hwnd is our live top-level window; the subclass lives as long as it does.
        unsafe {
            SetWindowSubclass(hwnd, Some(proc), 1, 0);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            );
        }
    }

    /// The standard Windows open/save dialog for skin files. `save` proposes `name`.
    pub fn skin_file_dialog(save: bool, name: &str) -> Option<PathBuf> {
        let filter: Vec<u16> = "Skin files (*.json)\0*.json\0All files\0*.*\0\0"
            .encode_utf16()
            .collect();
        let mut buf = [0u16; 1024];
        for (i, c) in name.encode_utf16().take(buf.len() - 1).enumerate() {
            buf[i] = c;
        }
        let ext = wide("json");
        // SAFETY: zeroed is a valid OPENFILENAMEW; the pointers below outlive the call.
        let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
        ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.lpstrDefExt = ext.as_ptr();
        ofn.Flags = OFN_NOCHANGEDIR | if save { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST };
        // SAFETY: ofn is fully initialised for the call.
        let ok = unsafe {
            if save {
                GetSaveFileNameW(&mut ofn)
            } else {
                GetOpenFileNameW(&mut ofn)
            }
        };
        if ok == 0 {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}
