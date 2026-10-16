//! lghub-lite-bg: the windowless `watch` that `lghub-lite autostart on` registers for login.
//! Output goes to watch.log next to the preset file (moved to watch.log.old at start-up once
//! it passes 1 MB). Only one copy runs per user session.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::fs::OpenOptions;
use std::path::PathBuf;

use lghub_lite::{config, hidpp, platform, watch};

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

    // Single instance per session; the lock is held until the process exits.
    let Some(_lock) = platform::instance_lock("bg") else {
        return;
    };

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
    // Route println!/eprintln! into the log; `file` stays open until main returns.
    platform::redirect_stdio(&file);
    println!("lghub-lite-bg {} started", env!("CARGO_PKG_VERSION"));
    if let Err(e) = watch::run(hidpp::WATCH_SWID, false, &cfg) {
        println!("error: {e}");
    }
    drop(file);
}
