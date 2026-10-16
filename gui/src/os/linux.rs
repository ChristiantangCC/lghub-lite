//! Linux helpers: reaching an already running instance, file dialogs via zenity/kdialog,
//! window icons through winit, icon sizes.
//! [INF] Written without a Linux desktop to try it on; built and type-checked only.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Socket the running app listens on for "show" requests from a second launch.
fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    Some(dir.join("lghub-lite-gui.sock"))
}

/// A second launch: ask the running app to show its window.
pub fn focus_existing(_title: &str) {
    if let Some(mut s) = socket_path().and_then(|p| UnixStream::connect(p).ok()) {
        let _ = s.write_all(b"show\n");
    }
}

/// The running app: call `show` whenever another launch asks for the window.
pub fn serve_show_requests(show: Box<dyn Fn() + Send>) {
    let Some(p) = socket_path() else { return };
    let _ = std::fs::remove_file(&p); // stale socket from a crash; we hold the instance lock
    let Ok(listener) = UnixListener::bind(&p) else { return };
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let mut buf = [0u8; 16];
            if conn.read(&mut buf).is_ok_and(|n| buf[..n].starts_with(b"show")) {
                show();
            }
        }
    });
}

/// Window icons are set through winit (`ViewportCommand::Icon`) on Linux; Wayland
/// compositors take the icon from the .desktop file instead.
pub fn set_window_icons(_hwnd: isize, _small: &[u8], _ss: u32, _big: &[u8], _bs: u32) {}

/// Windows-only fix for a winit quirk; nothing to do here.
pub fn full_client_area(_hwnd: isize) {}

/// Pick a skin file to open, or a place to save one, with zenity (GNOME, most desktops) or
/// kdialog (KDE). None if cancelled or neither is installed; dropping a file on the window
/// works everywhere.
pub fn skin_file_dialog(save: bool, name: &str) -> Option<PathBuf> {
    let zenity = {
        let mut c = Command::new("zenity");
        c.args([
            "--file-selection",
            "--title=Skin file",
            "--file-filter=Skin files | *.json",
        ]);
        if save {
            c.args(["--save", "--confirm-overwrite", &format!("--filename={name}")]);
        }
        c
    };
    let kdialog = {
        let mut c = Command::new("kdialog");
        let start = if save { name } else { "." };
        c.args([
            if save { "--getsavefilename" } else { "--getopenfilename" },
            start,
            "*.json",
        ]);
        c
    };
    for mut c in [zenity, kdialog] {
        let Ok(out) = c.stderr(Stdio::null()).output() else {
            continue; // not installed: try the next one
        };
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return (out.status.success() && !path.is_empty()).then(|| PathBuf::from(path));
    }
    None
}

/// (tray, window) icon sizes. The tray gets several sizes (see tray/linux.rs); the window
/// icon is scaled by the desktop.
pub fn icon_sizes() -> (u32, u32) {
    (32, 64)
}
