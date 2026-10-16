//! macOS helpers: reaching an already running instance (the shared Unix socket), file dialogs
//! through AppleScript, icon sizes. Window and Dock icons come from winit/eframe.

use std::path::PathBuf;
use std::process::{Command, Stdio};

pub use super::unix::{focus_existing, serve_show_requests};

/// The Dock icon is set through eframe (`ViewportCommand::Icon`); nothing extra to do.
pub fn set_window_icons(_hwnd: isize, _small: &[u8], _ss: u32, _big: &[u8], _bs: u32) {}

/// Windows-only fix for a winit quirk; nothing to do here.
pub fn full_client_area(_hwnd: isize) {}

/// Quote `s` as an AppleScript string literal.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Pick a skin file to open, or a place to save one, with the standard macOS panels
/// (`osascript`, part of every macOS). None if cancelled.
pub fn skin_file_dialog(save: bool, name: &str) -> Option<PathBuf> {
    let script = if save {
        format!(
            "POSIX path of (choose file name with prompt \"Skin file\" default name {})",
            applescript_string(name)
        )
    } else {
        "POSIX path of (choose file with prompt \"Skin file\" of type {\"public.json\"})".to_string()
    };
    let out = Command::new("osascript")
        .args(["-e", &script])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
}

/// (menu bar, Dock) icon sizes in pixels: the menu bar item is drawn at 2x for Retina
/// screens and scaled to the bar's height; the Dock wants a large image.
pub fn icon_sizes() -> (u32, u32) {
    (44, 256)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_quoting() {
        assert_eq!(applescript_string(r#"my "skin".json"#), r#""my \"skin\".json""#);
        assert_eq!(applescript_string(r"a\b"), r#""a\\b""#);
    }
}
