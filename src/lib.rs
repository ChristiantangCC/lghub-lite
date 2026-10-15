//! lghub-lite: talks HID++ directly to Logitech mice and headsets on Windows.
//! Shared by the `lghub-lite` CLI and the windowless `lghub-lite-bg` watcher.
//! Not affiliated with or endorsed by Logitech. Protocol evidence: NOTES.md.

pub mod audio;
pub mod autostart;
pub mod battery;
pub mod config;
pub mod device;
pub mod hid;
pub mod hidpp;
pub mod profiles;
pub mod watch;

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn verbose(args: std::fmt::Arguments) {
    if VERBOSE.load(Ordering::Relaxed) {
        eprintln!("{args}");
    }
}

pub const LOGITECH_VID: u16 = 0x046D;
