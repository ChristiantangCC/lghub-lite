//! `watch`: long-running, event-driven monitor. The main thread blocks in a single
//! WaitForMultipleObjects on every open HID++ collection plus the PnP notification event,
//! so it uses no CPU while nothing happens. The one exception: headsets that report battery
//! only as a voltage (0x1F20) are read every BATTERY_POLL, because no unsolicited 0x1F20
//! event has been seen on hardware (G HUB polls it too). Battery drains over hours, so a
//! 5-minute read costs nothing measurable.
//! Whenever a device attaches (start-up, plug-in, receiver link-up, wake from sleep) its preset
//! from the preset file is applied; the file is re-read each time, so edits need no restart.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::INFINITE;

/// Local wall-clock time for log lines, e.g. `2026-10-15 22:31:02`.
fn now() -> String {
    let mut t: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// `println!` with a timestamp, so watch output (and the background log) is a history.
macro_rules! say {
    ($($t:tt)*) => { println!("{} {}", now(), format_args!($($t)*)) };
}

use crate::audio::{self, Adc, Curve};
use crate::config;
use crate::device::{self, EVENT_FEATURES, Event};
use crate::hid::{self, Notifier, Wake};
use crate::hidpp::{self, DIRECT, Error, GET_REGISTER, Hidpp};

/// One HID++ device on an interface: the direct device (index 0xFF) or a receiver slot.
struct Dev {
    index: u8,
    /// Feature index -> ID for the features whose events we decode.
    feats: Vec<(u8, u16)>,
    /// Battery reported only as a voltage (0x1F20); read on a timer.
    has_voltage: bool,
    curve: Option<Curve>,
    /// What was last printed for the battery, to print only changes.
    last_battery: String,
}

const BATTERY_POLL: Duration = Duration::from_secs(300);

/// One open HID++ interface (a direct device or a receiver).
struct Live {
    key: String,
    pid: u16,
    product: String,
    h: Hidpp,
    devs: Vec<Dev>,
    presets: PathBuf,
}

impl Live {
    fn sel(&self, index: u8) -> String {
        if index == DIRECT {
            format!("{:04x}", self.pid)
        } else {
            format!("{:04x}:{index}", self.pid)
        }
    }
}

pub fn run(swid: u8, trace: bool, presets: &Path) -> Result<(), Error> {
    let notifier = Notifier::hid().map_err(|e| Error::Io(e.to_string()))?;
    let mut live: Vec<Live> = Vec::new();
    say!("presets    {}", presets.display());
    sync(&mut live, swid, trace, presets)?;
    say!("watching for device and setting changes (Ctrl+C to stop)");
    let mut next_poll = Instant::now() + BATTERY_POLL;
    loop {
        // Reports that arrived while a transaction was waiting for its reply.
        for l in live.iter_mut() {
            while let Some(r) = l.h.queued() {
                handle(l, &r, trace);
            }
        }
        // Map the wake-up to plain indices so the borrow of `live` ends here.
        enum Got {
            Report(usize, Vec<u8>),
            Gone(usize, std::io::Error),
            Pnp,
            Poll,
        }
        let polling = live.iter().any(|l| l.devs.iter().any(|d| d.has_voltage));
        let timeout = if polling {
            // < INFINITE, so a long wait can never be mistaken for "no timeout"
            let ms = next_poll.saturating_duration_since(Instant::now()).as_millis();
            ms.min(u32::MAX as u128 - 1) as u32
        } else {
            INFINITE
        };
        let got = {
            let mut hs = Vec::new();
            let mut owner = Vec::new();
            for (k, l) in live.iter_mut().enumerate() {
                for h in l.h.handles() {
                    hs.push(h);
                    owner.push(k);
                }
            }
            match hid::wait_any(&mut hs, Some(&notifier), timeout) {
                Wake::Report(i, r) => Got::Report(owner[i], r),
                Wake::Failed(i, e) => Got::Gone(owner[i], e),
                Wake::Extra => Got::Pnp,
                Wake::Timeout => Got::Poll,
            }
        };
        match got {
            Got::Report(k, r) => handle(&mut live[k], &r, trace),
            Got::Gone(k, e) => {
                let l = live.remove(k);
                say!("- {:04x}  {}  removed ({e})", l.pid, l.product);
            }
            Got::Pnp => {
                // One plug-in raises several interface notifications; let them settle, then
                // re-enumerate once.
                notifier.settle(300);
                sync(&mut live, swid, trace, presets)?;
            }
            Got::Poll => {
                next_poll = Instant::now() + BATTERY_POLL;
                for l in live.iter_mut() {
                    poll_battery(l);
                }
            }
        }
    }
}

/// Re-enumerate HID++ interfaces: drop the ones that disappeared, open and describe new ones.
fn sync(live: &mut Vec<Live>, swid: u8, trace: bool, presets: &Path) -> Result<(), Error> {
    let ifs = device::interfaces()?;
    let keys: Vec<String> = ifs.iter().map(|f| device::group_key(&f.cols[0].path)).collect();
    live.retain(|l| {
        let keep = keys.contains(&l.key);
        if !keep {
            say!("- {:04x}  {}  removed", l.pid, l.product);
        }
        keep
    });
    for (f, key) in ifs.iter().zip(keys) {
        if live.iter().any(|l| l.key == key) {
            continue;
        }
        match Hidpp::open(&f.cols, swid, trace) {
            Ok(h) => {
                let mut l = Live {
                    key,
                    pid: f.pid,
                    product: f.product.clone(),
                    h,
                    devs: Vec::new(),
                    presets: presets.to_path_buf(),
                };
                describe(&mut l);
                live.push(l);
            }
            Err(e) => say!("! {:04x}  {}  cannot open: {e}", f.pid, f.product),
        }
    }
    Ok(())
}

/// Identify a newly opened interface; for receivers, attach every online paired device.
fn describe(l: &mut Live) {
    match l.h.ping(DIRECT, 0x5A) {
        Ok(_) => attach(l, DIRECT),
        Err(Error::Hpp10(_)) => {
            // HID++ 1.0 answer to a 2.0 ping: a receiver. [OBS §9.6]
            say!("+ {:04x}  {}  (receiver)", l.pid, l.product);
            // Notification flags (register 0x00) are only read, never written. [OBS §9.8]
            match l.h.read_register(DIRECT, GET_REGISTER, 0x00, [0, 0, 0], &[]) {
                Ok(p) => say!("  notification flags (reg 0x00): {}", hidpp::hex(&p[..3.min(p.len())])),
                Err(e) => say!("  notification flags (reg 0x00): {e}"),
            }
            for slot in 0..6u8 {
                if device::pairing(&mut l.h, slot).is_ok() {
                    attach(l, slot + 1);
                }
            }
        }
        Err(e) => say!("+ {:04x}  {}  [no HID++ 2.0 answer: {e}]", l.pid, l.product),
    }
}

/// Ping a device, print its name and battery, and learn the indices of the event features.
fn attach(l: &mut Live, index: u8) {
    let sel = l.sel(index);
    let (maj, min) = match l.h.ping(index, 0x5A) {
        Ok(v) => v,
        // [HW] the receiver answers 0x8F code 0x08 at once for a paired device whose link is down.
        Err(Error::Hpp10(0x08)) => {
            say!("  {sel:<8} offline (paired, not connected)");
            return;
        }
        Err(e) => {
            say!("  {sel:<8} offline ({e})");
            return;
        }
    };
    let name = device::name(&mut l.h, index).unwrap_or_else(|_| l.product.clone());
    let mut feats = Vec::new();
    for id in EVENT_FEATURES {
        if let Ok((i, _, _)) = l.h.get_feature(index, id)
            && i != 0
        {
            feats.push((i, id));
        }
    }
    let has_voltage = feats.iter().any(|f| f.1 == 0x1F20);
    let mut dev = Dev {
        index,
        feats,
        has_voltage,
        curve: None,
        last_battery: String::new(),
    };
    let battery = if has_voltage {
        let dir = l.presets.parent().map(Path::to_path_buf).unwrap_or_default();
        dev.curve = device::model_id(&mut l.h, index)
            .ok()
            .and_then(|m| audio::find_curve(&dir, m));
        match audio::adc(&mut l.h, index) {
            Ok(a) => {
                dev.last_battery = battery_key(&a, dev.curve.as_ref());
                format!("  battery {}", audio::voltage_text(&a, dev.curve.as_ref()))
            }
            Err(e) => format!("  battery unreadable: {e}"),
        }
    } else {
        device::battery(&mut l.h, index)
            .map(|b| format!("  battery {b}"))
            .unwrap_or_default()
    };
    say!("+ {sel:<8} {name}  (HID++ {maj}.{min}){battery}");
    l.devs.retain(|d| d.index != index);
    l.devs.push(dev);
    apply_preset(l, index);
}

/// Apply this device's preset, if any. Errors are reported, never fatal: `watch` keeps running.
fn apply_preset(l: &mut Live, index: u8) {
    let sel = l.sel(index);
    let list = match config::load(&l.presets) {
        Ok(v) => v,
        Err(e) => {
            say!("  {sel:<8} presets not loaded: {e}");
            return;
        }
    };
    if list.is_empty() {
        return;
    }
    let model = match device::model_id(&mut l.h, index) {
        Ok(m) => m,
        Err(e) => {
            say!("  {sel:<8} model ID unknown, preset not applied: {e}");
            return;
        }
    };
    let Some(p) = list.iter().find(|p| p.model == model) else {
        return;
    };
    match config::apply(&mut l.h, index, p) {
        Ok(s) => say!("  {sel:<8} preset applied: {s}"),
        Err(e) => say!("  {sel:<8} preset failed: {e}"),
    }
}

fn handle(l: &mut Live, r: &[u8], trace: bool) {
    if r.len() < 4 {
        return;
    }
    let feats: &[(u8, u16)] = l.devs.iter().find(|d| d.index == r[1]).map_or(&[], |d| &d.feats);
    let Some(ev) = device::decode_event(r, feats) else {
        if trace {
            eprintln!("<- {}  (not an event we decode)", hidpp::hex(r));
        }
        return;
    };
    if trace {
        eprintln!("<- {}", hidpp::hex(r));
    }
    match ev {
        Event::Connection {
            index, online: true, ..
        } => {
            say!("  {:<8} {ev}", l.sel(index));
            attach(l, index);
        }
        Event::Disconnected { index } => {
            say!("  {:<8} {ev}", l.sel(index));
            l.devs.retain(|d| d.index != index);
        }
        Event::Voltage { mv, flags } => report_voltage(l, r[1], &Adc { mv, flags }),
        _ => say!("  {:<8} {ev}", l.sel(r[1])),
    }
}

/// Change key for battery printing: the percentage with a curve, else 10 mV steps.
fn battery_key(a: &Adc, curve: Option<&Curve>) -> String {
    match curve {
        Some(c) => format!("{}%", c.percent(a.mv)),
        None => format!("{}", a.mv / 10),
    }
}

/// Print a voltage reading if it changed what the user sees.
fn report_voltage(l: &mut Live, index: u8, a: &Adc) {
    let sel = l.sel(index);
    let Some(d) = l.devs.iter_mut().find(|d| d.index == index) else {
        return;
    };
    let key = battery_key(a, d.curve.as_ref());
    if key != d.last_battery {
        d.last_battery = key;
        say!("  {sel:<8} battery {}", audio::voltage_text(a, d.curve.as_ref()));
    }
}

/// Timer read for voltage-only batteries. A failed read (e.g. headset switched off) is only
/// shown with --verbose; watch keeps running.
fn poll_battery(l: &mut Live) {
    let idx: Vec<u8> = l.devs.iter().filter(|d| d.has_voltage).map(|d| d.index).collect();
    for i in idx {
        match audio::adc(&mut l.h, i) {
            Ok(a) => report_voltage(l, i, &a),
            Err(e) => crate::verbose(format_args!("battery read failed: {e}")),
        }
    }
}
