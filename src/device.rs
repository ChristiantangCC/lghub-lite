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

pub fn firmware_kind_name(k: u8) -> &'static str {
    match k {
        0 => "main",
        1 => "bootloader",
        2 => "hardware",
        _ => "other",
    }
}

// ---------------------------------------------------------------- battery

#[derive(Debug, PartialEq)]
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

#[derive(Debug, PartialEq)]
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
/// [OBS �9.9; dec_all.c FUN_140ccee80, FUN_140cd04e0]
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
/// Replies echo the sensor in byte 4. [OBS �9.9; FUN_140ccee80, FUN_140ccf280]
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
/// - f5 [sensor] -> [sensor, x BE16, y BE16, defX BE16, defY BE16, lod].
///
/// [OBS �9.9; dec_all.c FUN_140cd04e0, FUN_140cd0df0]. dir 0 = X axis [INF].
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
    need(&g, 10)?;
    Ok(Dpi {
        feature: 0x2202,
        index,
        version,
        current: u16::from_be_bytes([g[1], g[2]]),
        y: Some(u16::from_be_bytes([g[3], g[4]])),
        default: u16::from_be_bytes([g[5], g[6]]),
        lod: g[9],
        caps,
    })
}

/// Parameters for a DPI set. UNVERIFIED on hardware until logged in NOTES.md.
/// - 0x2201 f3: [sensor, dpi BE16] (+ dpiLevel byte when feature version >= 2; meaning
///   [UNK], 0 sent). [OBS �9.9; FUN_140ccf450]
/// - 0x2202 f6: [sensor, x BE16, y BE16, lod]; X and Y both set, LOD preserved.
///   [OBS �9.9; FUN_140cd0ff0]
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
/// 0x8100 f2 get mode: 1 = onboard [OBS §9.9]; 2 = host [PUB libratbag].
pub fn onboard_mode(h: &mut Hidpp, dev: u8) -> Result<Option<u8>, Error> {
    let (i, _, _) = h.get_feature(dev, 0x8100)?;
    if i == 0 {
        return Ok(None);
    }
    let p = h.call(dev, i, 2, &[], &[])?;
    need(&p, 1)?;
    Ok(Some(p[0]))
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
