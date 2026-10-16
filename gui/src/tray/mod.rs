//! The tray icon: tray-icon on Windows and macOS (menu bar), a StatusNotifierItem (ksni) on
//! Linux.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(windows, target_os = "macos"))]
mod native;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(any(windows, target_os = "macos"))]
pub use native::*;
