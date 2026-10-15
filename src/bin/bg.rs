//! lghub-lite-bg: the windowless `watch` that `lghub-lite autostart on` registers for login.
//! Output goes to watch.log next to the preset file (moved to watch.log.old at start-up once
//! it passes 1 MB). Only one copy runs per user session.
#![windows_subsystem = "windows"]

use std::fs::OpenOptions;
use std::os::windows::io::AsRawHandle;
use std::path::PathBuf;
use std::ptr::null;

use lghub_lite::{config, hidpp, watch};
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
use windows_sys::Win32::System::Threading::CreateMutexW;

const LOG_MAX: u64 = 1 << 20;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut cfg: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        if a == "--config" {
            cfg = args.next().map(PathBuf::from);
        }
    }
    let Some(cfg) = cfg.or_else(config::default_path) else {
        std::process::exit(2);
    };

    // Single instance per session. The handle stays open until the process exits.
    let name: Vec<u16> = "Local\\lghub-lite-bg".encode_utf16().chain(Some(0)).collect();
    // SAFETY: NUL-terminated name, default security.
    let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
    // SAFETY: plain thread-local error query right after the call.
    if !mutex.is_null() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }

    let dir = cfg.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if std::fs::create_dir_all(&dir).is_err() {
        std::process::exit(1);
    }
    let log = dir.join("watch.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = std::fs::rename(&log, dir.join("watch.log.old"));
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&log) else {
        std::process::exit(1);
    };
    // Route println!/eprintln! into the log: std looks the handle up on every write.
    // SAFETY: `file` stays alive (and its handle open) until main returns.
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, file.as_raw_handle());
        SetStdHandle(STD_ERROR_HANDLE, file.as_raw_handle());
    }
    println!("lghub-lite-bg {} started", env!("CARGO_PKG_VERSION"));
    if let Err(e) = watch::run(hidpp::DEFAULT_SWID, false, &cfg) {
        println!("error: {e}");
    }
    drop(file);
}
