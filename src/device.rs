//! Identify devices and query/set their features. Evidence pointers: NOTES.md.

use crate::hid;
use crate::hidpp::{self, DIRECT, Error, GET_LONG_REGISTER, GET_REGISTER, Hidpp};

/// The HID++ collections belonging to one USB interface.
pub struct Iface {
    pub pid: u16,
    pub product: String,
    pub cols: Vec<hid::Info>,
}

/// Is this collection one G HUB would talk HID++ on? [OBS §9.2]
/// - page 0xFF00, usage 1 (short, 7 B) or 2 (long, 20 B);
/// - page 0xFF43, usage HHLL where LL is a single bit and HH & LL != 0.
///
/// Lengths are also required to match a HID++ report so that the 64-byte 0xFF00/1
/// collection on the PRO X headset (Avnera "AvServe", §9.2 step 4) is excluded. [INF]
pub fn is_hidpp(i: &hid::Info) -> bool {
    match i.usage_page {
        0xFF00 => matches!((i.usage, i.out_len), (1, 7) | (2, 20)),
        0xFF43 => {
            let (hh, ll) = ((i.usage >> 8) as u8, i.usage as u8);
            ll.count_ones() == 1 && hh & ll != 0 && hidpp::report_id_for_len(i.out_len as usize).is_some()
        }
        _ => false,
    }
}

/// Group key: interface path without the `&colNN` part and the per-collection instance
/// suffix, e.g. `vid_046d&pid_c54d&mi_02#8&22bf657e&0`.
pub fn group_key(path: &str) -> String {
    let p = path.to_ascii_lowercase();
    let segs: Vec<&str> = p.split('#').collect();
    let (Some(a), Some(b)) = (segs.get(1), segs.get(2)) else {
        return p;
    };
    let a = match a.find("&col") {
        Some(i) => &a[..i],
        None => a,
    };
    let b = match b.rfind('&') {
        Some(i) => &b[..i],
        None => b,
    };
    format!("{a}#{b}")
}

pub fn interfaces() -> Result<Vec<Iface>, Error> {
    let all = hid::enumerate(crate::LOGITECH_VID).map_err(|e| Error::Io(e.to_string()))?;
    let mut out: Vec<(String, Iface)> = Vec::new();
    for i in all.into_iter().filter(is_hidpp) {
        let k = group_key(&i.path);
        match out.iter_mut().find(|(key, _)| *key == k) {
            Some((_, g)) => g.cols.push(i),
            None => out.push((
                k,
                Iface {
                    pid: i.pid,
                    product: i.product.clone(),
                    cols: vec![i],
                },
            )),
        }
    }
    Ok(out.into_iter().map(|(_, g)| g).collect())
}

/// `PID` or `PID:INDEX` in hex/decimal, e.g. `c54d:1`, `0aba`.
pub fn parse_selector(s: &str) -> Option<(u16, u8)> {
    let (pid, idx) = match s.split_once(':') {
        Some((p, i)) => (p, i.parse::<u8>().ok().filter(|i| (1..=6).contains(i))?),
        None => (s, DIRECT),
    };
    Some((u16::from_str_radix(pid.trim_start_matches("0x"), 16).ok()?, idx))
}

pub fn open(pid: u16, swid: u8, trace: bool) -> Result<Hidpp, Error> {
    let ifs = interfaces()?;
    let f = ifs
        .iter()
        .find(|f| f.pid == pid)
        .ok_or_else(|| Error::Other(format!("no HID++ interface with PID {pid:04x} (run `devices`)")))?;
    Hidpp::open(&f.cols, swid, trace)
}

fn need(p: &[u8], n: usize) -> Result<(), Error> {
    if p.len() < n {
        Err(Error::BadReply("reply shorter than expected"))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------- receiver

/// Pairing info for one receiver slot. Request bytes [OBS §9.8], reply layout [PUB Solaar];
/// wpid and kind verified [HW] on receiver c54d (NOTES.md §2).
#[derive(Debug, PartialEq)]
pub struct Pairing {
    pub wpid: u16,
    pub kind: u8,
    pub raw: Vec<u8>,
}

/// Payload p = reply bytes 4.. : p[0] echoes 0x20+slot, p[3..5] wireless PID big-endian,
/// p[7] low nibble device kind. [PUB Solaar receiver.py] [HW c54d: 20 08 08 40 A9 11 00 02 ...]
pub fn parse_pairing(p: &[u8]) -> Result<Pairing, Error> {
    need(p, 8)?;
    Ok(Pairing {
        wpid: u16::from_be_bytes([p[3], p[4]]),
        kind: p[7] & 0x0F,
        raw: p.to_vec(),
    })
}

pub fn pairing(h: &mut Hidpp, slot: u8) -> Result<Pairing, Error> {
    let sub = 0x20 + slot;
    let p = h.read_register(DIRECT, GET_LONG_REGISTER, 0xB5, [sub, 0, 0], &[4])?;
    parse_pairing(&p)
}

/// Receiver register 0x02 (connection state). [OBS §9.8] p[1] = number of connected devices
/// [PUB Solaar] [HW c54d: `00 01 00` with one mouse connected].
pub fn connected_count(h: &mut Hidpp) -> Result<u8, Error> {
    let p = h.read_register(DIRECT, GET_REGISTER, 0x02, [0, 0, 0], &[])?;
    need(&p, 2)?;
    Ok(p[1])
}

/// Device kind names. [PUB HID++ 1.0 receiver docs / Solaar DEVICE_KIND]
pub fn kind_name(k: u8) -> &'static str {
    match k {
        1 => "keyboard",
        2 => "mouse",
        3 => "numpad",
        4 => "presenter",
        8 => "trackball",
        9 => "touchpad",
        _ => "?",
    }
}

// ---------------------------------------------------------------- identity

pub struct Feature {
    pub index: u8,
    pub id: u16,
    pub flags: u8,
    pub version: u8,
}

/// FeatureSet 0x0001: f0 count, f1 [index] -> [id_hi, id_lo, flags, version]. [OBS §9.7]
pub fn features(h: &mut Hidpp, dev: u8) -> Result<Vec<Feature>, Error> {
    let fs = h.feature(dev, 0x0001)?;
    let p = h.call(dev, fs, 0, &[], &[])?;
    need(&p, 1)?;
    let mut out = vec![Feature {
        index: 0,
        id: 0x0000,
        flags: 0,
        version: 0,
    }];
    for i in 1..=p[0] {
        let q = h.call(dev, fs, 1, &[i], &[])?;
        need(&q, 4)?;
        out.push(Feature {
            index: i,
            id: u16::from_be_bytes([q[0], q[1]]),
            flags: q[2],
            version: q[3],
        });
    }
    Ok(out)
}

/// DeviceNameType 0x0005: f0 -> length, f1 [offset] -> chars. [OBS §9.7]
pub fn name(h: &mut Hidpp, dev: u8) -> Result<String, Error> {
    let fi = h.feature(dev, 0x0005)?;
    let p = h.call(dev, fi, 0, &[], &[])?;
    need(&p, 1)?;
    let len = p[0] as usize;
    let mut s = Vec::with_capacity(len);
    while s.len() < len {
        let q = h.call(dev, fi, 1, &[s.len() as u8], &[])?;
        if q.is_empty() {
            break;
        }
        let take = (len - s.len()).min(q.len());
        s.extend_from_slice(&q[..take]);
    }
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    Ok(String::from_utf8_lossy(&s[..end]).into_owned())
}

/// DeviceNameType f2: device type. [OBS §9.7 function exists; value names PUB]
pub fn device_type(h: &mut Hidpp, dev: u8) -> Result<u8, Error> {
    let fi = h.feature(dev, 0x0005)?;
    let p = h.call(dev, fi, 2, &[], &[])?;
    need(&p, 1)?;
    Ok(p[0])
}

pub fn device_type_name(t: u8) -> &'static str {
    // [PUB] HID++ 2.0 x0005 spec / Solaar DEVICE_KIND for feature 0x0005
    match t {
        0 => "keyboard",
        1 => "remote control",
        2 => "numpad",
        3 => "mouse",
        4 => "trackpad",
        5 => "trackball",
        6 => "presenter",
        7 => "receiver",
        8 => "headset",
        9 => "webcam",
        _ => "?",
    }
}

pub struct Firmware {
    pub kind: u8,
    pub prefix: String,
    pub number: u8,
    pub revision: u8,
    pub build: u16,
}

/// FirmwareInfo 0x0003 f1 [entity] reply [HW SL2 + headset, cross-checked with agent]: p[0]&0x0F type (0 main, 1 boot,
/// 2 hardware) [OBS §9.7 + PUB], p[1..4] ASCII prefix, p[4] number, p[5] revision,
/// p[6..8] build BE [PUB].
pub fn parse_firmware(p: &[u8]) -> Result<Firmware, Error> {
    need(p, 8)?;
    Ok(Firmware {
        kind: p[0] & 0x0F,
        prefix: p[1..4]
            .iter()
            .filter(|c| c.is_ascii_graphic())
            .map(|&c| c as char)
            .collect(),
        number: p[4],
        revision: p[5],
        build: u16::from_be_bytes([p[6], p[7]]),
    })
}

pub fn firmware(h: &mut Hidpp, dev: u8) -> Result<Vec<Firmware>, Error> {
    let fi = h.feature(dev, 0x0003)?;
    let p = h.call(dev, fi, 0, &[], &[])?;
    need(&p, 1)?;
    let mut out = Vec::new();
    for e in 0..p[0] {
        let q = h.call(dev, fi, 1, &[e], &[])?;
        out.push(parse_firmware(&q)?);
    }
    Ok(out)
}

/// First non-zero model ID from FirmwareInfo f0:
/// `[count, unitId(4), transport(2), modelId(2) × 3, …]`. [OBS §9.7 model ID read; PUB layout]
/// [HW SL2 over the receiver: `02 7A F4 AD 28 00 0C 40 A9 C0 9B 00 00 …` -> 0x40A9;
/// headset: `01 FF FF FF FF 00 03 00 00 00 00 0A BA …` -> 0x0ABA]
pub fn parse_model_id(p: &[u8]) -> Result<u16, Error> {
    need(p, 13)?;
    [7, 9, 11]
        .iter()
        .map(|&i| u16::from_be_bytes([p[i], p[i + 1]]))
        .find(|&m| m != 0)
        .ok_or(Error::BadReply("no model ID"))
}

pub fn model_id(h: &mut Hidpp, dev: u8) -> Result<u16, Error> {
    let fi = h.feature(dev, 0x0003)?;
    parse_model_id(&h.call(dev, fi, 0, &[], &[])?)
}

pub fn firmware_kind_name(k: u8) -> &'static str {
    match k {
        0 => "main",
        1 => "bootloader",
        2 => "hardware",
        _ => "other",
    }
}

// ---------------------------------------------------------------- battery

#[derive(Debug, Clone, PartialEq)]
pub enum Battery {
    /// 0x1004 f1: [soc %, level bits, charging, external power]. [OBS order §10.5, PUB bytes]
    Unified {
        soc: u8,
        level: u8,
        charging: u8,
        external: u8,
    },
    /// 0x1000 f0: [level %, next level %, status]. [OBS order §10.5, PUB bytes]
    Level { level: u8, next: u8, status: u8 },
    /// 0x1001 f0: [mV BE16, flags]. [OBS §9.9]
    Voltage { mv: u16, flags: u8 },
}

pub fn parse_unified(p: &[u8]) -> Result<Battery, Error> {
    need(p, 4)?;
    Ok(Battery::Unified {
        soc: p[0],
        level: p[1],
        charging: p[2],
        external: p[3],
    })
}

pub fn battery(h: &mut Hidpp, dev: u8) -> Result<Battery, Error> {
    // 0x1004 read [HW SL2]; physical value check pending. 0x1000/0x1001: UNVERIFIED.
    if let Ok(fi) = h.feature(dev, 0x1004) {
        return parse_unified(&h.call(dev, fi, 1, &[], &[])?);
    }
    if let Ok(fi) = h.feature(dev, 0x1000) {
        let p = h.call(dev, fi, 0, &[], &[])?;
        need(&p, 3)?;
        return Ok(Battery::Level {
            level: p[0],
            next: p[1],
            status: p[2],
        });
    }
    let fi = h.feature(dev, 0x1001)?;
    let p = h.call(dev, fi, 0, &[], &[])?;
    need(&p, 3)?;
    Ok(Battery::Voltage {
        mv: u16::from_be_bytes([p[0], p[1]]),
        flags: p[2],
    })
}

impl Battery {
    /// State of charge in percent, when the device reports one (not for voltage-only).
    pub fn percent(&self) -> Option<u8> {
        match *self {
            Battery::Unified { soc, .. } => Some(soc),
            Battery::Level { level, .. } => Some(level),
            Battery::Voltage { .. } => None,
        }
    }
}

impl std::fmt::Display for Battery {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Battery::Unified {
                soc,
                level,
                charging,
                external,
            } => {
                // charging: 0 discharging, 1 charging, 2 slow, 3 complete, 4 error [OBS §10.5]
                let c = ["discharging", "charging", "slow charging", "full", "charge error"];
                let lv = ["critical", "low", "good", "full"]
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| level & (1 << i) != 0)
                    .map(|(_, n)| *n)
                    .collect::<Vec<_>>()
                    .join("|");
                let ext = ["none", "wired", "wireless"];
                write!(
                    f,
                    "{soc}% level={lv} {} external={}",
                    c.get(charging as usize).unwrap_or(&"?"),
                    ext.get(external as usize).unwrap_or(&"?")
                )
            }
            Battery::Level { level, next, status } => write!(f, "{level}% (next {next}%) status={status}"),
            Battery::Voltage { mv, flags } => write!(f, "{mv} mV flags={flags:#04x}"),
        }
    }
}

// ---------------------------------------------------------------- DPI

#[derive(Debug, Clone, PartialEq)]
pub enum DpiCaps {
    /// Discrete values.
    List(Vec<u16>),
    /// points[i]..=points[i+1] in steps of steps[i].
    Ranges { points: Vec<u16>, steps: Vec<u16> },
}

impl DpiCaps {
    pub fn allows(&self, v: u16) -> bool {
        match self {
            DpiCaps::List(l) => l.contains(&v),
            DpiCaps::Ranges { points, steps } => steps.iter().enumerate().any(|(i, &s)| {
                let (lo, hi) = (points[i], points[i + 1]);
                s > 0 && v >= lo && v <= hi && (v - lo).is_multiple_of(s)
            }),
        }
    }
}

impl std::fmt::Display for DpiCaps {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            DpiCaps::List(l) => write!(f, "{l:?}"),
            DpiCaps::Ranges { points, steps } => {
                for (i, s) in steps.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}..={} step {s}", points[i], points[i + 1])?;
                }
                Ok(())
            }
        }
    }
}

/// DPI capability stream: BE16 values, 0x0000 terminates, 0xE0xx means "step xx" between
/// the neighbouring values (step from the low byte only). Used by 0x2201 f1 and 0x2202 f2.
/// [OBS §9.9; dec_all.c FUN_140ccee80, FUN_140cd04e0]
/// Returns None if no terminator was found (more pages needed).
pub fn parse_dpi_list(p: &[u8]) -> Result<Option<DpiCaps>, Error> {
    let mut points = Vec::new();
    let mut steps = Vec::new();
    let mut terminated = false;
    for c in p.as_chunks::<2>().0 {
        let v = u16::from_be_bytes(*c);
        if v == 0 {
            terminated = true;
            break;
        }
        if c[0] == 0xE0 {
            // A step must sit between two values: exactly one value before it per step.
            if points.len() != steps.len() + 1 {
                return Err(Error::BadReply("DPI step without a preceding value"));
            }
            steps.push(c[1] as u16);
        } else {
            points.push(v);
        }
    }
    if !terminated {
        return Ok(None);
    }
    if points.is_empty() {
        return Err(Error::BadReply("empty DPI list"));
    }
    if steps.is_empty() {
        return Ok(Some(DpiCaps::List(points)));
    }
    if points.len() != steps.len() + 1 {
        return Err(Error::BadReply("DPI range form not understood"));
    }
    Ok(Some(DpiCaps::Ranges { points, steps }))
}

#[derive(Debug, Clone)]
pub struct Dpi {
    pub feature: u16,
    pub index: u8,
    pub version: u8,
    pub current: u16,
    pub default: u16,
    /// 0x2202 only: Y DPI and lift-off distance, preserved on set.
    pub y: Option<u16>,
    pub lod: u8,
    pub caps: DpiCaps,
}

/// Read DPI state for sensor 0 via 0x2201 or 0x2202, whichever the device has.
pub fn dpi(h: &mut Hidpp, dev: u8) -> Result<Dpi, Error> {
    let (index, _, version) = h.get_feature(dev, 0x2201)?;
    if index != 0 {
        return dpi_2201(h, dev, index, version);
    }
    let (index, _, version) = h.get_feature(dev, 0x2202)?;
    if index != 0 {
        return dpi_2202(h, dev, index, version);
    }
    Err(Error::NoFeature(0x2201))
}

/// 0x2201: f1 [sensor] -> [sensor, list...]; f2 [sensor] -> [sensor, cur BE16, def BE16].
/// Replies echo the sensor in byte 4. [OBS §9.9; FUN_140ccee80, FUN_140ccf280]
fn dpi_2201(h: &mut Hidpp, dev: u8, index: u8, version: u8) -> Result<Dpi, Error> {
    let l = h.call(dev, index, 1, &[0], &[4])?;
    need(&l, 3)?;
    let caps = parse_dpi_list(&l[1..])?.ok_or(Error::BadReply("unterminated DPI list"))?;
    let g = h.call(dev, index, 2, &[0], &[4])?;
    need(&g, 5)?;
    Ok(Dpi {
        feature: 0x2201,
        index,
        version,
        current: u16::from_be_bytes([g[1], g[2]]),
        default: u16::from_be_bytes([g[3], g[4]]),
        y: None,
        lod: 0,
        caps,
    })
}

/// 0x2202 (feature version < 4 page form):
/// - f2 [sensor, dir, page] -> echo of those 3 bytes, then a slice of the capability byte
///   stream; pages are concatenated (values may straddle pages) until a terminator.
/// - f5 [sensor] -> see `parse_2202_params`.
///
/// [OBS §9.9; dec_all.c FUN_140cd04e0, FUN_140cd0df0]. dir 0 = X axis [INF].
fn dpi_2202(h: &mut Hidpp, dev: u8, index: u8, version: u8) -> Result<Dpi, Error> {
    if version >= 4 {
        // v4 drops the page byte (FUN_140cd07c0); not implemented until a device needs it.
        return Err(Error::Other(format!("0x2202 v{version} range format not implemented")));
    }
    let mut stream = Vec::new();
    let mut caps = None;
    for page in 0..16u8 {
        let r = h.call(dev, index, 2, &[0, 0, page], &[4, 5, 6])?;
        need(&r, 3)?;
        stream.extend_from_slice(&r[3..]);
        caps = parse_dpi_list(&stream)?;
        if caps.is_some() {
            break;
        }
    }
    let caps = caps.ok_or(Error::BadReply("DPI range list never terminated"))?;
    let g = h.call(dev, index, 5, &[0], &[4])?;
    let (current, default, y, lod) = parse_2202_params(&g)?;
    Ok(Dpi {
        feature: 0x2202,
        index,
        version,
        current,
        default,
        y: Some(y),
        lod,
        caps,
    })
}

/// 0x2202 f5 reply: [sensor, X BE16, defaultX BE16, Y BE16, defaultY BE16, lod].
/// Returns (x, default x, y, lod). [HW SL2: after setting X=Y=1600 the reply was
/// `00 06 40 03 20 06 40 03 20 02`]. This corrects the X, Y, defX, defY order implied by
/// protocol.md §9.9; G HUB only shows four BE16 fields at bytes 1..9 (FUN_140cd0df0).
pub fn parse_2202_params(g: &[u8]) -> Result<(u16, u16, u16, u8), Error> {
    need(g, 10)?;
    let be = |i: usize| u16::from_be_bytes([g[i], g[i + 1]]);
    Ok((be(1), be(3), be(5), g[9]))
}

/// Parameters for a DPI set.
/// - 0x2201 f3: [sensor, dpi BE16] (+ dpiLevel byte when feature version >= 2; meaning
///   [UNK], 0 sent). [OBS §9.9; FUN_140ccf450] UNVERIFIED: no 0x2201 device available.
/// - 0x2202 f6: [sensor, x BE16, y BE16, lod]; X and Y both set, LOD preserved.
///   [OBS §9.9; FUN_140cd0ff0] [HW SL2 wired, host mode: `11 FF 09 63 00 06 40 06 40 02`
///   echoed, read-back 1600/1600, device event `11 FF 09 00 00 06 40 06 40 02`]
pub fn set_dpi_params(d: &Dpi, dpi: u16) -> (u8, Vec<u8>) {
    let [hi, lo] = dpi.to_be_bytes();
    match d.feature {
        0x2202 => (6, vec![0, hi, lo, hi, lo, d.lod]),
        _ if d.version < 2 => (3, vec![0, hi, lo]),
        _ => (3, vec![0, hi, lo, 0]),
    }
}

pub fn set_dpi(h: &mut Hidpp, dev: u8, d: &Dpi, value: u16) -> Result<(), Error> {
    if !d.caps.allows(value) {
        return Err(Error::Other(format!(
            "{value} DPI is not allowed by the device ({})",
            d.caps
        )));
    }
    let (func, params) = set_dpi_params(d, value);
    // 0x2202 replies echo the sensor (FUN_140cd0ff0); 0x2201 set is not echo-checked.
    let extra: &[usize] = if d.feature == 0x2202 { &[4] } else { &[] };
    h.call(dev, d.index, func, &params, extra)?;
    Ok(())
}

/// 0x8100 f2 get mode: 1 = onboard [OBS §9.9]; 2 = host [PUB libratbag] [HW SL2].
pub fn onboard_mode(h: &mut Hidpp, dev: u8) -> Result<Option<u8>, Error> {
    let (i, _, _) = h.get_feature(dev, 0x8100)?;
    if i == 0 {
        return Ok(None);
    }
    let p = h.call(dev, i, 2, &[], &[])?;
    need(&p, 1)?;
    Ok(Some(p[0]))
}

pub const MODE_ONBOARD: u8 = 1;
pub const MODE_HOST: u8 = 2;

/// 0x8100 f1 set mode `[mode]`: 1 = onboard, 2 = host. G HUB's SetOnboardMode(bool) sends
/// exactly one byte computed as onboard ? 1 : 2. [OBS dec_all.c FUN_140d0e970] [PUB libratbag]
/// [HW SL2 wired: `10 FF 0D 13 02` -> mode reads back 2; `10 FF 0D 13 01` -> back to 1].
/// Whether host mode survives a power cycle is [UNK].
pub fn set_onboard_mode(h: &mut Hidpp, dev: u8, mode: u8) -> Result<(), Error> {
    let i = h.feature(dev, 0x8100)?;
    h.call(dev, i, 1, &[mode], &[])?;
    Ok(())
}

pub fn mode_name(m: u8) -> &'static str {
    match m {
        MODE_ONBOARD => "onboard",
        MODE_HOST => "host",
        _ => "?",
    }
}

// ---------------------------------------------------------------- verified writes
// Shared by the CLI and the GUI: write, read back, and explain refusals.

/// Turn a write refusal into a readable hint when the device is in onboard-profile mode.
/// Values are validated against the device's own capability list before sending, so the
/// refusal comes from the mode. [HW] SL2 in onboard mode: 0x2202 f6 -> NotAllowed (5),
/// 0x8061 f3 -> InvalidArgument (2).
pub fn explain_refusal(r: Result<(), Error>, mode: Option<u8>) -> Result<(), Error> {
    match r {
        Err(Error::Hpp20(c @ (2 | 5))) if mode == Some(MODE_ONBOARD) => Err(Error::Other(format!(
            "device refused ({}): it is in onboard-profile mode; switch it to host mode first",
            crate::hidpp::hpp20_error_name(c)
        ))),
        r => r,
    }
}

fn readback<T: PartialEq + std::fmt::Display>(want: T, got: T) -> Result<T, Error> {
    if got == want {
        Ok(got)
    } else {
        Err(Error::Other(format!(
            "read-back {got} differs from the value written ({want})"
        )))
    }
}

/// Set DPI (X = Y), then read it back. Returns the read-back value.
pub fn set_dpi_verified(h: &mut Hidpp, dev: u8, value: u16) -> Result<u16, Error> {
    let d = dpi(h, dev)?;
    let mode = onboard_mode(h, dev).ok().flatten();
    explain_refusal(set_dpi(h, dev, &d, value), mode)?;
    readback(value, dpi(h, dev)?.current)
}

/// Set the report rate in Hz, then read it back. Returns the read-back value.
pub fn set_rate_verified(h: &mut Hidpp, dev: u8, hz: u32) -> Result<u32, Error> {
    let r = report_rate(h, dev)?;
    let mode = onboard_mode(h, dev).ok().flatten();
    explain_refusal(set_report_rate(h, dev, &r, hz), mode)?;
    readback(hz, rate_hz(report_rate(h, dev)?.current))
}

/// Set onboard (1) or host (2) mode, then read it back.
pub fn set_mode_verified(h: &mut Hidpp, dev: u8, mode: u8) -> Result<u8, Error> {
    set_onboard_mode(h, dev, mode)?;
    readback(mode, onboard_mode(h, dev)?.ok_or(Error::NoFeature(0x8100))?)
}

// ---------------------------------------------------------------- events

/// Feature IDs whose events `decode_event` understands.
pub const EVENT_FEATURES: [u16; 4] = [0x1004, 0x2202, 0x8061, 0x1F20];

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Receiver 0x41 device connection: device index (slot+1), link up/down, wireless PID,
    /// device type nibble, protocol byte. [OBS §9.8 UnifyingBus::onRead]
    Connection {
        index: u8,
        online: bool,
        wpid: u16,
        kind: u8,
        protocol: u8,
    },
    /// Receiver 0x40 with byte 3 == 0x02: device unpaired or disconnected. [OBS §9.8]
    Disconnected { index: u8 },
    /// 0x1004 event, same layout as f1. [OBS §10.5]
    Battery(Battery),
    /// 0x2202 event [sensor, X BE16, Y BE16, lod]. [HW SL2 after f6 and after mode change]
    Dpi { x: u16, y: u16, lod: u8 },
    /// 0x8061 event [connType, rate bit]. [HW SL2: `11 FF 0C 00 00 02` wired after setting
    /// bit 2; `11 01 0C 00 01 03` over the receiver]
    ReportRate { conn: u8, bit: u8 },
    /// 0x1F20 event `[mV BE16, flags]`, same layout as f0. [OBS 0x140cc46c0] Not yet seen on
    /// hardware: the headset's 0x1F20 traffic so far was G HUB's own polling (swid 0x0D).
    Voltage { mv: u16, flags: u8 },
}

/// Decode an unsolicited report. `feats` maps feature index -> feature ID for the device at
/// `r[1]`. HID++ 2.0 events have swid 0 [OBS §9.5]. Returns None for anything else
/// (replies to other software, unknown events).
pub fn decode_event(r: &[u8], feats: &[(u8, u16)]) -> Option<Event> {
    if r.len() < 7 {
        return None;
    }
    match r[2] {
        0x41 => Some(Event::Connection {
            index: r[1],
            online: r[4] & 0x40 == 0,
            wpid: u16::from_le_bytes([r[5], r[6]]),
            kind: r[4] & 0x0F,
            protocol: r[3],
        }),
        0x40 if r[3] == 0x02 => Some(Event::Disconnected { index: r[1] }),
        idx if r[3] & 0x0F == 0 && r[3] >> 4 == 0 => {
            let id = feats.iter().find(|(i, _)| *i == idx)?.1;
            let p = &r[4..];
            match id {
                0x1004 => parse_unified(p).ok().map(Event::Battery),
                0x2202 if p.len() >= 6 => Some(Event::Dpi {
                    x: u16::from_be_bytes([p[1], p[2]]),
                    y: u16::from_be_bytes([p[3], p[4]]),
                    lod: p[5],
                }),
                0x8061 if p.len() >= 2 => Some(Event::ReportRate { conn: p[0], bit: p[1] }),
                0x1F20 if p.len() >= 3 => Some(Event::Voltage {
                    mv: u16::from_be_bytes([p[0], p[1]]),
                    flags: p[2],
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Event::Connection {
                index,
                online,
                wpid,
                kind,
                protocol,
            } => write!(
                f,
                "slot {index}: {} wpid={wpid:04x} kind={} protocol={protocol:#04x}",
                if *online { "connected" } else { "link lost" },
                kind_name(*kind)
            ),
            Event::Disconnected { index } => write!(f, "slot {index}: unpaired/disconnected"),
            Event::Battery(b) => write!(f, "battery {b}"),
            Event::Dpi { x, y, lod } => write!(f, "dpi {x} (y {y}, lod {lod})"),
            Event::ReportRate { conn, bit } => write!(
                f,
                "report rate {} Hz ({})",
                rate_hz(*bit),
                if *conn == 0 { "wired" } else { "wireless" }
            ),
            Event::Voltage { mv, flags } => write!(f, "battery {mv} mV flags={flags:#04x}"),
        }
    }
}

// ---------------------------------------------------------------- report rate

/// 0x8061 ExtendedAdjustableReportRate state.
pub struct ReportRate {
    pub index: u8,
    /// Bitmap of supported rates (bit i = `rate_hz(i)`).
    pub supported: u8,
    /// Bit number of the active rate.
    pub current: u8,
}

/// Rate for bit i of the 0x8061 bitmap: bit 0 = 8 ms (125 Hz) … bit 6 = 125 µs (8000 Hz).
/// [PUB Solaar] UNVERIFIED Hz mapping: G HUB code only shows bit = 1 << index. On the wired SL2
/// the bitmap is 0x0F and the default bit 3, i.e. 1000 Hz, which is consistent with [PUB],
/// but no measurement of the actual polling rate has been made.
pub fn rate_hz(bit: u8) -> u32 {
    125u32 << bit.min(7)
}

/// 0x8061 connection type: 0 = wired, 1 = wireless. [HW SL2: f0 [0] -> caps 0x0F (cable rates),
/// f0 [1] -> 0x7F (LIGHTSPEED rates); events carry 00 over the cable and 01 over the receiver]
/// Deciding it from the device index (behind a receiver = wireless) is [INF]; a direct
/// Bluetooth device would need another rule.
pub fn conn_type(dev: u8) -> u8 {
    if dev == DIRECT { 0 } else { 1 }
}

/// f1 GetActualReportRateList -> reply[1] = bitmap for the active connection;
/// f2 GetReportRate [connType] -> reply[0] = bit number of that connection's rate.
/// [OBS dec_all.c FUN_140cf3c80, FUN_140cf3dd0]
/// [HW SL2: wired f1 `00 0F`, f2 [0] `03`; over the receiver with wireless set to 500 Hz,
/// f2 [0] -> 03 (the wired setting) but f2 [1] -> 02. So f2 must be asked for the active type.]
pub fn report_rate(h: &mut Hidpp, dev: u8) -> Result<ReportRate, Error> {
    let index = h.feature(dev, 0x8061)?;
    let l = h.call(dev, index, 1, &[], &[])?;
    need(&l, 2)?;
    let g = h.call(dev, index, 2, &[conn_type(dev)], &[])?;
    need(&g, 1)?;
    Ok(ReportRate {
        index,
        supported: l[1],
        current: g[0],
    })
}

/// Bit number for `hz` if the device supports it.
pub fn rate_bit(r: &ReportRate, hz: u32) -> Option<u8> {
    (0..8u8).find(|&b| r.supported & (1 << b) != 0 && rate_hz(b) == hz)
}

/// f3 SetReportRate [bit number]. [OBS FUN_140cf3f60: log2 of the rate bit]
/// [HW SL2 wired, host mode: `10 FF 0C 33 02` -> f2 reads back 2, event `11 FF 0C 00 00 02`;
/// in onboard mode the same request is refused with InvalidArgument.]
pub fn set_report_rate(h: &mut Hidpp, dev: u8, r: &ReportRate, hz: u32) -> Result<(), Error> {
    let bit = rate_bit(r, hz).ok_or_else(|| Error::Other(format!("{hz} Hz is not supported by the device")))?;
    h.call(dev, r.index, 3, &[bit], &[])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(page: u16, usage: u16, out: u16) -> hid::Info {
        hid::Info {
            path: String::new(),
            vid: 0x46D,
            pid: 1,
            usage_page: page,
            usage,
            in_len: out,
            out_len: out,
            product: String::new(),
        }
    }

    #[test]
    fn hidpp_collection_filter() {
        // [HW] receiver c54d and headset 0aba collections, see NOTES.md
        assert!(is_hidpp(&info(0xFF00, 1, 7)));
        assert!(is_hidpp(&info(0xFF00, 2, 20)));
        assert!(is_hidpp(&info(0xFF43, 0x0202, 20)));
        assert!(!is_hidpp(&info(0xFF00, 1, 64))); // headset AvServe collection
        assert!(!is_hidpp(&info(0x000C, 1, 0)));
        assert!(!is_hidpp(&info(0xFF43, 0x0203, 20))); // LL not a single bit
        assert!(!is_hidpp(&info(0xFF43, 0x0102, 20))); // HH & LL == 0
    }

    #[test]
    fn groups_collections_of_one_interface() {
        let a = r"\\?\hid#vid_046d&pid_c54d&mi_02&col01#8&22bf657e&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let b = r"\\?\hid#vid_046d&pid_c54d&mi_02&col02#8&22bf657e&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let c = r"\\?\hid#vid_046d&pid_c54d&mi_01&col01#8&35ef81bc&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(group_key(a), group_key(b));
        assert_ne!(group_key(a), group_key(c));
    }

    #[test]
    fn selectors() {
        assert_eq!(parse_selector("c54d:1"), Some((0xC54D, 1)));
        assert_eq!(parse_selector("0aba"), Some((0x0ABA, 0xFF)));
        assert_eq!(parse_selector("c54d:7"), None);
        assert_eq!(parse_selector("zz"), None);
    }

    fn range(points: &[u16], steps: &[u16]) -> DpiCaps {
        DpiCaps::Ranges {
            points: points.to_vec(),
            steps: steps.to_vec(),
        }
    }

    #[test]
    fn dpi_list_forms() {
        // range form: min, E0 step, max, 0 [OBS §9.9]
        let r = [0x00, 0x64, 0xE0, 0x32, 0x63, 0x9C, 0x00, 0x00];
        assert_eq!(parse_dpi_list(&r).unwrap(), Some(range(&[100, 25500], &[50])));
        let l = [0x01, 0x90, 0x03, 0x20, 0x06, 0x40, 0x00, 0x00, 0x12];
        assert_eq!(parse_dpi_list(&l).unwrap(), Some(DpiCaps::List(vec![400, 800, 1600])));
        // no terminator yet -> more pages needed
        assert_eq!(parse_dpi_list(&[0x00, 0x64, 0xE0]).unwrap(), None);
        assert!(parse_dpi_list(&[0, 0]).is_err());
        assert!(parse_dpi_list(&[0xE0, 0x0A, 0, 0]).is_err());
        assert!(range(&[100, 25500], &[50]).allows(400));
        assert!(!range(&[100, 25500], &[50]).allows(425));
        assert!(!range(&[100, 25500], &[0]).allows(400));
        // two segments
        let two = range(&[100, 1000, 8000], &[10, 50]);
        assert!(two.allows(110) && two.allows(1050) && !two.allows(1010) && !two.allows(8050));
    }

    #[test]
    fn dpi_2202_pages_concatenate() {
        // 13 payload bytes per page: a BE16 value can straddle two pages. [OBS FUN_140cd04e0]
        let page0 = [
            0x00, 0x64, 0xE0, 0x0A, 0x03, 0xE8, 0xE0, 0x32, 0x1F, 0x40, 0xE0, 0x64, 0xAB,
        ];
        let page1 = [0xE0, 0x00, 0x00];
        let mut s = page0.to_vec();
        assert_eq!(parse_dpi_list(&s).unwrap(), None);
        s.extend_from_slice(&page1);
        assert_eq!(
            parse_dpi_list(&s).unwrap(),
            Some(range(&[100, 1000, 8000, 0xABE0], &[10, 50, 100]))
        );
    }

    fn dpi_of(feature: u16, version: u8) -> Dpi {
        Dpi {
            feature,
            index: 9,
            version,
            current: 800,
            default: 800,
            y: None,
            lod: 2,
            caps: DpiCaps::List(vec![]),
        }
    }

    #[test]
    fn dpi_set_params_by_feature() {
        assert_eq!(set_dpi_params(&dpi_of(0x2201, 1), 1600), (3, vec![0, 0x06, 0x40]));
        assert_eq!(set_dpi_params(&dpi_of(0x2201, 2), 1600), (3, vec![0, 0x06, 0x40, 0]));
        assert_eq!(
            set_dpi_params(&dpi_of(0x2202, 0), 400),
            (6, vec![0, 0x01, 0x90, 0x01, 0x90, 2])
        );
    }

    #[test]
    fn dpi_2202_params_layout_from_hw() {
        // [HW] SL2 wired: before and after f6 [00 06 40 06 40 02]
        assert_eq!(
            parse_2202_params(&[0, 3, 0x20, 3, 0x20, 3, 0x20, 3, 0x20, 2]).unwrap(),
            (800, 800, 800, 2)
        );
        let after = [
            0, 0x06, 0x40, 0x03, 0x20, 0x06, 0x40, 0x03, 0x20, 0x02, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(parse_2202_params(&after).unwrap(), (1600, 800, 1600, 2));
        assert!(parse_2202_params(&after[..9]).is_err());
    }

    #[test]
    fn model_id_from_firmware_info() {
        // [HW] SL2 and headset FirmwareInfo f0 replies
        let sl2 = [
            0x02, 0x7A, 0xF4, 0xAD, 0x28, 0x00, 0x0C, 0x40, 0xA9, 0xC0, 0x9B, 0x00, 0x00, 0x00, 0x01, 0x00,
        ];
        assert_eq!(parse_model_id(&sl2).unwrap(), 0x40A9);
        let headset = [
            0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x03, 0, 0, 0, 0, 0x0A, 0xBA, 0, 0, 0,
        ];
        assert_eq!(parse_model_id(&headset).unwrap(), 0x0ABA);
        assert!(parse_model_id(&[0; 16]).is_err());
        assert!(parse_model_id(&sl2[..12]).is_err());
    }

    #[test]
    fn decodes_events() {
        let feats = [(6u8, 0x1004u16), (9, 0x2202), (12, 0x8061)];
        let pad = |b: &[u8]| {
            let mut v = b.to_vec();
            v.resize(20, 0);
            v
        };
        // headset 0x1F20 at index 6: event [mV BE16, flags]; G HUB's poll replies (swid 0x0D,
        // seen on [HW]: `11 FF 06 0D 0F 44 01`) are not events
        let headset = [(6u8, 0x1F20u16)];
        assert_eq!(
            decode_event(&pad(&[0x11, 0xFF, 0x06, 0x00, 0x0F, 0x44, 0x01]), &headset),
            Some(Event::Voltage { mv: 3908, flags: 1 })
        );
        assert_eq!(
            decode_event(&pad(&[0x11, 0xFF, 0x06, 0x0D, 0x0F, 0x44, 0x01]), &headset),
            None
        );
        // [HW] SL2 wired
        assert_eq!(
            decode_event(
                &pad(&[0x11, 0xFF, 0x09, 0x00, 0x00, 0x06, 0x40, 0x06, 0x40, 0x02]),
                &feats
            ),
            Some(Event::Dpi {
                x: 1600,
                y: 1600,
                lod: 2
            })
        );
        assert_eq!(
            decode_event(&pad(&[0x11, 0xFF, 0x0C, 0x00, 0x00, 0x02]), &feats),
            Some(Event::ReportRate { conn: 0, bit: 2 })
        );
        // [HW] over the receiver
        assert_eq!(
            decode_event(&pad(&[0x11, 0x01, 0x0C, 0x00, 0x01, 0x03]), &feats),
            Some(Event::ReportRate { conn: 1, bit: 3 })
        );
        // 0x1004 event, same layout as f1 [OBS]; bytes from the [HW] f1 reply
        assert_eq!(
            decode_event(&pad(&[0x11, 0x01, 0x06, 0x00, 0x1C, 0x02, 0x01, 0x01]), &feats),
            Some(Event::Battery(Battery::Unified {
                soc: 28,
                level: 2,
                charging: 1,
                external: 1
            }))
        );
        // our own reply (swid 3) and another program's reply (swid 0x0A) are not events
        assert_eq!(decode_event(&pad(&[0x11, 0xFF, 0x0C, 0x23, 0x03]), &feats), None);
        assert_eq!(decode_event(&pad(&[0x11, 0xFF, 0x0C, 0x2A, 0x03]), &feats), None);
        // fn != 0 with swid 0 is not an event we know
        assert_eq!(decode_event(&pad(&[0x11, 0xFF, 0x0C, 0x10, 0x03]), &feats), None);
        // unknown feature index
        assert_eq!(decode_event(&pad(&[0x11, 0xFF, 0x05, 0x00, 0x03]), &feats), None);
        // receiver 0x41 [OBS §9.8]: index 1, link up, type 2, wpid 0x40A9 little-endian
        assert_eq!(
            decode_event(&[0x10, 0x01, 0x41, 0x04, 0x02, 0xA9, 0x40], &[]),
            Some(Event::Connection {
                index: 1,
                online: true,
                wpid: 0x40A9,
                kind: 2,
                protocol: 4
            })
        );
        // bit 6 = link not established
        assert!(matches!(
            decode_event(&[0x10, 0x01, 0x41, 0x04, 0x42, 0xA9, 0x40], &[]),
            Some(Event::Connection { online: false, .. })
        ));
        assert_eq!(
            decode_event(&[0x10, 0x01, 0x40, 0x02, 0, 0, 0], &[]),
            Some(Event::Disconnected { index: 1 })
        );
        assert_eq!(decode_event(&[0x10, 0x01, 0x41, 0x04], &[]), None); // too short
    }

    #[test]
    fn report_rate_bits() {
        // [HW] wired SL2: supported 0x0F, current bit 3
        let r = ReportRate {
            index: 12,
            supported: 0x0F,
            current: 3,
        };
        assert_eq!(rate_hz(r.current), 1000);
        assert_eq!(rate_bit(&r, 500), Some(2));
        assert_eq!(rate_bit(&r, 125), Some(0));
        assert_eq!(rate_bit(&r, 2000), None); // bit 4 not in the bitmap
        assert_eq!(rate_bit(&r, 300), None);
        // f3 request: [bit]
        let m = hidpp::build20(DIRECT, 12, 3, 3, &[2]).unwrap();
        assert_eq!(m, [0x10, 0xFF, 0x0C, 0x33, 0x02, 0x00, 0x00]);
        // f2 asks for the active connection type
        assert_eq!((conn_type(DIRECT), conn_type(1)), (0, 1));
    }

    #[test]
    fn set_host_mode_request_bytes() {
        // 0x8100 at index 0x0D on the SL2 [HW]; f1 [2] = host [OBS FUN_140d0e970]
        let m = hidpp::build20(1, 0x0D, 1, 3, &[MODE_HOST]).unwrap();
        assert_eq!(m, [0x10, 0x01, 0x0D, 0x13, 0x02, 0x00, 0x00]);
    }

    #[test]
    fn unified_battery_decode() {
        assert_eq!(
            parse_unified(&[85, 0x04, 0, 0]).unwrap(),
            Battery::Unified {
                soc: 85,
                level: 4,
                charging: 0,
                external: 0
            }
        );
        assert!(parse_unified(&[85, 4]).is_err());
    }

    #[test]
    fn pairing_and_firmware_need_enough_bytes() {
        assert!(parse_pairing(&[0x20, 0, 0]).is_err());
        let p = parse_pairing(&[0x20, 0x00, 0x08, 0x40, 0x9F, 0, 0, 0x02]).unwrap();
        assert_eq!((p.wpid, p.kind), (0x409F, 2));
        assert!(parse_firmware(&[0; 7]).is_err());
    }
}
