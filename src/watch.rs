//! `watch`: long-running, event-driven monitor. `Monitor::next` blocks in a single
//! WaitForMultipleObjects on every open HID++ collection plus the PnP notification event (and
//! an optional cross-thread `Waker`), so it uses no CPU while nothing happens. The one
//! exception: headsets that report battery only as a voltage (0x1F20) are read every
//! BATTERY_POLL, because no unsolicited 0x1F20 event has been seen on hardware (G HUB polls it
//! too). Battery drains over hours, so a 5-minute read costs nothing measurable.
//! Whenever a device attaches (start-up, plug-in, receiver link-up, wake from sleep) its preset
//! from the preset file is applied; the file is re-read each time, so edits need no restart.
//!
//! The CLI (`run`) prints each `Update`; the GUI consumes the same updates as data and sends
//! commands through `Monitor::exec`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::INFINITE;

use crate::audio::{self, Adc};
use crate::battery::{self, Curve};
use crate::config::{self, Preset};
use crate::device::{self, Dpi, EVENT_FEATURES, Event};
use crate::hid::{self, Notifier, Signal, Wake, Waker};
use crate::hidpp::{self, DIRECT, Error, GET_REGISTER, Hidpp};

/// Local wall-clock time for log lines, e.g. `2026-10-15 22:31:02`.
pub fn now() -> String {
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

/// Something that happened, as data. `Display` gives the CLI's `watch` line.
#[derive(Debug, Clone)]
pub enum Update {
    /// Informational line (receiver flags, open errors, ...).
    Line(String),
    Receiver {
        pid: u16,
        product: String,
    },
    Added {
        sel: String,
        name: String,
        protocol: (u8, u8),
        /// Battery text as shown by `battery`, or "unreadable: ..."; None if no battery.
        battery: Option<String>,
        battery_percent: Option<u8>,
    },
    Offline {
        sel: String,
        reason: String,
    },
    /// An interface disappeared; `sels` are the devices that were on it.
    Removed {
        pid: u16,
        product: String,
        reason: Option<String>,
        sels: Vec<String>,
    },
    Event {
        sel: String,
        event: Event,
    },
    Battery {
        sel: String,
        text: String,
        percent: Option<u8>,
    },
    Preset {
        sel: String,
        result: Result<String, String>,
    },
}

impl fmt::Display for Update {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Update::Line(s) => f.write_str(s),
            Update::Receiver { pid, product } => write!(f, "+ {pid:04x}  {product}  (receiver)"),
            Update::Added {
                sel,
                name,
                protocol: (maj, min),
                battery,
                ..
            } => {
                write!(f, "+ {sel:<8} {name}  (HID++ {maj}.{min})")?;
                match battery {
                    Some(b) => write!(f, "  battery {b}"),
                    None => Ok(()),
                }
            }
            Update::Offline { sel, reason } => write!(f, "  {sel:<8} offline ({reason})"),
            Update::Removed {
                pid, product, reason, ..
            } => {
                write!(f, "- {pid:04x}  {product}  removed")?;
                match reason {
                    Some(r) => write!(f, " ({r})"),
                    None => Ok(()),
                }
            }
            Update::Event { sel, event } => write!(f, "  {sel:<8} {event}"),
            Update::Battery { sel, text, .. } => write!(f, "  {sel:<8} battery {text}"),
            Update::Preset { sel, result: Ok(s) } => write!(f, "  {sel:<8} preset applied: {s}"),
            Update::Preset { sel, result: Err(e) } => write!(f, "  {sel:<8} preset failed: {e}"),
        }
    }
}

/// Commands the GUI sends to the monitor's thread.
#[derive(Debug, Clone)]
pub enum Cmd {
    Refresh,
    SetDpi(u16),
    SetRate(u32),
    SetMode(u8),
    /// Build or refine the measured battery curve from the voltage recording.
    BuildCurve,
    /// Save the given values as this model's preset (None clears that field).
    SavePreset {
        dpi: Option<u16>,
        rate: Option<u32>,
    },
}

/// Everything the GUI shows for one device, read fresh from the hardware.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub sel: String,
    pub name: String,
    pub battery: Option<String>,
    pub battery_percent: Option<u8>,
    /// Battery voltage for devices that only report a voltage (0x1F20).
    pub voltage_mv: Option<u16>,
    /// A voltage-only battery whose percentage is only the generic estimate.
    pub needs_curve: bool,
    /// Where the percentage comes from (voltage-only batteries).
    pub curve_label: Option<String>,
    /// State of the voltage recording that measured curves are built from.
    pub recording: Option<String>,
    /// Hours left in tenths, when the curve knows the full runtime (voltage-only batteries).
    pub tenths_left: Option<u32>,
    pub dpi: Option<Dpi>,
    /// (current Hz, allowed Hz)
    pub rate: Option<(u32, Vec<u32>)>,
    pub mode: Option<u8>,
    pub model: Option<u16>,
    pub preset: Option<Preset>,
}

/// One HID++ device on an interface: the direct device (index 0xFF) or a receiver slot.
struct Dev {
    index: u8,
    name: String,
    /// Feature index -> ID for the features whose events we decode.
    feats: Vec<(u8, u16)>,
    /// Battery reported only as a voltage (0x1F20); read on a timer.
    has_voltage: bool,
    model: u16,
    /// Best curve for a voltage-only battery (always set for those).
    curve: Option<Curve>,
    /// Lowest voltage since the last charge; a clear rise above it means charging.
    low_mv: u16,
    /// What was last reported for the battery, to report only changes.
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
    /// Apply presets on attach (off when another watcher already does it).
    apply_presets: bool,
}

impl Live {
    /// Folder of the preset file, which also holds battery curves and recordings.
    fn dir(&self) -> PathBuf {
        self.presets.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    fn sel(&self, index: u8) -> String {
        if index == DIRECT {
            format!("{:04x}", self.pid)
        } else {
            format!("{:04x}:{index}", self.pid)
        }
    }
}

pub struct Monitor {
    notifier: Notifier,
    live: Vec<Live>,
    swid: u8,
    trace: bool,
    presets: PathBuf,
    next_poll: Instant,
    apply_presets: bool,
}

impl Monitor {
    /// Open every HID++ interface; the returned updates describe what was found.
    pub fn new(swid: u8, trace: bool, presets: &Path) -> Result<(Monitor, Vec<Update>), Error> {
        let notifier = Notifier::hid().map_err(|e| Error::Io(e.to_string()))?;
        let mut m = Monitor {
            notifier,
            live: Vec::new(),
            swid,
            trace,
            presets: presets.to_path_buf(),
            next_poll: Instant::now() + BATTERY_POLL,
            apply_presets: true,
        };
        let mut out = Vec::new();
        m.sync(&mut out)?;
        Ok((m, out))
    }

    /// Turn automatic preset application on attach on or off (on by default). The GUI turns it
    /// off while `lghub-lite-bg` is running, so only one process applies presets.
    pub fn set_apply_presets(&mut self, on: bool) {
        self.apply_presets = on;
        for l in self.live.iter_mut() {
            l.apply_presets = on;
        }
    }

    /// Selectors of all attached (online) devices.
    pub fn devices(&self) -> Vec<String> {
        self.live
            .iter()
            .flat_map(|l| l.devs.iter().map(|d| l.sel(d.index)))
            .collect()
    }

    /// Block until something happens and return what did. Returns an empty list when only
    /// `waker` fired (a command is waiting for the caller).
    pub fn next(&mut self, waker: Option<&Waker>) -> Result<Vec<Update>, Error> {
        let mut out = Vec::new();
        // Reports that arrived while a transaction was waiting for its reply.
        for l in self.live.iter_mut() {
            while let Some(r) = l.h.queued() {
                handle(l, &r, self.trace, &mut out);
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
        // Map the wake-up to plain indices so the borrow of `live` ends here.
        enum Got {
            Report(usize, Vec<u8>),
            Gone(usize, std::io::Error),
            Pnp,
            Woken,
            Poll,
        }
        let polling = self.live.iter().any(|l| l.devs.iter().any(|d| d.has_voltage));
        let timeout = if polling {
            // < INFINITE, so a long wait can never be mistaken for "no timeout"
            let ms = self.next_poll.saturating_duration_since(Instant::now()).as_millis();
            ms.min(u32::MAX as u128 - 1) as u32
        } else {
            INFINITE
        };
        let got = {
            let mut hs = Vec::new();
            let mut owner = Vec::new();
            for (k, l) in self.live.iter_mut().enumerate() {
                for h in l.h.handles() {
                    hs.push(h);
                    owner.push(k);
                }
            }
            let mut extras: Vec<&dyn Signal> = vec![&self.notifier];
            if let Some(w) = waker {
                extras.push(w);
            }
            match hid::wait_any(&mut hs, &extras, timeout) {
                Wake::Report(i, r) => Got::Report(owner[i], r),
                Wake::Failed(i, e) => Got::Gone(owner[i], e),
                Wake::Extra(0) => Got::Pnp,
                Wake::Extra(_) => Got::Woken,
                Wake::Timeout => Got::Poll,
            }
        };
        match got {
            Got::Report(k, r) => handle(&mut self.live[k], &r, self.trace, &mut out),
            Got::Gone(k, e) => {
                let l = self.live.remove(k);
                out.push(removed(&l, Some(e.to_string())));
            }
            Got::Pnp => {
                // One plug-in raises several interface notifications; let them settle, then
                // re-enumerate once.
                self.notifier.settle(300);
                self.sync(&mut out)?;
            }
            Got::Woken => {}
            Got::Poll => {
                self.next_poll = Instant::now() + BATTERY_POLL;
                for l in self.live.iter_mut() {
                    poll_battery(l, &mut out);
                }
            }
        }
        Ok(out)
    }

    /// Re-enumerate HID++ interfaces: drop the ones that disappeared, open and describe new ones.
    fn sync(&mut self, out: &mut Vec<Update>) -> Result<(), Error> {
        let ifs = device::interfaces()?;
        let keys: Vec<String> = ifs.iter().map(|f| device::group_key(&f.cols[0].path)).collect();
        let (keep, gone): (Vec<Live>, Vec<Live>) = std::mem::take(&mut self.live)
            .into_iter()
            .partition(|l| keys.contains(&l.key));
        self.live = keep;
        out.extend(gone.iter().map(|l| removed(l, None)));
        for (f, key) in ifs.iter().zip(keys) {
            if self.live.iter().any(|l| l.key == key) {
                continue;
            }
            match Hidpp::open(&f.cols, self.swid, self.trace) {
                Ok(h) => {
                    let mut l = Live {
                        key,
                        pid: f.pid,
                        product: f.product.clone(),
                        h,
                        devs: Vec::new(),
                        presets: self.presets.clone(),
                        apply_presets: self.apply_presets,
                    };
                    describe(&mut l, out);
                    self.live.push(l);
                }
                Err(e) => out.push(Update::Line(format!(
                    "! {:04x}  {}  cannot open: {e}",
                    f.pid, f.product
                ))),
            }
        }
        Ok(())
    }

    fn find(&mut self, sel: &str) -> Result<(&mut Live, u8), Error> {
        for l in self.live.iter_mut() {
            if let Some(d) = l.devs.iter().find(|d| l.sel(d.index) == sel) {
                let i = d.index;
                return Ok((l, i));
            }
        }
        Err(Error::Other(format!("device {sel} is not connected")))
    }

    /// Read the current state of one device.
    pub fn snapshot(&mut self, sel: &str) -> Result<Snapshot, Error> {
        let (l, i) = self.find(sel)?;
        let name = l
            .devs
            .iter()
            .find(|d| d.index == i)
            .map(|d| d.name.clone())
            .unwrap_or_default();
        // A curve may have been imported or built since the last look: look again.
        let dir = l.dir();
        let (mut needs_curve, mut curve_label, mut recording) = (false, None, None);
        if let Some(d) = l.devs.iter_mut().find(|d| d.index == i && d.has_voltage) {
            let c = battery::find_curve(&dir, d.model);
            needs_curve = c.is_approx();
            curve_label = Some(c.label());
            recording = Some(battery::recording_status(&dir, d.model));
            d.curve = Some(c);
        }
        let (battery, battery_percent, voltage_mv) = battery_now(l, i);
        let tenths_left = voltage_mv.and_then(|mv| {
            let d = l.devs.iter().find(|d| d.index == i)?;
            d.curve.as_ref()?.tenths_of_hours_left(mv)
        });
        let dpi = optional(device::dpi(&mut l.h, i))?;
        let rate = optional(device::report_rate(&mut l.h, i))?.map(|r| {
            let allowed = (0..8u8)
                .filter(|b| r.supported & (1 << b) != 0)
                .map(device::rate_hz)
                .collect();
            (device::rate_hz(r.current), allowed)
        });
        let mode = device::onboard_mode(&mut l.h, i).ok().flatten();
        let model = device::model_id(&mut l.h, i).ok();
        let preset = match model {
            Some(m) => config::load(&l.presets)
                .map_err(Error::Other)?
                .into_iter()
                .find(|p| p.model == m),
            None => None,
        };
        Ok(Snapshot {
            sel: sel.to_string(),
            name,
            battery,
            battery_percent,
            voltage_mv,
            needs_curve,
            curve_label,
            recording,
            tenths_left,
            dpi,
            rate,
            mode,
            model,
            preset,
        })
    }

    /// Run a GUI command against one device, then return its fresh state.
    pub fn exec(&mut self, sel: &str, cmd: Cmd) -> Result<Snapshot, Error> {
        {
            let (l, i) = self.find(sel)?;
            match &cmd {
                Cmd::Refresh => {}
                Cmd::BuildCurve => {
                    let dir = l.dir();
                    let d = l
                        .devs
                        .iter_mut()
                        .find(|d| d.index == i && d.has_voltage)
                        .ok_or_else(|| Error::Other("this device reports no battery voltage".into()))?;
                    match battery::update_measured(&dir, d.model).map_err(Error::Other)? {
                        Some(c) => d.curve = Some(c),
                        None => {
                            return Err(Error::Other(
                                "the measured curve already includes the latest recorded discharge".into(),
                            ));
                        }
                    }
                }
                Cmd::SetDpi(v) => {
                    device::set_dpi_verified(&mut l.h, i, *v)?;
                }
                Cmd::SetRate(hz) => {
                    device::set_rate_verified(&mut l.h, i, *hz)?;
                }
                Cmd::SetMode(m) => {
                    device::set_mode_verified(&mut l.h, i, *m)?;
                }
                Cmd::SavePreset { dpi, rate } => {
                    let model = device::model_id(&mut l.h, i)?;
                    let mut list = config::load(&l.presets).map_err(Error::Other)?;
                    list.retain(|p| p.model != model);
                    if dpi.is_some() || rate.is_some() {
                        list.push(Preset {
                            model,
                            dpi: *dpi,
                            rate: *rate,
                        });
                    }
                    config::save(&l.presets, &list).map_err(Error::Other)?;
                }
            }
        }
        self.snapshot(sel)
    }
}

/// NoFeature -> Ok(None): the device simply does not have that setting.
fn optional<T>(r: Result<T, Error>) -> Result<Option<T>, Error> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(Error::NoFeature(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn removed(l: &Live, reason: Option<String>) -> Update {
    Update::Removed {
        pid: l.pid,
        product: l.product.clone(),
        reason,
        sels: l.devs.iter().map(|d| l.sel(d.index)).collect(),
    }
}

/// Battery text, percent and (for voltage-only batteries) millivolts for a device:
/// 0x1004/0x1000/0x1001, or 0x1F20 with a curve.
fn battery_now(l: &mut Live, index: u8) -> (Option<String>, Option<u8>, Option<u16>) {
    if l.devs.iter().any(|d| d.index == index && d.has_voltage) {
        return match audio::adc(&mut l.h, index) {
            Ok(a) => {
                let mut ignored = Vec::new();
                report_voltage(l, index, &a, &mut ignored);
                let c = l.devs.iter().find(|d| d.index == index).and_then(|d| d.curve.as_ref());
                (
                    c.map(|c| audio::voltage_text(&a, c)),
                    c.map(|c| c.percent(a.mv)),
                    Some(a.mv),
                )
            }
            Err(e) => (Some(format!("unreadable: {e}")), None, None),
        };
    }
    match device::battery(&mut l.h, index) {
        Ok(b) => (Some(b.to_string()), b.percent(), None),
        Err(Error::NoFeature(_)) => (None, None, None),
        Err(e) => (Some(format!("unreadable: {e}")), None, None),
    }
}

/// CLI `watch`: print every update with a timestamp until Ctrl+C.
pub fn run(swid: u8, trace: bool, presets: &Path) -> Result<(), Error> {
    say!("presets    {}", presets.display());
    let (mut m, first) = Monitor::new(swid, trace, presets)?;
    for u in first {
        say!("{u}");
    }
    say!("watching for device and setting changes (Ctrl+C to stop)");
    loop {
        for u in m.next(None)? {
            say!("{u}");
        }
    }
}

/// Identify a newly opened interface; for receivers, attach every online paired device.
fn describe(l: &mut Live, out: &mut Vec<Update>) {
    match l.h.ping(DIRECT, 0x5A) {
        Ok(_) => attach(l, DIRECT, out),
        Err(Error::Hpp10(_)) => {
            // HID++ 1.0 answer to a 2.0 ping: a receiver. [OBS §9.6]
            out.push(Update::Receiver {
                pid: l.pid,
                product: l.product.clone(),
            });
            // Notification flags (register 0x00) are only read, never written. [OBS §9.8]
            out.push(Update::Line(
                match l.h.read_register(DIRECT, GET_REGISTER, 0x00, [0, 0, 0], &[]) {
                    Ok(p) => format!("  notification flags (reg 0x00): {}", hidpp::hex(&p[..3.min(p.len())])),
                    Err(e) => format!("  notification flags (reg 0x00): {e}"),
                },
            ));
            for slot in 0..6u8 {
                if device::pairing(&mut l.h, slot).is_ok() {
                    attach(l, slot + 1, out);
                }
            }
        }
        Err(e) => out.push(Update::Line(format!(
            "+ {:04x}  {}  [no HID++ 2.0 answer: {e}]",
            l.pid, l.product
        ))),
    }
}

/// Ping a device, report its name and battery, and learn the indices of the event features.
fn attach(l: &mut Live, index: u8, out: &mut Vec<Update>) {
    let sel = l.sel(index);
    let protocol = match l.h.ping(index, 0x5A) {
        Ok(v) => v,
        // [HW] the receiver answers 0x8F code 0x08 at once for a paired device whose link is down.
        Err(Error::Hpp10(0x08)) => {
            out.push(Update::Offline {
                sel,
                reason: "paired, not connected".into(),
            });
            return;
        }
        Err(e) => {
            out.push(Update::Offline {
                sel,
                reason: e.to_string(),
            });
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
        name: name.clone(),
        feats,
        has_voltage,
        model: 0,
        curve: None,
        low_mv: u16::MAX,
        last_battery: String::new(),
    };
    let (battery, battery_percent) = if has_voltage {
        let dir = l.dir();
        dev.model = device::model_id(&mut l.h, index).unwrap_or(l.pid);
        let adc = audio::adc(&mut l.h, index);
        if let Ok(a) = &adc {
            learn(&dir, &mut dev, a, &sel, out);
        }
        // The headset may have been charged while nothing was running; the reading just
        // recorded then closes the discharge before it, so build from it now.
        match battery::update_measured(&dir, dev.model) {
            Ok(Some(_)) => out.push(Update::Line(format!(
                "  {sel:<8} battery curve updated from the recorded discharge"
            ))),
            Ok(None) => {}
            Err(e) => crate::verbose(format_args!("{sel}: no new measured curve: {e}")),
        }
        let c = battery::find_curve(&dir, dev.model);
        let r = match adc {
            Ok(a) => {
                dev.last_battery = battery_key(&a, Some(&c));
                (Some(audio::voltage_text(&a, &c)), Some(c.percent(a.mv)))
            }
            Err(e) => (Some(format!("unreadable: {e}")), None),
        };
        dev.curve = Some(c);
        r
    } else {
        match device::battery(&mut l.h, index) {
            Ok(b) => (Some(b.to_string()), b.percent()),
            Err(_) => (None, None),
        }
    };
    out.push(Update::Added {
        sel,
        name,
        protocol,
        battery,
        battery_percent,
    });
    l.devs.retain(|d| d.index != index);
    l.devs.push(dev);
    if l.apply_presets {
        apply_preset(l, index, out);
    }
}

/// Apply this device's preset, if any. Errors are reported, never fatal: `watch` keeps running.
fn apply_preset(l: &mut Live, index: u8, out: &mut Vec<Update>) {
    let sel = l.sel(index);
    let list = match config::load(&l.presets) {
        Ok(v) => v,
        Err(e) => {
            out.push(Update::Line(format!("  {sel:<8} presets not loaded: {e}")));
            return;
        }
    };
    if list.is_empty() {
        return;
    }
    let model = match device::model_id(&mut l.h, index) {
        Ok(m) => m,
        Err(e) => {
            out.push(Update::Line(format!(
                "  {sel:<8} model ID unknown, preset not applied: {e}"
            )));
            return;
        }
    };
    let Some(p) = list.iter().find(|p| p.model == model) else {
        return;
    };
    let result = config::apply(&mut l.h, index, p).map_err(|e| e.to_string());
    out.push(Update::Preset { sel, result });
}

fn handle(l: &mut Live, r: &[u8], trace: bool, out: &mut Vec<Update>) {
    if r.len() < 4 {
        return;
    }
    let feats: &[(u8, u16)] = l.devs.iter().find(|d| d.index == r[1]).map_or(&[], |d| &d.feats);
    let Some(event) = device::decode_event(r, feats) else {
        if trace {
            eprintln!("<- {}  (not an event we decode)", hidpp::hex(r));
        }
        return;
    };
    if trace {
        eprintln!("<- {}", hidpp::hex(r));
    }
    match event {
        Event::Connection {
            index, online: true, ..
        } => {
            out.push(Update::Event {
                sel: l.sel(index),
                event,
            });
            attach(l, index, out);
        }
        Event::Disconnected { index } => {
            out.push(Update::Event {
                sel: l.sel(index),
                event,
            });
            l.devs.retain(|d| d.index != index);
        }
        Event::Voltage { mv, flags } => report_voltage(l, r[1], &Adc { mv, flags }, out),
        _ => out.push(Update::Event {
            sel: l.sel(r[1]),
            event,
        }),
    }
}

/// Change key for battery reporting: the percentage with a curve, else 10 mV steps.
fn battery_key(a: &Adc, curve: Option<&Curve>) -> String {
    match curve {
        Some(c) => format!("{}%", c.percent(a.mv)),
        None => format!("{}", a.mv / 10),
    }
}

/// Record a voltage reading and report it if it changed what the user sees.
fn report_voltage(l: &mut Live, index: u8, a: &Adc, out: &mut Vec<Update>) {
    let sel = l.sel(index);
    let dir = l.dir();
    let Some(d) = l.devs.iter_mut().find(|d| d.index == index) else {
        return;
    };
    learn(&dir, d, a, &sel, out);
    let key = battery_key(a, d.curve.as_ref());
    if key != d.last_battery
        && let Some(c) = &d.curve
    {
        d.last_battery = key;
        out.push(Update::Battery {
            sel,
            text: audio::voltage_text(a, c),
            percent: Some(c.percent(a.mv)),
        });
    }
}

/// Add the reading to the voltage recording. When charging starts after a discharge (a clear
/// rise over the lowest voltage seen), build or refine the measured curve from the recording.
fn learn(dir: &Path, d: &mut Dev, a: &Adc, sel: &str, out: &mut Vec<Update>) {
    if let Err(e) = battery::record(dir, d.model, a.mv, a.flags) {
        crate::verbose(format_args!("{sel}: voltage recording failed: {e}"));
    }
    if d.low_mv != u16::MAX && a.mv > d.low_mv.saturating_add(battery::CHARGE_RISE_MV) {
        d.low_mv = a.mv;
        match battery::update_measured(dir, d.model) {
            Ok(Some(c)) => {
                out.push(Update::Line(format!(
                    "  {sel:<8} battery curve updated from the recorded discharge"
                )));
                d.curve = Some(c);
            }
            Ok(None) => {}
            Err(e) => crate::verbose(format_args!("{sel}: no new measured curve: {e}")),
        }
    } else {
        d.low_mv = d.low_mv.min(a.mv);
    }
}

/// Timer read for voltage-only batteries. A failed read (e.g. headset switched off) is only
/// shown with --verbose; watch keeps running.
fn poll_battery(l: &mut Live, out: &mut Vec<Update>) {
    let idx: Vec<u8> = l.devs.iter().filter(|d| d.has_voltage).map(|d| d.index).collect();
    for i in idx {
        match audio::adc(&mut l.h, i) {
            Ok(a) => report_voltage(l, i, &a, out),
            Err(e) => crate::verbose(format_args!("battery read failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The CLI `watch` lines must stay exactly as before the Monitor refactor ([HW] traces).
    #[test]
    fn update_lines_match_cli_watch() {
        let cases: Vec<(Update, &str)> = vec![
            (
                Update::Receiver {
                    pid: 0xC54D,
                    product: "USB Receiver".into(),
                },
                "+ c54d  USB Receiver  (receiver)",
            ),
            (
                Update::Added {
                    sel: "c54d:1".into(),
                    name: "PRO X 2".into(),
                    protocol: (4, 2),
                    battery: Some("54% level=good discharging external=none".into()),
                    battery_percent: Some(54),
                },
                "+ c54d:1   PRO X 2  (HID++ 4.2)  battery 54% level=good discharging external=none",
            ),
            (
                Update::Added {
                    sel: "0aba".into(),
                    name: "H".into(),
                    protocol: (4, 2),
                    battery: None,
                    battery_percent: None,
                },
                "+ 0aba     H  (HID++ 4.2)",
            ),
            (
                Update::Offline {
                    sel: "c54d:1".into(),
                    reason: "paired, not connected".into(),
                },
                "  c54d:1   offline (paired, not connected)",
            ),
            (
                Update::Removed {
                    pid: 0xC54D,
                    product: "USB Receiver".into(),
                    reason: Some("x".into()),
                    sels: vec![],
                },
                "- c54d  USB Receiver  removed (x)",
            ),
            (
                Update::Removed {
                    pid: 0xC09B,
                    product: "PRO X 2".into(),
                    reason: None,
                    sels: vec![],
                },
                "- c09b  PRO X 2  removed",
            ),
            (
                Update::Event {
                    sel: "c54d:1".into(),
                    event: Event::Dpi { x: 800, y: 800, lod: 2 },
                },
                "  c54d:1   dpi 800 (y 800, lod 2)",
            ),
            (
                Update::Battery {
                    sel: "0aba".into(),
                    text: "77%".into(),
                    percent: Some(77),
                },
                "  0aba     battery 77%",
            ),
            (
                Update::Preset {
                    sel: "c54d:1".into(),
                    result: Ok("host mode, dpi 1200".into()),
                },
                "  c54d:1   preset applied: host mode, dpi 1200",
            ),
            (
                Update::Preset {
                    sel: "c54d:1".into(),
                    result: Err("boom".into()),
                },
                "  c54d:1   preset failed: boom",
            ),
        ];
        for (u, want) in cases {
            assert_eq!(u.to_string(), want);
        }
    }
}
