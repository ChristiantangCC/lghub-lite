//! lghub-lite-gui: a small window + tray front end over the lghub-lite library.
//! Not affiliated with or endorsed by Logitech.
//!
//! Threads: the UI thread runs eframe; one worker thread owns the `watch::Monitor` and does all
//! HID I/O (requests can block for seconds while a wireless device wakes up), so the UI never
//! blocks. Commands go UI -> worker over a channel plus `hid::Waker`; updates come back over a
//! channel plus `request_repaint`. While the window is hidden nothing is repainted and the
//! worker sleeps in the same WaitForMultipleObjects as `lghub-lite watch`.
#![windows_subsystem = "windows"]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use eframe::egui;
use lghub_lite::device::{Event, MODE_HOST, MODE_ONBOARD};
use lghub_lite::hid::Waker;
use lghub_lite::watch::{self, Cmd, Monitor, Snapshot, Update};
use lghub_lite::{autostart, config, hidpp};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const TITLE: &str = "lghub-lite";
const LOG_LINES: usize = 200;

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
}

/// Flags set by tray callbacks (which run outside `logic`) and consumed in `logic`.
#[derive(Default)]
struct TrayFlags {
    show: AtomicBool,
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
    message: Option<(bool, String)>, // (is_error, text)
    log: VecDeque<String>,
    fatal: Option<String>,
    tray: Option<TrayIcon>,
    tray_tip: String,
    flags: Arc<TrayFlags>,
    quitting: bool,
    autostart_gui: bool,
    bg_running: bool,
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

    if !win::single_instance() {
        win::focus_existing(TITLE);
        return;
    }
    let bg_running = win::mutex_exists("Local\\lghub-lite-bg");

    let (w, h, rgba) = icon_rgba();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size([640.0, 440.0])
            .with_min_inner_size([520.0, 340.0])
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
        Box::new(move |cc| Ok(Box::new(App::new(cc, cfg, bg_running)))),
    );
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: PathBuf, bg_running: bool) -> App {
        let ctx = cc.egui_ctx.clone();
        let (tx, worker_rx) = channel();
        let (worker_tx, rx) = channel();
        let waker = Arc::new(Waker::new().expect("cannot create event"));
        {
            let (waker, ctx, cfg) = (waker.clone(), ctx.clone(), cfg.clone());
            std::thread::spawn(move || worker(cfg, worker_rx, worker_tx, waker, ctx, !bg_running));
        }
        let flags = Arc::new(TrayFlags::default());
        let tray = build_tray(&ctx, flags.clone());
        let autostart_gui = autostart::status()
            .ok()
            .flatten()
            .is_some_and(|c| c.contains("lghub-lite-gui"));
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
            log: VecDeque::new(),
            fatal: None,
            tray,
            tray_tip: String::new(),
            flags,
            quitting: false,
            autostart_gui,
            bg_running,
        }
    }

    fn send(&mut self, sel: &str, cmd: Cmd) {
        self.pending += 1;
        let _ = self.tx.send(ToWorker::Exec(sel.to_string(), cmd));
        self.waker.wake();
    }

    fn refresh_selected(&mut self) {
        if let Some(sel) = self.selected.clone() {
            self.send(&sel, Cmd::Refresh);
        }
    }

    fn on_update(&mut self, u: Update) {
        self.log
            .push_back(format!("{} {}", watch::now(), u.to_string().trim_start()));
        while self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
        let mut refresh = false;
        match u {
            Update::Added {
                sel,
                name,
                battery,
                battery_percent,
                ..
            } => {
                refresh = self.selected.as_deref() == Some(sel.as_str());
                match self.rows.iter_mut().find(|r| r.sel == sel) {
                    Some(r) => {
                        r.name = name;
                        r.battery = battery;
                        r.percent = battery_percent;
                        r.online = true;
                    }
                    None => self.rows.push(Row {
                        sel,
                        name,
                        battery,
                        percent: battery_percent,
                        online: true,
                    }),
                }
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
                if let Some(r) = self.rows.iter_mut().find(|r| r.sel == sel) {
                    r.battery = Some(text);
                    r.percent = percent;
                }
            }
            Update::Event { sel, event } => {
                match &event {
                    Event::Connection { online: false, .. } => {
                        if let Some(r) = self.rows.iter_mut().find(|r| r.sel == sel) {
                            r.online = false;
                        }
                    }
                    Event::Battery(b) => {
                        if let Some(r) = self.rows.iter_mut().find(|r| r.sel == sel) {
                            r.battery = Some(b.to_string());
                            r.percent = b.percent();
                        }
                    }
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

    fn update_tray_tip(&mut self) {
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
        if tip != self.tray_tip {
            if let Some(t) = &self.tray {
                let _ = t.set_tooltip(Some(&tip));
            }
            self.tray_tip = tip;
        }
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
            Err(e) => self.message = Some((true, format!("autostart: {e}"))),
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
                std::thread::sleep(std::time::Duration::from_secs(1));
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
                FromWorker::Snapshot(sel, r) => {
                    self.pending = self.pending.saturating_sub(1);
                    match r {
                        Ok(s) if self.selected.as_deref() == Some(sel.as_str()) => {
                            if let Some(d) = &s.dpi {
                                self.dpi_input = d.current.to_string();
                            }
                            if let Some(row) = self.rows.iter_mut().find(|r| r.sel == sel) {
                                row.battery = s.battery.clone();
                                row.percent = s.battery_percent;
                            }
                            self.snap = Some(s);
                        }
                        Ok(_) => {}
                        Err(e) => self.message = Some((true, e)),
                    }
                }
                FromWorker::Fatal(e) => self.fatal = Some(e),
            }
        }
        self.update_tray_tip();

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
        egui::Panel::bottom("footer").show(ui, |ui| self.footer(ui));
        egui::Panel::left("devices")
            .resizable(false)
            .exact_size(190.0)
            .show(ui, |ui| self.device_list(ui));
        egui::CentralPanel::default().show(ui, |ui| self.details(ui));
    }
}

impl App {
    fn device_list(&mut self, ui: &mut egui::Ui) {
        ui.heading("Devices");
        ui.add_space(4.0);
        if self.rows.is_empty() {
            ui.label(if self.fatal.is_some() {
                "Device access failed."
            } else {
                "Looking for devices…"
            });
        }
        let mut clicked = None;
        for r in &self.rows {
            let pct = r.percent.map(|p| format!("  {p}%")).unwrap_or_default();
            let text = format!("{}{pct}\n{}", r.name, if r.online { r.sel.as_str() } else { "offline" });
            let selected = self.selected.as_deref() == Some(r.sel.as_str());
            let label = egui::RichText::new(text);
            let label = if r.online { label } else { label.weak() };
            if ui.selectable_label(selected, label).clicked() && !selected {
                clicked = Some(r.sel.clone());
            }
        }
        if let Some(sel) = clicked {
            self.selected = Some(sel);
            self.snap = None;
            self.message = None;
            self.refresh_selected();
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.fatal {
            ui.colored_label(ui.visuals().error_fg_color, format!("Cannot watch devices: {e}"));
            return;
        }
        let Some(sel) = self.selected.clone() else {
            ui.label("Select a device.");
            return;
        };
        let online = self.rows.iter().any(|r| r.sel == sel && r.online);
        let Some(s) = self.snap.as_ref().filter(|s| s.sel == sel) else {
            if online {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading device…");
                });
            } else {
                ui.label("The device is offline. Move it or switch it on.");
            }
            return;
        };
        let busy = self.pending > 0;
        ui.horizontal(|ui| {
            ui.heading(&s.name);
            ui.weak(&s.sel);
            if busy {
                ui.spinner();
            }
        });
        ui.add_space(6.0);

        // Battery
        match (s.battery_percent, &s.battery) {
            (Some(p), text) => {
                let detail = text.as_deref().unwrap_or("");
                ui.add(
                    egui::ProgressBar::new(p as f32 / 100.0)
                        .text(format!("{p}%"))
                        .desired_width(260.0),
                );
                ui.weak(detail);
            }
            (None, Some(t)) => {
                ui.label(format!("Battery: {t}"));
            }
            (None, None) => {}
        }
        ui.add_space(8.0);

        let mut cmd: Option<Cmd> = None;
        let onboard = s.mode == Some(MODE_ONBOARD);
        egui::Grid::new("settings")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                if let Some(m) = s.mode {
                    ui.label("Mode");
                    ui.horizontal(|ui| {
                        if m == MODE_ONBOARD {
                            ui.label("Onboard profile (set on the mouse)");
                            if ui
                                .add_enabled(!busy, egui::Button::new("Use settings from this PC"))
                                .clicked()
                            {
                                cmd = Some(Cmd::SetMode(MODE_HOST));
                            }
                        } else {
                            ui.label("Host (settings from this PC)");
                            if ui
                                .add_enabled(!busy, egui::Button::new("Back to onboard profile"))
                                .clicked()
                            {
                                cmd = Some(Cmd::SetMode(MODE_ONBOARD));
                            }
                        }
                    });
                    ui.end_row();
                }
                if let Some(d) = &s.dpi {
                    ui.label("DPI");
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(&mut self.dpi_input).desired_width(70.0);
                        let r = ui.add_enabled(!onboard && !busy, edit);
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let apply = ui.add_enabled(!onboard && !busy, egui::Button::new("Apply")).clicked();
                        if apply || enter {
                            match self.dpi_input.trim().parse::<u16>() {
                                Ok(v) if d.caps.allows(v) => cmd = Some(Cmd::SetDpi(v)),
                                _ => self.message = Some((true, format!("DPI must be one of: {}", d.caps))),
                            }
                        }
                        ui.weak(format!("now {}", d.current));
                    });
                    ui.end_row();
                }
                if let Some((current, allowed)) = &s.rate {
                    ui.label("Report rate");
                    let mut pick = *current;
                    ui.add_enabled_ui(!onboard && !busy, |ui| {
                        egui::ComboBox::from_id_salt("rate")
                            .selected_text(format!("{current} Hz"))
                            .show_ui(ui, |ui| {
                                for hz in allowed {
                                    ui.selectable_value(&mut pick, *hz, format!("{hz} Hz"));
                                }
                            });
                    });
                    if pick != *current {
                        cmd = Some(Cmd::SetRate(pick));
                    }
                    ui.end_row();
                }
                if s.dpi.is_some() || s.rate.is_some() {
                    ui.label("Preset");
                    ui.horizontal(|ui| {
                        match &s.preset {
                            Some(p) => ui.label(config::line(p).split_once(' ').map_or("", |x| x.1).to_string()),
                            None => ui.weak("none"),
                        };
                        let save = egui::Button::new("Save current");
                        if ui
                            .add_enabled(!busy, save)
                            .on_hover_text("Apply these settings every time it connects")
                            .clicked()
                        {
                            cmd = Some(Cmd::SavePreset {
                                dpi: s.dpi.as_ref().map(|d| d.current),
                                rate: s.rate.as_ref().map(|r| r.0),
                            });
                        }
                        if s.preset.is_some() && ui.add_enabled(!busy, egui::Button::new("Clear")).clicked() {
                            cmd = Some(Cmd::SavePreset { dpi: None, rate: None });
                        }
                    });
                    ui.end_row();
                }
            });
        if onboard && (s.dpi.is_some() || s.rate.is_some()) {
            ui.weak("DPI and report rate can be changed after switching to settings from this PC.");
        }
        if let Some(c) = cmd {
            self.message = None;
            self.send(&sel, c);
        }
        if let Some((err, text)) = &self.message {
            let color = if *err {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().text_color()
            };
            ui.colored_label(color, text);
        }
        ui.add_space(8.0);
        egui::CollapsingHeader::new("Event log")
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.monospace(l);
                        }
                    });
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut on = self.autostart_gui;
            if ui
                .checkbox(&mut on, "Start at login (in the tray)")
                .on_hover_text("Applies saved presets whenever a device connects")
                .changed()
            {
                self.set_autostart(on);
            }
            if self.bg_running {
                ui.weak("· presets are applied by lghub-lite-bg");
            }
        });
        ui.weak("Not affiliated with or endorsed by Logitech.");
        ui.add_space(2.0);
    }
}

fn build_tray(ctx: &egui::Context, flags: Arc<TrayFlags>) -> Option<TrayIcon> {
    let open = MenuItem::new("Open", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::with_items(&[&open, &quit]).ok()?;
    let (w, h, rgba) = icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, w, h).ok()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(TITLE)
        .with_icon(icon)
        .build()
        .ok()?;
    let (open_id, quit_id) = (open.id().clone(), quit.id().clone());
    {
        let (ctx, flags) = (ctx.clone(), flags.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if e.id == open_id {
                flags.show.store(true, Ordering::Relaxed);
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
fn icon_rgba() -> (u32, u32, Vec<u8>) {
    static ICON: Mutex<Option<Vec<u8>>> = Mutex::new(None);
    let mut cache = ICON.lock().unwrap_or_else(|e| e.into_inner());
    let px = cache.get_or_insert_with(|| {
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
                    [38, 132, 255, 255]
                };
                px[i..i + 4].copy_from_slice(&color);
            }
        }
        px
    });
    (32, 32, px.clone())
}

/// Small Win32 helpers: single instance and bringing an existing window forward.
mod win {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::{CreateMutexW, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
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
}
