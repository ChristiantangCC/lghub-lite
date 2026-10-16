//! Linux tray icon: a StatusNotifierItem over D-Bus (ksni), shown by KDE, XFCE, Cinnamon,
//! MATE, LXQt and GNOME with the AppIndicator extension. No GTK needed.
//! [INF] Written without a Linux desktop to try it on; built and type-checked only.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;

use crate::{TITLE, TrayFlags, icons};

struct Sni {
    ctx: egui::Context,
    flags: Arc<TrayFlags>,
    /// Device lines for the tooltip.
    tip: String,
    icons: Vec<ksni::Icon>,
}

impl Sni {
    fn raise(&self, flag: &AtomicBool) {
        flag.store(true, Ordering::Relaxed);
        self.ctx.request_repaint();
    }
}

impl ksni::Tray for Sni {
    fn id(&self) -> String {
        "lghub-lite".into()
    }

    fn title(&self) -> String {
        TITLE.into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: TITLE.into(),
            description: self.tip.clone(),
            ..Default::default()
        }
    }

    /// Left click: show the window.
    fn activate(&mut self, _x: i32, _y: i32) {
        self.raise(&self.flags.show);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let item = |label: &str, f: fn(&mut Sni)| {
            StandardItem {
                label: label.into(),
                activate: Box::new(f),
                ..Default::default()
            }
            .into()
        };
        vec![
            item("Open", |t| t.raise(&t.flags.show)),
            item("Skins…", |t| t.raise(&t.flags.skins)),
            ksni::MenuItem::Separator,
            item("Quit", |t| t.raise(&t.flags.quit)),
        ]
    }
}

/// The tray's battery image in the sizes panels commonly ask for, as ARGB32.
fn pixmaps(top: Option<icons::Level>, bottom: Option<icons::Level>) -> Vec<ksni::Icon> {
    [16u32, 32, 48]
        .into_iter()
        .map(|s| {
            let mut data = icons::tray(s, top, bottom);
            for px in data.chunks_mut(4) {
                px.rotate_right(1); // RGBA -> ARGB
            }
            ksni::Icon {
                width: s as i32,
                height: s as i32,
                data,
            }
        })
        .collect()
}

pub struct Tray(Handle<Sni>);

/// Start the tray service; None when no StatusNotifierItem host is running (the app then
/// quits when its window is closed, like on a desktop without a tray).
pub fn build(ctx: &egui::Context, flags: Arc<TrayFlags>) -> Option<Tray> {
    let sni = Sni {
        ctx: ctx.clone(),
        flags,
        tip: String::new(),
        icons: pixmaps(None, None),
    };
    match sni.spawn() {
        Ok(h) => Some(Tray(h)),
        Err(e) => {
            eprintln!("no tray icon: {e}");
            None
        }
    }
}

impl Tray {
    pub fn set_tooltip(&self, tip: &str) {
        // the first line is the app name, already the tooltip title
        let lines = tip.split_once('\n').map_or("", |x| x.1).to_string();
        self.0.update(|t| t.tip = lines);
    }

    /// The two-bar battery icon for the first two devices.
    pub fn set_levels(&self, top: Option<icons::Level>, bottom: Option<icons::Level>) {
        let icons = pixmaps(top, bottom);
        self.0.update(|t| t.icons = icons);
    }
}
