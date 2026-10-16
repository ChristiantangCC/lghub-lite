//! Linux and macOS: a second launch asks the running app to show its window over a Unix
//! socket (both instances share the per-user runtime folder).

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

/// Socket the running app listens on for "show" requests from a second launch.
/// `$XDG_RUNTIME_DIR` on Linux; on macOS `$TMPDIR`, which is per user too.
fn socket_path() -> Option<PathBuf> {
    let runtime = if cfg!(target_os = "macos") {
        "TMPDIR"
    } else {
        "XDG_RUNTIME_DIR"
    };
    let dir = std::env::var_os(runtime).map(PathBuf::from)?;
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
