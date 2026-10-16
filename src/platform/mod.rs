//! Small OS services: local time, the settings folder, single-instance locks, routing stdout
//! to a log file, opening a folder in the file manager, executable names.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(windows)]
pub use windows::*;

/// Local wall-clock time `[year, month, day, hour, minute, second]`.
pub fn local_time_text() -> String {
    let [y, mo, d, h, mi, s] = local_time();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// `lghub-lite-bg` → `lghub-lite-bg.exe` on Windows, unchanged elsewhere.
pub fn exe(base: &str) -> String {
    format!("{base}{}", std::env::consts::EXE_SUFFIX)
}
