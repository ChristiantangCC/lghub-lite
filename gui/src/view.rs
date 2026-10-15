//! The window: a skinned battery widget (display, segment gauge, 24 h history, controls, a
//! settings drawer and the device list) and, next to it on demand, the skin manager.
//! Everything is painted with egui; the look follows the "Battery Tracker v3" design.

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, FontId, Layout, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};
use lghub_lite::device::{MODE_HOST, MODE_ONBOARD};
use lghub_lite::watch::Cmd;

use crate::history::{self, BARS};
use crate::skin::{self, BAD, Digits, FACE, FIELDS, FRAME, HI, INK, LCD, LCD_DIM, LCD_INK, LO, Skin};
use crate::{App, win};

pub const WIDGET_W: f32 = 420.0;
const MANAGER_W: f32 = 400.0;
const GAP: f32 = 16.0;
const DRAWER_H: f32 = 118.0;
const GAUGE_W: f32 = 28.0;

// Skin manager colours (fixed, not skinned).
const M_BG: Color32 = Color32::from_rgb(0x0d, 0x0e, 0x10);
const M_CARD: Color32 = Color32::from_rgb(0x16, 0x17, 0x1a);
const M_CARD_HOVER: Color32 = Color32::from_rgb(0x1d, 0x1f, 0x23);
const M_EDGE: Color32 = Color32::from_rgb(0x26, 0x28, 0x2d);
const M_EDGE2: Color32 = Color32::from_rgb(0x33, 0x36, 0x3b);
const M_TEXT: Color32 = Color32::from_rgb(0xe7, 0xe8, 0xea);
const M_SOFT: Color32 = Color32::from_rgb(0xc9, 0xcc, 0xd1);
const M_DIM: Color32 = Color32::from_rgb(0x7d, 0x80, 0x87);
const M_ACCENT: Color32 = Color32::from_rgb(0x7f, 0xd3, 0xde);
const M_TEAL: Color32 = Color32::from_rgb(0x2f, 0x8e, 0x9b);
const M_TEAL_HOVER: Color32 = Color32::from_rgb(0x38, 0xa2, 0xb0);
const M_ERR: Color32 = Color32::from_rgb(0xff, 0x8a, 0x7a);

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

fn sans(size: f32) -> FontId {
    FontId::proportional(size)
}

fn text_w(ui: &Ui, s: &str, font: &FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
        .size()
        .x
}

/// Shorten `s` with "…" to fit `max_w`.
fn elide(ui: &Ui, s: &str, font: &FontId, max_w: f32) -> String {
    if text_w(ui, s, font) <= max_w {
        return s.to_string();
    }
    let mut chars: Vec<char> = s.chars().collect();
    while chars.pop().is_some() {
        let t: String = chars.iter().collect::<String>() + "…";
        if text_w(ui, &t, font) <= max_w {
            return t;
        }
    }
    "…".into()
}

/// A raised skin button: face fill, edge outline, highlight on the top edge.
fn raised(ui: &mut Ui, size: Vec2, fill: Color32, k: &Skin, tip: &str) -> (Rect, Response) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() {
        mix(fill, Color32::WHITE, 0.12)
    } else {
        fill
    };
    let p = ui.painter();
    p.rect_filled(rect, 7.0, fill);
    p.rect_stroke(rect, 7.0, Stroke::new(1.0, k.c(LO)), StrokeKind::Outside);
    p.line_segment(
        [rect.left_top() + vec2(5.0, 0.5), rect.right_top() + vec2(-5.0, 0.5)],
        Stroke::new(1.0, k.c(HI)),
    );
    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
    let resp = if tip.is_empty() { resp } else { resp.on_hover_text(tip) };
    (rect, resp)
}

/// A flat outlined chip on the display, optionally filled (selected).
fn chip(ui: &mut Ui, label: &str, k: &Skin, filled: bool, enabled: bool) -> Response {
    let font = mono(10.5);
    let size = vec2(text_w(ui, label, &font) + 14.0, 22.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    let fill = if filled {
        k.c(LCD_DIM)
    } else if resp.hovered() && enabled {
        alpha(k.c(LO), 200)
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, 5.0, fill);
    p.rect_stroke(rect, 5.0, Stroke::new(1.0, k.c(LCD_DIM)), StrokeKind::Inside);
    let ink = if enabled { k.c(LCD_INK) } else { k.c(LCD_DIM) };
    p.text(rect.center(), Align2::CENTER_CENTER, label, font, ink);
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// Recessed display panel (the "LCD").
fn lcd_shapes(rect: Rect, radius: f32, k: &Skin) -> Vec<Shape> {
    vec![
        Shape::rect_filled(rect, radius, k.c(LCD)),
        // inset look: a darker band at the top and an edge outline
        Shape::rect_filled(
            Rect::from_min_size(rect.min + vec2(2.0, 1.0), vec2(rect.width() - 4.0, 3.0)),
            radius.min(3.0),
            Color32::from_black_alpha(60),
        ),
        Shape::rect_stroke(rect, radius, Stroke::new(1.0, k.c(LO)), StrokeKind::Inside),
    ]
}

fn lcd_panel(p: &Painter, rect: Rect, radius: f32, k: &Skin) {
    p.extend(lcd_shapes(rect, radius, k));
}

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// A small filled triangle (the default fonts have no arrow glyphs).
fn triangle(p: &Painter, c: Pos2, r: f32, dir: Dir, color: Color32) {
    let pts = match dir {
        Dir::Left => vec![c + vec2(-r, 0.0), c + vec2(r * 0.8, -r), c + vec2(r * 0.8, r)],
        Dir::Right => vec![c + vec2(r, 0.0), c + vec2(-r * 0.8, r), c + vec2(-r * 0.8, -r)],
        Dir::Up => vec![c + vec2(0.0, -r * 0.8), c + vec2(r, r * 0.6), c + vec2(-r, r * 0.6)],
        Dir::Down => vec![c + vec2(0.0, r * 0.8), c + vec2(-r, -r * 0.6), c + vec2(r, -r * 0.6)],
    };
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// What the display shows for the selected device.
struct Cur {
    name: String,
    conn: String,
    pct: Option<u8>,
    alert: u8,
    color: Color32,
    time_left: String,
    drain: String,
    status: String,
    chips: Vec<String>,
    bars: [Option<u8>; BARS],
}

impl App {
    fn current(&self, k: &Skin) -> Cur {
        let row = self
            .selected
            .as_ref()
            .and_then(|s| self.rows.iter().find(|r| &r.sel == s));
        let Some(r) = row else {
            return Cur {
                name: "lghub-lite".into(),
                conn: String::new(),
                pct: None,
                alert: 20,
                color: k.c(LCD_DIM),
                time_left: String::new(),
                drain: String::new(),
                status: if self.fatal.is_some() {
                    "Device access failed"
                } else {
                    "Looking for devices…"
                }
                .into(),
                chips: self.fatal.iter().cloned().collect(),
                bars: [None; BARS],
            };
        };
        let now = history::now();
        let snap = self.snap.as_ref().filter(|s| s.sel == r.sel);
        let alert = self.alert_level(&r.sel);
        let pct = r.percent.filter(|_| r.online);
        let charging = r.charging();
        let drain = r.history.drain_tenths(now).filter(|_| !charging);
        let conn = match r.sel.split_once(':') {
            Some((_, slot)) => format!("Receiver · slot {slot}"),
            None => "USB".into(),
        };
        let mut chips = Vec::new();
        if let Some((a, b)) = r.protocol {
            chips.push(format!("HID++ {a}.{b}"));
        }
        let voltage = snap.and_then(|s| s.voltage_mv);
        if let Some(mv) = voltage {
            chips.push(format!("{mv} mV"));
        }
        if let Some(s) = snap.filter(|s| s.curve_label.is_some()) {
            let label = s.curve_label.as_deref().unwrap_or("");
            chips.push(
                if s.needs_curve {
                    "Estimated from voltage"
                } else if label.starts_with("your") {
                    "Measured curve"
                } else if label.starts_with("built-in") {
                    "Built-in curve"
                } else {
                    "G HUB curve"
                }
                .into(),
            );
        }
        chips.push(r.sel.clone());
        let status = if !r.online {
            "Offline".to_string()
        } else if let Some(b) = r.battery.as_deref() {
            if b.contains("discharging") {
                "Discharging".into()
            } else if b.contains("full") {
                "Full".into()
            } else if b.contains("charging") {
                "Charging".into()
            } else if voltage.is_some() || b.contains(" mV") {
                "On battery".into()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let time_left = if !r.online {
            String::new()
        } else if charging {
            "charging".into()
        } else if let Some(t) = snap.and_then(|s| s.tenths_left) {
            history::hours_text(t)
        } else if let (Some(p), Some(d)) = (pct, drain) {
            history::hours_text(p as u32 * 100 / d.max(1))
        } else {
            "— h left".into()
        };
        let drain = match drain {
            Some(d) => format!("−{}.{}% / h", d / 10, d % 10),
            None if r.online && !charging => "measuring drain".into(),
            None => String::new(),
        };
        Cur {
            name: r.name.clone(),
            conn,
            pct,
            alert,
            color: pct.map_or(k.c(LCD_DIM), |p| k.level(p, alert)),
            time_left,
            drain,
            status,
            chips,
            bars: r.history.bars(now),
        }
    }

    pub fn draw(&mut self, ui: &mut Ui) {
        self.take_dropped_files(ui.ctx());
        let k = self.skin().clone();
        ui.style_mut().interaction.selectable_labels = false;
        let content = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing = vec2(GAP, 8.0);
                    ui.vertical(|ui| {
                        ui.set_width(WIDGET_W);
                        self.widget(ui, &k);
                    });
                    if self.show_skins {
                        ui.vertical(|ui| {
                            ui.set_width(MANAGER_W);
                            self.manager(ui, &k);
                        });
                    }
                })
                .response
                .rect
            })
            .inner;
        // The window always fits the content.
        let want = vec2(content.right().ceil(), content.bottom().ceil());
        let have = ui.ctx().input(|i| i.viewport().inner_rect.map(|r| r.size()));
        if have.is_none_or(|h| (h - want).length() > 1.0) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
        }
    }

    // ------------------------------------------------------------------ the widget

    fn widget(&mut self, ui: &mut Ui, k: &Skin) {
        let cur = self.current(k);
        egui::Frame::NONE
            .fill(k.c(FRAME))
            .stroke(Stroke::new(1.0, k.c(LO)))
            .corner_radius(14.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
                let top = ui.max_rect();
                ui.painter().line_segment(
                    [
                        pos2(top.left() + 4.0, top.top() - 9.5),
                        pos2(top.right() - 4.0, top.top() - 9.5),
                    ],
                    Stroke::new(1.0, k.c(HI)),
                );
                self.title_bar(ui, k, &cur);
                self.display(ui, k, &cur);
                history_strip(ui, k, &cur);
                self.controls(ui, k, &cur);
                if let Some((err, text, _)) = &self.message {
                    let color = if *err { k.c(BAD) } else { k.c(INK) };
                    ui.add(egui::Label::new(egui::RichText::new(text).size(11.0).color(color)).wrap());
                }
                self.drawer(ui, k);
                self.device_list(ui, k);
                ui.label(
                    egui::RichText::new("Not affiliated with or endorsed by Logitech.")
                        .size(10.5)
                        .color(alpha(k.c(INK), 120)),
                );
            });
    }

    fn title_bar(&mut self, ui: &mut Ui, k: &Skin, cur: &Cur) {
        let (rect, drag) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click_and_drag());
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        let p = ui.painter().clone();
        let led = pos2(rect.left() + 8.0, rect.center().y);
        p.circle_filled(led, 6.0, alpha(cur.color, 50));
        p.circle_filled(led, 4.0, cur.color);
        p.text(
            pos2(rect.left() + 20.0, rect.center().y),
            Align2::LEFT_CENTER,
            "lghub-lite",
            sans(12.0),
            k.c(INK),
        );

        // window buttons: minimise, hide to tray
        let mut x = rect.right() - 10.0;
        for (i, (hover, tip)) in [(BAD, "Hide to tray"), (HI, "Minimise")].into_iter().enumerate() {
            let c = pos2(x, rect.center().y);
            let r = Rect::from_center_size(c, vec2(14.0, 14.0));
            let resp = ui
                .interact(r, ui.scope_id().with(("winbtn", i)), Sense::click())
                .on_hover_text(tip);
            p.circle_filled(c, 6.0, if resp.hovered() { k.c(hover) } else { k.c(FACE) });
            p.circle_stroke(c, 6.0, Stroke::new(1.0, k.c(LO)));
            if resp.clicked() {
                let cmd = if i == 0 {
                    egui::ViewportCommand::Close
                } else {
                    egui::ViewportCommand::Minimized(true)
                };
                ui.ctx().send_viewport_cmd(cmd);
            }
            x -= 18.0;
        }
        // skins toggle
        let label = "Skins";
        let w = text_w(ui, label, &sans(11.0)) + 22.0;
        let r = Rect::from_min_size(pos2(x - 4.0 - w, rect.top() + 2.0), vec2(w, 18.0));
        let resp = ui
            .interact(r, ui.scope_id().with("skins-toggle"), Sense::click())
            .on_hover_text("Choose, edit, load or export skins")
            .on_hover_cursor(CursorIcon::PointingHand);
        if resp.hovered() {
            p.rect_filled(r, 5.0, alpha(k.c(HI), 90));
        }
        let ink = alpha(k.c(INK), 200);
        p.text(
            r.left_center() + vec2(6.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            sans(11.0),
            ink,
        );
        let dir = if self.show_skins { Dir::Left } else { Dir::Right };
        triangle(&p, r.right_center() - vec2(8.0, 0.0), 3.0, dir, ink);
        if resp.clicked() {
            self.show_skins = !self.show_skins;
        }
    }

    fn display(&mut self, ui: &mut Ui, k: &Skin, cur: &Cur) {
        let lcd_w = ui.available_width() - GAUGE_W - 8.0;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let top = ui.cursor().min;
            // lay out the display content first, then put the panel behind it
            let bg = ui.painter().add(Shape::Noop);
            let inner = Rect::from_min_size(top + vec2(14.0, 12.0), vec2(lcd_w - 28.0, 400.0));
            let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
            child.spacing_mut().item_spacing = vec2(4.0, 8.0);
            display_content(&mut child, k, cur);
            let lcd = Rect::from_min_max(top, pos2(top.x + lcd_w, child.min_rect().bottom() + 12.0));
            ui.painter().set(bg, Shape::Vec(lcd_shapes(lcd, 10.0, k)));
            ui.allocate_rect(lcd, Sense::hover());
            let (g, _) = ui.allocate_exact_size(vec2(GAUGE_W, lcd.height()), Sense::hover());
            gauge(ui, g, k, cur);
        });
    }

    fn controls(&mut self, ui: &mut Ui, k: &Skin, cur: &Cur) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let n = self.rows.len();
            let idx = self
                .selected
                .as_ref()
                .and_then(|s| self.rows.iter().position(|r| &r.sel == s));
            for (dir, tip, step) in [
                (Dir::Left, "Previous device", n.saturating_sub(1)),
                (Dir::Right, "Next device", 1),
            ] {
                let (rect, resp) = raised(ui, vec2(30.0, 26.0), k.c(FACE), k, tip);
                triangle(ui.painter(), rect.center(), 4.0, dir, k.c(INK));
                if resp.clicked() && n > 0 {
                    let i = idx.map_or(0, |i| (i + step) % n);
                    self.select(self.rows[i].sel.clone());
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Settings (drawer)
                let fill = if self.drawer { k.c(LO) } else { k.c(FACE) };
                let (rect, resp) = raised(ui, vec2(90.0, 26.0), fill, k, "Device settings");
                let p = ui.painter();
                let ink = k.c(INK);
                let o = rect.left_center() + vec2(9.0, 0.0);
                for (y, cx) in [(-3.0, 3.0), (3.0, 7.0)] {
                    p.line_segment([o + vec2(0.0, y), o + vec2(10.0, y)], Stroke::new(1.4, ink));
                    p.circle_filled(o + vec2(cx, y), 1.8, ink);
                }
                p.text(o + vec2(16.0, 0.0), Align2::LEFT_CENTER, "Settings", sans(11.0), ink);
                let dir = if self.drawer { Dir::Up } else { Dir::Down };
                triangle(p, rect.right_center() - vec2(10.0, 0.0), 3.0, dir, alpha(ink, 180));
                if resp.clicked() {
                    self.drawer = !self.drawer;
                }
                // Tray = start at login, in the tray
                let (rect, resp) = raised(ui, vec2(56.0, 26.0), k.c(FACE), k, "Start at login, in the tray");
                let led = rect.left_center() + vec2(12.0, 0.0);
                let on = self.autostart_gui;
                if on {
                    ui.painter().circle_filled(led, 5.0, alpha(k.c(skin::OK), 60));
                }
                ui.painter()
                    .circle_filled(led, 3.0, if on { k.c(skin::OK) } else { k.c(LO) });
                ui.painter()
                    .text(led + vec2(9.0, 0.0), Align2::LEFT_CENTER, "Tray", sans(11.0), k.c(INK));
                if resp.clicked() {
                    self.set_autostart(!on);
                }
                // Alert level stepper
                let (rect, _) = ui.allocate_exact_size(vec2(78.0, 26.0), Sense::hover());
                lcd_panel(ui.painter(), rect, 7.0, k);
                let minus = Rect::from_min_size(rect.min, vec2(22.0, 26.0));
                let plus = Rect::from_min_size(rect.right_top() - vec2(22.0, 0.0), vec2(22.0, 26.0));
                let sel = self.selected.clone();
                for (r, label, delta) in [(minus, "−", -5i16), (plus, "+", 5)] {
                    let resp = ui
                        .interact(r, ui.scope_id().with(("alert", label)), Sense::click())
                        .on_hover_cursor(CursorIcon::PointingHand);
                    if resp.hovered() {
                        ui.painter().rect_filled(r.shrink(1.0), 6.0, k.c(LO));
                    }
                    ui.painter()
                        .text(r.center(), Align2::CENTER_CENTER, label, sans(13.0), k.c(LCD_INK));
                    if resp.clicked()
                        && let Some(sel) = &sel
                    {
                        let v = (cur.alert as i16 + delta).clamp(5, 50) as u8;
                        self.settings.set_alert(sel, v);
                        self.save_settings();
                    }
                }
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    format!("{}%", cur.alert),
                    mono(11.5),
                    k.c(BAD),
                );
                ui.label(egui::RichText::new("Alert at").size(11.0).color(alpha(k.c(INK), 205)));
            });
        });
    }

    fn drawer(&mut self, ui: &mut Ui, k: &Skin) {
        let t = ui
            .ctx()
            .animate_bool_with_time(ui.scope_id().with("drawer"), self.drawer, 0.28);
        if t <= 0.0 {
            return;
        }
        let w = ui.available_width();
        let (slot, _) = ui.allocate_exact_size(vec2(w, DRAWER_H * t), Sense::hover());
        let full = Rect::from_min_size(slot.min, vec2(w, DRAWER_H));
        let mut child = ui.new_child(UiBuilder::new().max_rect(full));
        child.set_clip_rect(slot.intersect(ui.clip_rect()));
        lcd_panel(child.painter(), full, 10.0, k);
        let inner = full.shrink2(vec2(12.0, 10.0));
        let mut c = child.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
        c.spacing_mut().item_spacing = vec2(6.0, 8.0);
        self.drawer_content(&mut c, k);
    }

    fn drawer_content(&mut self, ui: &mut Ui, k: &Skin) {
        let dim = |s: &str| egui::RichText::new(s).size(11.5).color(k.c(LCD_DIM));
        let Some(sel) = self.selected.clone() else {
            ui.label(dim("No device selected."));
            return;
        };
        let Some(s) = self.snap.as_ref().filter(|s| s.sel == sel) else {
            let online = self.rows.iter().any(|r| r.sel == sel && r.online);
            ui.label(dim(if online {
                "Reading device…"
            } else {
                "The device is offline. Move it or switch it on."
            }));
            return;
        };
        let busy = self.pending > 0;
        let had_preset = s.preset.is_some();
        let mut cmd: Option<Cmd> = None;
        let mut err: Option<String> = None;
        let mut import = None;
        if s.dpi.is_some() || s.rate.is_some() || s.mode.is_some() {
            let onboard = s.mode == Some(MODE_ONBOARD);
            let locked = onboard || busy;
            egui::Grid::new("mouse-settings")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .min_row_height(24.0)
                .show(ui, |ui| {
                    if let Some(d) = &s.dpi {
                        ui.label(dim("DPI"));
                        ui.horizontal(|ui| {
                            let v = ui.visuals_mut();
                            v.extreme_bg_color = Color32::TRANSPARENT;
                            for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
                                w.bg_stroke = Stroke::new(1.0, k.c(LCD_DIM));
                                w.corner_radius = 6.0.into();
                            }
                            v.selection.stroke = Stroke::new(1.0, k.c(LCD_INK));
                            let edit = egui::TextEdit::singleline(&mut self.dpi_input)
                                .desired_width(54.0)
                                .font(mono(11.5))
                                .text_color(k.c(LCD_INK))
                                .margin(egui::Margin::symmetric(8, 4));
                            let r = ui.add_enabled(!locked, edit);
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let (rect, resp) = raised(ui, vec2(52.0, 24.0), k.c(FACE), k, "");
                            ui.painter()
                                .text(rect.center(), Align2::CENTER_CENTER, "Apply", sans(11.5), k.c(INK));
                            if (resp.clicked() || enter) && !locked {
                                match self.dpi_input.trim().parse::<u16>() {
                                    Ok(v) if d.caps.allows(v) => {
                                        cmd = Some(Cmd::SetDpi(v));
                                    }
                                    _ => err = Some(format!("DPI must be one of: {}", d.caps)),
                                }
                            } else if resp.clicked() && onboard {
                                err = Some("Switch to host mode to change DPI.".into());
                            }
                            ui.label(
                                egui::RichText::new(format!("now {}", d.current))
                                    .font(mono(10.5))
                                    .color(k.c(LCD_DIM)),
                            );
                        });
                        ui.end_row();
                    }
                    if let Some((current, allowed)) = &s.rate {
                        ui.label(dim("Rate"));
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            for hz in allowed {
                                let label = if *hz >= 1000 {
                                    format!("{}k", hz / 1000)
                                } else {
                                    hz.to_string()
                                };
                                let r = chip(ui, &label, k, hz == current, !locked).on_hover_text(format!("{hz} Hz"));
                                if r.clicked() && hz != current {
                                    cmd = Some(Cmd::SetRate(*hz));
                                }
                            }
                        });
                        ui.end_row();
                    }
                    if let Some(m) = s.mode {
                        ui.label(dim("Mode"));
                        ui.horizontal(|ui| {
                            let (label, action, to) = if m == MODE_ONBOARD {
                                ("Onboard profile", "Use host", MODE_HOST)
                            } else {
                                ("Host — settings from this PC", "Use onboard", MODE_ONBOARD)
                            };
                            ui.label(egui::RichText::new(label).size(11.5).color(k.c(LCD_INK)));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let tip = if to == MODE_HOST {
                                    "Let this PC set DPI and report rate (saved as this mouse's preset)"
                                } else {
                                    "Go back to the profile stored on the mouse"
                                };
                                if chip(ui, action, k, false, !busy).on_hover_text(tip).clicked() {
                                    cmd = Some(Cmd::SetMode(to));
                                }
                            });
                        });
                        ui.end_row();
                    }
                });
            if let Some(c) = &cmd {
                match c {
                    Cmd::SetDpi(_) | Cmd::SetRate(_) => self.save_preset = true,
                    // Back to onboard: drop the preset, or it would switch to host again on connect.
                    Cmd::SetMode(m) if *m == MODE_ONBOARD && had_preset => {
                        let sel2 = sel.clone();
                        self.send(&sel2, Cmd::SavePreset { dpi: None, rate: None });
                    }
                    _ => {}
                }
            }
        } else if let Some(label) = s.curve_label.clone() {
            let ink = k.c(LCD_INK);
            ui.label(
                egui::RichText::new(format!("Battery curve: {label}"))
                    .size(11.5)
                    .color(ink),
            );
            let note = if s.needs_curve {
                "No adjustable settings. Until one full discharge is recorded (from full to nearly empty, then charging) the percentage is a rough estimate."
            } else {
                "No adjustable settings. Each full discharge refines your measured curve."
            };
            ui.add(egui::Label::new(egui::RichText::new(note).size(10.5).color(k.c(LCD_DIM))).wrap());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if chip(ui, "Build from recording", k, false, !busy)
                    .on_hover_text(s.recording.clone().unwrap_or_default())
                    .clicked()
                {
                    cmd = Some(Cmd::BuildCurve);
                }
                if s.needs_curve
                    && let Some(src) = self.ghub_curve.as_ref().and_then(|g| g.1.clone())
                    && chip(ui, "Import from G HUB", k, false, true)
                        .on_hover_text(src.display().to_string())
                        .clicked()
                {
                    import = Some(src);
                }
                if chip(ui, "Open folder", k, false, true)
                    .on_hover_text(self.dir.display().to_string())
                    .clicked()
                {
                    let _ = std::fs::create_dir_all(&self.dir);
                    let _ = std::process::Command::new("explorer").arg(&self.dir).spawn();
                }
            });
        } else {
            ui.label(
                egui::RichText::new("No adjustable settings")
                    .size(11.5)
                    .color(k.c(LCD_INK)),
            );
            ui.label(dim("This device has no DPI, report rate or battery curve."));
        }
        if let Some(e) = err {
            self.say(true, e);
        }
        if let Some(c) = cmd {
            self.message = None;
            self.send(&sel, c);
        }
        if let Some(src) = import {
            match lghub_lite::battery::import_curve(&src, &self.dir) {
                Ok(dest) => {
                    self.say(false, format!("Imported {}", dest.display()));
                    self.ghub_curve = None;
                    self.send(&sel, Cmd::Refresh);
                }
                Err(e) => self.say(true, format!("Import failed: {e}")),
            }
        }
    }

    fn device_list(&mut self, ui: &mut Ui, k: &Skin) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("DEVICES")
                    .size(10.5)
                    .strong()
                    .color(alpha(k.c(INK), 180)),
            );
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() - 4.0, 10.0), Sense::hover());
            ui.painter()
                .line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, k.c(LO)));
        });
        let w = ui.available_width();
        let row_h = 30.0;
        let n = self.rows.len().max(1);
        let (rect, _) = ui.allocate_exact_size(vec2(w, n as f32 * (row_h + 2.0) + 6.0), Sense::hover());
        lcd_panel(ui.painter(), rect, 9.0, k);
        if self.rows.is_empty() {
            let msg = if self.fatal.is_some() {
                "Device access failed"
            } else {
                "Looking for devices…"
            };
            ui.painter()
                .text(rect.center(), Align2::CENTER_CENTER, msg, sans(12.0), k.c(LCD_DIM));
            return;
        }
        let mut clicked = None;
        for (i, r) in self.rows.iter().enumerate() {
            let rr = Rect::from_min_size(
                rect.min + vec2(4.0, 4.0 + i as f32 * (row_h + 2.0)),
                vec2(w - 8.0, row_h),
            );
            let resp = ui
                .interact(rr, ui.scope_id().with(("row", &r.sel)), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            let selected = self.selected.as_deref() == Some(r.sel.as_str());
            let p = ui.painter();
            if selected || resp.hovered() {
                p.rect_filled(rr, 6.0, k.c(LO));
            }
            let alert = self.alert_level(&r.sel);
            let pct = r.percent.filter(|_| r.online);
            let color = pct.map_or(k.c(LCD_DIM), |p| k.level(p, alert));
            let y = rr.center().y;
            p.text(
                pos2(rr.left() + 10.0, y),
                Align2::LEFT_CENTER,
                (i + 1).to_string(),
                mono(10.5),
                k.c(LCD_DIM),
            );
            let name_font = sans(12.5);
            let name_w = rr.width() - 30.0 - 48.0 - 38.0 - 30.0;
            let name = elide(ui, &r.name, &name_font, name_w);
            let ink = if r.online { k.c(LCD_INK) } else { k.c(LCD_DIM) };
            p.text(pos2(rr.left() + 30.0, y), Align2::LEFT_CENTER, name, name_font, ink);
            let bar = Rect::from_min_size(pos2(rr.right() - 10.0 - 38.0 - 10.0 - 48.0, y - 2.0), vec2(48.0, 4.0));
            p.rect_filled(
                bar,
                2.0,
                k.c(LO)
                    .gamma_multiply(if selected || resp.hovered() { 0.6 } else { 1.0 }),
            );
            if let Some(v) = pct {
                let fill = Rect::from_min_size(bar.min, vec2(48.0 * v as f32 / 100.0, 4.0));
                p.rect_filled(fill, 2.0, color);
            }
            let label = pct.map_or_else(|| if r.online { "—".into() } else { "off".into() }, |v| format!("{v}%"));
            p.text(
                pos2(rr.right() - 10.0, y),
                Align2::RIGHT_CENTER,
                label,
                mono(11.5),
                color,
            );
            if resp.clicked() {
                clicked = Some(r.sel.clone());
            }
        }
        if let Some(sel) = clicked {
            self.select(sel);
        }
    }

    // ------------------------------------------------------------------ skin manager

    fn manager(&mut self, ui: &mut Ui, k: &Skin) {
        let preview_pct = self.current(k).pct.unwrap_or(53);
        egui::Frame::NONE
            .fill(M_BG)
            .stroke(Stroke::new(1.0, M_EDGE))
            .corner_radius(14.0)
            .inner_margin(20.0)
            .show(ui, |ui| {
                ui.set_width(MANAGER_W - 40.0);
                ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
                ui.label(egui::RichText::new("Skins").size(20.0).strong().color(M_TEXT));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Pick one, tweak the colours, or load a skin file someone shared.")
                            .size(13.0)
                            .color(Color32::from_rgb(0x8a, 0x8d, 0x94)),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
                self.skin_cards(ui, k, preview_pct);
                ui.add_space(4.0);
                self.load_export(ui, k);
                if let Some((ok, msg)) = &self.skin_msg {
                    ui.label(egui::RichText::new(msg).size(12.0).color(if *ok { M_ACCENT } else { M_ERR }));
                }
                ui.add_space(4.0);
                self.skin_editor(ui, k);
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "A skin is a small .json file with a name, author, eleven colours and a digit style. Export one to see the format, edit it in any text editor, and share it. You can also drop a skin file on the window.",
                        )
                        .size(11.5)
                        .color(Color32::from_rgb(0x6b, 0x6e, 0x75)),
                    )
                    .wrap(),
                );
            });
    }

    fn skin_cards(&mut self, ui: &mut Ui, k: &Skin, pct: u8) {
        let cols = 3;
        let gap = 10.0;
        let w = ((MANAGER_W - 40.0) - gap * (cols as f32 - 1.0)) / cols as f32;
        let h = 86.0;
        let mut pick = None;
        for chunk in self.skins.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for s in chunk {
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
                    let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                    let p = ui.painter();
                    p.rect_filled(rect, 10.0, if resp.hovered() { M_CARD_HOVER } else { M_CARD });
                    let border = if s.id == k.id { M_ACCENT } else { M_EDGE };
                    p.rect_stroke(rect, 10.0, Stroke::new(1.0, border), StrokeKind::Inside);
                    let frame = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(w - 12.0, 46.0));
                    p.rect_filled(frame, 7.0, s.c(FRAME));
                    p.line_segment(
                        [frame.left_top() + vec2(4.0, 0.5), frame.right_top() + vec2(-4.0, 0.5)],
                        Stroke::new(1.0, s.c(HI)),
                    );
                    let lcd = frame.shrink(4.0);
                    p.rect_filled(lcd, 5.0, s.c(LCD));
                    let label = format!("{pct}%");
                    match s.digits {
                        Digits::Pixel => {
                            pixel_text(p, lcd.center(), Align2::CENTER_CENTER, &label, 18.0, s.c(skin::OK))
                        }
                        d => {
                            p.text(
                                lcd.center(),
                                Align2::CENTER_CENTER,
                                &label,
                                digit_font(d, 22.0),
                                s.c(skin::OK),
                            );
                        }
                    }
                    let name = elide(ui, &s.name, &sans(12.0), w - 12.0);
                    let author = elide(ui, &s.author, &sans(10.5), w - 12.0);
                    let p = ui.painter();
                    p.text(
                        pos2(rect.left() + 6.0, rect.top() + 58.0),
                        Align2::LEFT_TOP,
                        name,
                        sans(12.0),
                        M_TEXT,
                    );
                    p.text(
                        pos2(rect.left() + 6.0, rect.top() + 72.0),
                        Align2::LEFT_TOP,
                        author,
                        sans(10.5),
                        M_DIM,
                    );
                    if resp.clicked() {
                        pick = Some(s.id.clone());
                    }
                }
            });
        }
        if let Some(id) = pick {
            self.settings.skin = id;
            self.skin_msg = None;
            self.save_settings();
        }
    }

    fn load_export(&mut self, ui: &mut Ui, k: &Skin) {
        let w = (MANAGER_W - 40.0 - 8.0) / 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            ui.painter()
                .rect_filled(rect, 8.0, if resp.hovered() { M_TEAL_HOVER } else { M_TEAL });
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Load skin file…",
                sans(13.0),
                Color32::WHITE,
            );
            if resp.clicked()
                && let Some(path) = win::skin_file_dialog(false, "")
            {
                self.import_skin(&path);
            }
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::click());
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            if resp.hovered() {
                ui.painter().rect_filled(rect, 8.0, M_CARD_HOVER);
            }
            ui.painter()
                .rect_stroke(rect, 8.0, Stroke::new(1.0, M_EDGE2), StrokeKind::Inside);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Export current",
                sans(13.0),
                M_TEXT,
            );
            if resp.clicked() {
                let mut out = k.clone();
                if out.builtin() {
                    out.author = "you".into();
                }
                let name = format!("{}.skin.json", skin::slug(&out.name));
                if let Some(path) = win::skin_file_dialog(true, &name) {
                    self.skin_msg = Some(match lghub_lite::config::write_atomic(&path, &out.to_json()) {
                        Ok(()) => (
                            true,
                            format!("Exported {}", path.file_name().unwrap_or_default().to_string_lossy()),
                        ),
                        Err(e) => (false, format!("Export failed: {e}")),
                    });
                }
            }
        });
    }

    fn skin_editor(&mut self, ui: &mut Ui, k: &Skin) {
        egui::Frame::NONE
            .fill(M_CARD)
            .stroke(Stroke::new(1.0, M_EDGE))
            .corner_radius(12.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.set_width(MANAGER_W - 40.0 - 32.0);
                ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Edit “{}”", k.name))
                            .size(13.0)
                            .strong()
                            .color(M_TEXT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let note = if k.builtin() {
                            "Edits save as a new custom skin"
                        } else {
                            "Saved automatically"
                        };
                        ui.label(egui::RichText::new(note).size(11.0).color(M_DIM));
                    });
                });
                let col_w = (ui.available_width() - 14.0) / 2.0;
                let mut change: Option<(usize, [u8; 3])> = None;
                for pair in FIELDS.iter().enumerate().collect::<Vec<_>>().chunks(2) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        for (i, (_, label)) in pair {
                            ui.allocate_ui_with_layout(vec2(col_w, 22.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.spacing_mut().interact_size = vec2(26.0, 20.0);
                                let mut c = k.colors[*i];
                                if egui::color_picker::color_edit_button_srgb(ui, &mut c).changed() {
                                    change = Some((*i, c));
                                }
                                ui.label(egui::RichText::new(*label).size(12.0).color(M_SOFT));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        egui::RichText::new(skin::hex_of(k.colors[*i]))
                                            .font(mono(10.5))
                                            .color(M_DIM),
                                    );
                                });
                            });
                        }
                    });
                }
                let mut digits = None;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(egui::RichText::new("Digits").size(12.0).color(M_SOFT));
                    ui.add_space(4.0);
                    for d in Digits::ALL {
                        let on = k.digits == d;
                        let font = sans(11.5);
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(text_w(ui, d.label(), &font) + 18.0, 24.0), Sense::click());
                        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                        let fill = if on {
                            Color32::from_rgb(0x1f, 0x34, 0x38)
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect_filled(rect, 5.0, fill);
                        let edge = if on { M_ACCENT } else { M_EDGE2 };
                        ui.painter()
                            .rect_stroke(rect, 5.0, Stroke::new(1.0, edge), StrokeKind::Inside);
                        ui.painter()
                            .text(rect.center(), Align2::CENTER_CENTER, d.label(), font, M_TEXT);
                        if resp.clicked() && !on {
                            digits = Some(d);
                        }
                    }
                });
                if let Some((i, c)) = change {
                    self.edit_skin(|s| s.colors[i] = c);
                }
                if let Some(d) = digits {
                    self.edit_skin(|s| s.digits = d);
                }
            });
    }

    /// Change the current skin; a built-in one is first copied to a new custom skin.
    fn edit_skin(&mut self, f: impl FnOnce(&mut Skin)) {
        let dir = self.dir.join("skins");
        let cur = self.skin().clone();
        let id = if cur.builtin() {
            let mut n = cur.clone();
            n.name = format!("{} (mine)", cur.name);
            n.author = "you".into();
            n.id = skin::free_id(&dir, &n.name, &self.skins);
            let id = n.id.clone();
            self.skins.push(n);
            self.settings.skin = id.clone();
            self.save_settings();
            id
        } else {
            cur.id
        };
        if let Some(s) = self.skins.iter_mut().find(|s| s.id == id) {
            f(s);
        }
        self.skin_dirty = Some(std::time::Instant::now());
    }

    fn import_skin(&mut self, path: &std::path::Path) {
        let stem = path
            .file_name()
            .map(|n| {
                n.to_string_lossy()
                    .trim_end_matches(".json")
                    .trim_end_matches(".skin")
                    .to_string()
            })
            .unwrap_or_default();
        let dir = self.dir.join("skins");
        let r = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| skin::Skin::from_json(&t, "", &stem))
            .and_then(|mut s| {
                s.id = skin::free_id(&dir, &s.name, &self.skins);
                skin::save(&dir, &s).map(|_| s)
            });
        self.skin_msg = Some(match r {
            Ok(s) => {
                let msg = format!("Loaded “{}”", s.name);
                self.settings.skin = s.id.clone();
                self.skins.push(s);
                self.save_settings();
                (true, msg)
            }
            Err(e) => (false, format!("Couldn’t read that skin: {e}")),
        });
        self.show_skins = true;
    }

    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        });
        for p in dropped {
            if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")) {
                self.import_skin(&p);
            }
        }
    }
}

// ---------------------------------------------------------------------- painted parts

fn digit_font(d: Digits, size: f32) -> FontId {
    match d {
        Digits::Mono => mono(size),
        _ => sans(size),
    }
}

fn display_content(ui: &mut Ui, k: &Skin, cur: &Cur) {
    let w = ui.available_width();
    // header: device name and connection
    let (h, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());
    let conn_font = mono(10.5);
    let conn_w = text_w(ui, &cur.conn, &conn_font);
    let name = elide(ui, &cur.name.to_uppercase(), &conn_font, w - conn_w - 10.0);
    let p = ui.painter().clone();
    p.text(
        h.left_center(),
        Align2::LEFT_CENTER,
        name,
        conn_font.clone(),
        k.c(LCD_INK),
    );
    p.text(
        h.right_center(),
        Align2::RIGHT_CENTER,
        &cur.conn,
        conn_font,
        k.c(LCD_DIM),
    );

    // big percentage and the three detail lines
    let (row, _) = ui.allocate_exact_size(vec2(w, 56.0), Sense::hover());
    let label = cur.pct.map_or_else(|| "--".to_string(), |p| format!("{p}%"));
    let anchor = row.left_bottom();
    match k.digits {
        Digits::Pixel => {
            pixel_text(&p, anchor, Align2::LEFT_BOTTOM, &label, 52.0, alpha(cur.color, 60));
            pixel_text(&p, anchor, Align2::LEFT_BOTTOM, &label, 50.0, cur.color);
        }
        d => {
            let font = digit_font(d, 60.0);
            let at = anchor + vec2(-2.0, 8.0);
            for o in [vec2(-1.5, 0.0), vec2(1.5, 0.0), vec2(0.0, -1.5), vec2(0.0, 1.5)] {
                p.text(at + o, Align2::LEFT_BOTTOM, &label, font.clone(), alpha(cur.color, 40));
            }
            // drawn twice, half a pixel apart, for a heavier weight
            p.text(at, Align2::LEFT_BOTTOM, &label, font.clone(), cur.color);
            p.text(at + vec2(0.7, 0.0), Align2::LEFT_BOTTOM, &label, font, cur.color);
        }
    }
    let f = mono(11.5);
    let right = row.right_bottom();
    p.text(right, Align2::RIGHT_BOTTOM, &cur.status, f.clone(), k.c(LCD_DIM));
    p.text(
        right - vec2(0.0, 16.0),
        Align2::RIGHT_BOTTOM,
        &cur.drain,
        f.clone(),
        k.c(LCD_DIM),
    );
    p.text(
        right - vec2(0.0, 32.0),
        Align2::RIGHT_BOTTOM,
        &cur.time_left,
        f,
        k.c(LCD_INK),
    );

    // chips
    if !cur.chips.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for c in &cur.chips {
                let font = mono(10.0);
                let (r, _) = ui.allocate_exact_size(vec2(text_w(ui, c, &font) + 12.0, 17.0), Sense::hover());
                ui.painter()
                    .rect_stroke(r, 4.0, Stroke::new(1.0, k.c(LCD_DIM)), StrokeKind::Inside);
                ui.painter()
                    .text(r.center(), Align2::CENTER_CENTER, c, font, k.c(LCD_INK));
            }
        });
    }
}

fn gauge(ui: &mut Ui, rect: Rect, k: &Skin, cur: &Cur) {
    let p = ui.painter();
    lcd_panel(p, rect, 10.0, k);
    let inner = rect.shrink(6.0);
    let gap = 3.0;
    let seg_h = (inner.height() - gap * 9.0) / 10.0;
    let lit = cur.pct.map_or(0, |v| ((v as u32 + 5) / 10) as usize);
    for i in 0..10 {
        let y = inner.bottom() - (i as f32 + 1.0) * seg_h - i as f32 * gap;
        let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), seg_h));
        if i < lit {
            p.rect_filled(r.expand(1.5), 3.0, alpha(cur.color, 50));
            p.rect_filled(r, 2.0, cur.color);
        } else {
            p.rect_filled(r, 2.0, k.c(LO));
        }
    }
    let y = inner.bottom() - inner.height() * cur.alert as f32 / 100.0;
    let line = Rect::from_min_size(pos2(rect.left() + 2.0, y - 1.0), vec2(rect.width() - 4.0, 2.0));
    p.rect_filled(line.expand(1.5), 2.0, alpha(k.c(BAD), 70));
    p.rect_filled(line, 1.0, k.c(BAD));
    ui.interact(line.expand(3.0), ui.scope_id().with("gauge-alert"), Sense::hover())
        .on_hover_text(format!("Alert at {}%", cur.alert));
}

fn history_strip(ui: &mut Ui, k: &Skin, cur: &Cur) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
    let p = ui.painter();
    lcd_panel(p, rect, 8.0, k);
    let inner = Rect::from_min_max(rect.min + vec2(6.0, 5.0), rect.max - vec2(6.0, 5.0));
    let gap = 2.0;
    let bw = (inner.width() - gap * (BARS as f32 - 1.0)) / BARS as f32;
    for (i, v) in cur.bars.iter().enumerate() {
        let Some(v) = v else { continue };
        let h = (inner.height() * *v as f32 / 100.0).max(1.0);
        let x = inner.left() + i as f32 * (bw + gap);
        let r = Rect::from_min_max(pos2(x, inner.bottom() - h), pos2(x + bw, inner.bottom()));
        p.rect_filled(r, 1.5, alpha(k.level(*v, cur.alert), 230));
    }
    let y = inner.bottom() - inner.height() * cur.alert as f32 / 100.0;
    p.extend(Shape::dashed_line(
        &[pos2(inner.left(), y), pos2(inner.right(), y)],
        Stroke::new(1.0, alpha(k.c(BAD), 150)),
        3.0,
        3.0,
    ));
    if cur.bars.iter().all(Option::is_none) {
        p.text(
            inner.center(),
            Align2::CENTER_CENTER,
            "history fills in as readings arrive",
            sans(10.5),
            k.c(LCD_DIM),
        );
    }
    resp.on_hover_text("Battery over the last 24 hours, one bar per hour");
}

/// 5×7 pixel glyphs for the "Pixel" digit style, so no font file is needed.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '%' => [0x19, 0x1A, 0x02, 0x04, 0x08, 0x0B, 0x13],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        _ => return None,
    })
}

fn pixel_text(p: &Painter, anchor: Pos2, align: Align2, s: &str, height: f32, color: Color32) {
    let cell = height / 7.0;
    let n = s.chars().filter(|c| glyph(*c).is_some()).count() as f32;
    let size = vec2((n * 6.0 - 1.0).max(0.0) * cell, height);
    let origin = align.anchor_size(anchor, size).min;
    let dot = cell * 0.86;
    for (gi, g) in s.chars().filter_map(glyph).enumerate() {
        for (y, bits) in g.iter().enumerate() {
            for x in 0..5 {
                if bits & (0x10 >> x) != 0 {
                    let o = origin + vec2((gi as f32 * 6.0 + x as f32) * cell, y as f32 * cell);
                    p.rect_filled(Rect::from_min_size(o, vec2(dot, dot)), 0.0, color);
                }
            }
        }
    }
}
