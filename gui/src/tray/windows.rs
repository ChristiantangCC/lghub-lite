//! Windows tray icon (tray-icon crate): Open / Skins… / Quit menu, left click opens.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::{TITLE, TrayFlags, icons};

pub struct Tray(TrayIcon);

impl Tray {
    pub fn set_tooltip(&self, tip: &str) {
        let _ = self.0.set_tooltip(Some(tip));
    }

    /// The two-bar battery icon for the first two devices.
    pub fn set_levels(&self, top: Option<icons::Level>, bottom: Option<icons::Level>) {
        let small = crate::os::icon_sizes().0;
        let _ = self
            .0
            .set_icon(tray_icon::Icon::from_rgba(icons::tray(small, top, bottom), small, small).ok());
    }
}

pub fn build(ctx: &egui::Context, flags: Arc<TrayFlags>) -> Option<Tray> {
    let open = MenuItem::new("Open", true, None);
    let skins = MenuItem::new("Skins…", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::with_items(&[&open, &skins, &quit]).ok()?;
    let small = crate::os::icon_sizes().0;
    let icon = tray_icon::Icon::from_rgba(icons::tray(small, None, None), small, small).ok()?;
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
    Some(Tray(tray))
}
