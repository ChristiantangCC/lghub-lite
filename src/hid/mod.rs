//! Thin HID layer: enumerate HID++ collections, open them, read/write reports with timeouts,
//! and wait on several devices plus other signals at once. One API, a backend per OS:
//! Windows (SetupDi/HidD, overlapped I/O), Linux (hidraw, poll) and macOS (IOHIDDevice, poll).

use std::io;

pub mod descriptor;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Handle, Notifier, RawWaitable, Waker, enumerate, wait_any};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Handle, Notifier, RawWaitable, Waker, enumerate, wait_any};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Handle, Notifier, RawWaitable, Waker, enumerate, wait_any};

/// One HID top-level collection. Windows exposes each collection as its own device path;
/// on Linux (a hidraw node) and macOS (an IOHIDDevice) one path carries all collections of an
/// interface, so several `Info`s share a `path`.
#[derive(Debug, Clone)]
pub struct Info {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub product: String,
    /// The same for all collections of one physical interface (grouping key).
    pub iface: String,
}

/// "No timeout" for `wait_any` (the same value as Win32 INFINITE).
pub const FOREVER: u32 = u32::MAX;

pub enum Wake {
    /// Input report from hs[i].
    Report(usize, Vec<u8>),
    /// Reading hs[i] failed, e.g. the device was unplugged.
    Failed(usize, io::Error),
    /// extras[i] was signalled.
    Extra(usize),
    Timeout,
}

/// Something `wait_any` can wait on besides device reads (PnP notifier, cross-thread waker):
/// a Win32 event handle on Windows, a pollable file descriptor on Linux and macOS.
pub trait Signal {
    fn raw(&self) -> RawWaitable;
    /// Reset after a wake-up. Windows events reset themselves; a Linux eventfd or socket, or a
    /// macOS pipe, stays readable until read.
    fn consume(&self) {}
}

impl Signal for Waker {
    fn raw(&self) -> RawWaitable {
        self.raw_waitable()
    }
    #[cfg(unix)]
    fn consume(&self) {
        Waker::consume(self)
    }
}

impl Signal for Notifier {
    fn raw(&self) -> RawWaitable {
        self.raw_waitable()
    }
    #[cfg(unix)]
    fn consume(&self) {
        Notifier::consume(self)
    }
}

/// Wait up to `timeout_ms` for an input report on any of `hs`.
/// Returns Ok(None) on timeout, else (index into hs, report bytes).
pub fn read_any(hs: &mut [&mut Handle], timeout_ms: u32) -> io::Result<Option<(usize, Vec<u8>)>> {
    match wait_any(hs, &[], timeout_ms) {
        Wake::Report(i, r) => Ok(Some((i, r))),
        Wake::Failed(_, e) => Err(e),
        Wake::Extra(_) | Wake::Timeout => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waker_wakes_wait_any_from_another_thread() {
        let w = std::sync::Arc::new(Waker::new().unwrap());
        let w2 = w.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            w2.wake();
        });
        let start = std::time::Instant::now();
        assert!(matches!(wait_any(&mut [], &[w.as_ref()], 5000), Wake::Extra(0)));
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        t.join().unwrap();
        // one wake releases one wait: nothing pending now
        assert!(matches!(wait_any(&mut [], &[w.as_ref()], 10), Wake::Timeout));
    }
}
