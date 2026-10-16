//! Start the background watcher (or the app) at login, and find/stop running copies.
//! Windows: the per-user Run registry value. Linux: an XDG autostart .desktop file.
//!
//! API (both OSes): `enable(command)`, `disable()`, `status()`, `start(command)`,
//! `stop_running(exe_name)`. `command` is a command line with quoted paths.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(windows)]
pub use windows::*;
