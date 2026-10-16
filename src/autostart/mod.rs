//! Start the background watcher (or the app) at login, and find/stop running copies.
//! Windows: the per-user Run registry value. Linux: an XDG autostart .desktop file.
//! macOS: a per-user LaunchAgent.
//!
//! API (both OSes): `enable(command)`, `disable()`, `status()`, `start(command)`,
//! `stop_running(exe_name)`. `command` is a command line with quoted paths.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(windows)]
pub use windows::*;
