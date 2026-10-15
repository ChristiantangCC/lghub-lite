//! Headset features: 0x1F20 ADC measurement (battery voltage) and 0x8310 hardware
//! equalizer (read-only). Layouts were read from G HUB's own request code (described in
//! NOTES.md, not copied) and confirmed on the PRO X Wireless headset (0aba).

use crate::hidpp::{Error, Hidpp};

fn need(p: &[u8], n: usize) -> Result<(), Error> {
    if p.len() < n {
        Err(Error::BadReply("reply shorter than expected"))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------- 0x1F20 battery voltage

#[derive(Debug, PartialEq)]
pub struct Adc {
    pub mv: u16,
    /// Status bits; G HUB splits them into four flags whose meanings are [UNK].
    pub flags: u8,
}

/// f0 `[channel]` -> `[mV BE16, flags]`; the event has the same layout.
/// [OBS lghub_agent.exe 0x140cc4070 (channel must be < 16) and 0x140cc46c0 (event)]
/// [HW headset: `-> 11 FF 06 03 00`, `<- 0F 4F 01` = 3919 mV while G HUB showed 80 %]
pub fn parse_adc(p: &[u8]) -> Result<Adc, Error> {
    need(p, 3)?;
    Ok(Adc {
        mv: u16::from_be_bytes([p[0], p[1]]),
        flags: p[2],
    })
}

pub fn adc(h: &mut Hidpp, dev: u8) -> Result<Adc, Error> {
    let fi = h.feature(dev, 0x1F20)?;
    parse_adc(&h.call(dev, fi, 0, &[0], &[])?)
}

// ---------------------------------------------------------------- battery curve

/// Discharge curve from a power-model XML that the user copies from their OWN G HUB install
/// into the preset folder (%APPDATA%\lghub-lite\). The file is never bundled or committed;
/// without it only the voltage is shown. Format [OBS protocol.md §10.6]:
/// `<powermodel vid=".." pid="0x0aba">`, a `<discharge>` block of `minutes, volts, mAh used`
/// rows in time order, and `<blocks default_unit="milliamps">` with `power="N"` currents.
#[derive(Debug)]
pub struct Curve {
    pub pid: u16,
    /// (millivolts, micro-amp-hours used so far), in discharge order. Fixed-point integers
    /// rather than floats: Rust's float parse/format code added ~50 KB to the exe.
    points: Vec<(u32, u64)>,
    /// Sum of the block currents in micro-amps, if the file states them in milliamps.
    pub draw_ua: Option<u64>,
    pub file: String,
}

/// Parse a plain decimal like `4.102`, `1271.357184` or `53` into an integer scaled by
/// 10^`digits` (extra fraction digits are truncated). No sign, no exponent.
pub fn fixed(s: &str, digits: u32) -> Option<u64> {
    let s = s.trim();
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    if int.is_empty() && frac.is_empty() || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut f = frac.bytes();
    for _ in 0..digits {
        let d = f.next().map_or(0, |b| (b - b'0') as u64);
        v = v.checked_mul(10)?.checked_add(d)?;
    }
    Some(v)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let len = tag[start..].find('"')?;
    Some(tag[start..start + len].to_string())
}

fn section<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let s = text.find(open)?;
    let body = &text[s..];
    let body = &body[body.find('>')? + 1..];
    Some(&body[..body.find(close)?])
}

pub fn parse_curve(text: &str, file: &str) -> Result<Curve, String> {
    let pm = text.find("<powermodel").ok_or("no <powermodel> element")?;
    let tag = &text[pm..pm + text[pm..].find('>').ok_or("unterminated <powermodel>")?];
    let pid = attr(tag, "pid").ok_or("no pid attribute")?;
    let pid = u16::from_str_radix(pid.trim_start_matches("0x"), 16).map_err(|_| "bad pid attribute")?;
    let body = section(text, "<discharge", "</discharge>").ok_or("no <discharge> block")?;
    let mut points = Vec::new();
    for line in body.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if let [_minutes, volts, mah] = f[..]
            && let (Some(mv), Some(uah)) = (fixed(volts, 3), fixed(mah, 3))
        {
            points.push((mv as u32, uah));
        }
    }
    if points.len() < 2 {
        return Err("discharge block has fewer than 2 rows".into());
    }
    // mAh used must never decrease; voltage only has to fall overall, because real measured
    // curves wiggle by a millivolt here and there [HW file check]. The "first row at or below"
    // lookup tolerates that noise.
    if points.windows(2).any(|w| w[1].1 < w[0].1) || points[0].0 <= points[points.len() - 1].0 {
        return Err("discharge rows are not in falling-voltage / rising-mAh order".into());
    }
    let draw_ua = section(text, "<blocks", "</blocks>")
        .filter(|_| text.contains("default_unit=\"milliamps\""))
        .map(|b| {
            b.split("power=\"")
                .skip(1)
                .filter_map(|s| fixed(s.split('"').next()?, 3))
                .sum()
        })
        .filter(|&ua: &u64| ua > 0);
    Ok(Curve {
        pid,
        points,
        draw_ua,
        file: file.to_string(),
    })
}

impl Curve {
    fn total(&self) -> u64 {
        self.points[self.points.len() - 1].1
    }

    /// Remaining share of the discharge capacity at `mv`, in micro-amp-hours: the first row at
    /// or below that voltage gives the charge already used. This reproduces G HUB's number
    /// [HW: 3919 mV -> 80.5 %, G HUB showed 80 %]. Only valid while discharging.
    pub fn remaining_uah(&self, mv: u16) -> u64 {
        let used = self
            .points
            .iter()
            .find(|p| p.0 <= mv as u32)
            .map_or(self.total(), |p| p.1);
        self.total().saturating_sub(used)
    }

    pub fn percent(&self, mv: u16) -> u8 {
        let t = self.total().max(1);
        (self.remaining_uah(mv) * 100 / t).min(100) as u8
    }

    /// Tenths of an hour left = remaining charge / block current [HW: matches G HUB's
    /// `mileage` 18.7 h at 78 %].
    pub fn tenths_of_hours_left(&self, mv: u16) -> Option<u64> {
        self.draw_ua.map(|ua| self.remaining_uah(mv) * 10 / ua)
    }
}

/// Where G HUB keeps its downloaded device resources (power models among them).
pub const GHUB_DEPOTS: &str = r"C:\ProgramData\LGHUB\depots";

/// Look for a power model for `pid` in a local G HUB install (`GHUB_DEPOTS`, at most three
/// folders deep, file names containing "battery"). Only used when the user asks to import one;
/// lghub-lite never reads G HUB's folders otherwise.
pub fn find_ghub_curve(pid: u16) -> Option<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, depth: u8, pid: u16) -> Option<std::path::PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth > 0
                    && let Some(p) = walk(&path, depth - 1, pid)
                {
                    return Some(p);
                }
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !name.ends_with(".xml") || !name.contains("battery") || meta.len() > 4 << 20 {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path)
                && parse_curve(&text, &name).is_ok_and(|c| c.pid == pid)
            {
                return Some(path);
            }
        }
        None
    }
    walk(std::path::Path::new(GHUB_DEPOTS), 3, pid)
}

/// Copy a power-model file into `dest_dir` (the user's own copy; see `find_curve`).
pub fn import_curve(src: &std::path::Path, dest_dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    std::fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.join(src.file_name().unwrap_or_default());
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

/// Find a curve for `pid` among the *.xml files in `dir` (files over 4 MB are ignored).
pub fn find_curve(dir: &std::path::Path, pid: u16) -> Option<Curve> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("xml") {
            continue;
        }
        if e.metadata().map_or(true, |m| m.len() > 4 << 20) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match parse_curve(&text, &name) {
            Ok(c) if c.pid == pid => return Some(c),
            Ok(_) => {}
            Err(err) => crate::verbose(format_args!("{}: {err}", path.display())),
        }
    }
    None
}

/// Battery line for any device: 0x1004/0x1000/0x1001 when present, else 0x1F20 voltage with
/// the curve for the device's model ID (if one is in `curve_dir`).
pub fn battery_text(h: &mut Hidpp, dev: u8, curve_dir: &std::path::Path) -> Result<String, Error> {
    match crate::device::battery(h, dev) {
        Ok(b) => return Ok(b.to_string()),
        Err(Error::NoFeature(_)) => {}
        Err(e) => return Err(e),
    }
    let a = adc(h, dev)?;
    let curve = crate::device::model_id(h, dev)
        .ok()
        .and_then(|m| find_curve(curve_dir, m));
    Ok(voltage_text(&a, curve.as_ref()))
}

/// One-line battery text for a 0x1F20 device: voltage, and percent/hours when a curve exists.
pub fn voltage_text(a: &Adc, curve: Option<&Curve>) -> String {
    match curve {
        Some(c) => {
            let hours = c
                .tenths_of_hours_left(a.mv)
                .map(|t| format!("  ~{}.{} h left", t / 10, t % 10))
                .unwrap_or_default();
            format!(
                "{}%{hours}  ({} mV, flags={:#04x}, curve {})",
                c.percent(a.mv),
                a.mv,
                a.flags,
                c.file
            )
        }
        None => format!(
            "{} mV  flags={:#04x}  (no battery curve: see `lghub-lite battery` help)",
            a.mv, a.flags
        ),
    }
}

// 0x8300 sidetone: the HID++ level reads and writes back correctly, but a human heard no
// effect at 50 [HW], so it is not exposed; findings are kept in NOTES.md.

// ---------------------------------------------------------------- 0x8310 equalizer

#[derive(Debug, PartialEq)]
pub struct EqInfo {
    pub bands: u8,
    /// Gains are limited to +/- this many dB.
    pub db_range: u8,
    pub capabilities: u8,
    pub db_min: i8,
    pub db_max: i8,
}

/// f0 GetEQInfo -> [count, dbRange, capabilities, dbMin (signed), dbMax (signed)].
/// [OBS 0x140d260e0 and its log format string] [HW headset: `0A 0C 01 00 00`]
pub fn parse_eq_info(p: &[u8]) -> Result<EqInfo, Error> {
    need(p, 5)?;
    Ok(EqInfo {
        bands: p[0],
        db_range: p[1],
        capabilities: p[2],
        db_min: p[3] as i8,
        db_max: p[4] as i8,
    })
}

/// f1 GetFrequencies [first band] -> [first band echo, BE16 Hz...]; a 16-byte payload holds
/// 7 bands, so 10 bands take two calls. [OBS 0x140d26280] [HW headset: from 0: 32 64 125 250
/// 500 1000 2000; from 7: 4000 8000 16000]
pub fn parse_frequencies(p: &[u8], first: u8, wanted: usize) -> Result<Vec<u16>, Error> {
    need(p, 1)?;
    if p[0] != first {
        return Err(Error::BadReply("frequency reply does not echo the first band"));
    }
    Ok(p[1..]
        .as_chunks::<2>()
        .0
        .iter()
        .take(wanted)
        .map(|c| u16::from_be_bytes(*c))
        .collect())
}

pub struct Eq {
    pub info: EqInfo,
    pub freqs: Vec<u16>,
    pub gains: Vec<i8>,
}

pub fn eq(h: &mut Hidpp, dev: u8) -> Result<Eq, Error> {
    let fi = h.feature(dev, 0x8310)?;
    let info = parse_eq_info(&h.call(dev, fi, 0, &[], &[])?)?;
    let n = info.bands as usize;
    let mut freqs = Vec::with_capacity(n);
    while freqs.len() < n {
        let first = freqs.len() as u8;
        let got = parse_frequencies(&h.call(dev, fi, 1, &[first], &[4])?, first, n - freqs.len())?;
        if got.is_empty() {
            return Err(Error::BadReply("no frequencies returned"));
        }
        freqs.extend(got);
    }
    // f2 GetEQ -> one signed dB byte per band. [OBS 0x140d264a0] [HW headset: all 0 = flat]
    let g = h.call(dev, fi, 2, &[], &[])?;
    need(&g, n)?;
    let gains = g[..n].iter().map(|&b| b as i8).collect();
    Ok(Eq { info, freqs, gains })
}

// No EQ write: G HUB's f3 SetEQ sends [one byte from its caller, gain per band]
// [OBS 0x140d26650], but the meaning of that first byte is [UNK]. On the PRO X Wireless
// headset G HUB never uses the hardware EQ (device capability onboardEqSupported = false,
// agent /devices/list), and `[00, 03, 02, 00 …]` / `[00, 00 × 10]` were refused with
// InvalidArgument [HW], device unchanged. Not guessing further.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adc_from_hw() {
        assert_eq!(parse_adc(&[0x0F, 0x4F, 0x01, 0]).unwrap(), Adc { mv: 3919, flags: 1 });
        assert!(parse_adc(&[0x0F, 0x4F]).is_err());
    }

    // Synthetic power model with made-up numbers (same shape as the real format, none of
    // Logitech's data): 100 mAh capacity, 10 mA draw.
    const SYNTHETIC: &str = r#"<?xml version="1.0"?>
<powermodel vid="0x46d" pid="0x1234">
  <battery>
    <!-- Discharge: minutes, volts, mAh -->
    <discharge format="default">
      0, 4.10, 0
      60, 4.00, 10
      240, 3.80, 40
      480, 3.60, 80
      600, 3.30, 100
    </discharge>
    <charging format="charging_topping"><ascending>3.5, 1.0</ascending></charging>
  </battery>
  <blocks default_unit="milliamps"><mcu power="7"/><radio power="3"/></blocks>
</powermodel>"#;

    #[test]
    fn curve_parse_and_lookup() {
        let c = parse_curve(SYNTHETIC, "synthetic.xml").unwrap();
        assert_eq!(c.pid, 0x1234);
        assert_eq!(c.draw_ua, Some(10_000));
        assert_eq!(c.percent(4200), 100); // above the first row
        assert_eq!(c.percent(4050), 90); // first row <= 4.05 V is 4.00 V, 10 mAh used
        assert_eq!(c.percent(3800), 60);
        assert_eq!(c.percent(3700), 20); // first row <= 3.70 V is 3.60 V, 80 mAh used
        assert_eq!(c.percent(3000), 0);
        assert_eq!(c.tenths_of_hours_left(3800), Some(60));
    }

    #[test]
    fn fixed_point_parsing() {
        assert_eq!(fixed("4.102", 3), Some(4102));
        assert_eq!(fixed(" 1271.357184 ", 3), Some(1_271_357)); // extra digits truncated
        assert_eq!(fixed("53", 3), Some(53_000));
        assert_eq!(fixed("0.5", 3), Some(500));
        assert_eq!(fixed(".25", 3), Some(250));
        assert_eq!(fixed("4.", 3), Some(4000));
        for bad in ["", ".", "-1", "1e3", "4.1.2", "abc", "99999999999999999999"] {
            assert_eq!(fixed(bad, 3), None, "{bad}");
        }
    }

    #[test]
    fn curve_rejects_bad_files() {
        assert!(parse_curve("<foo/>", "x").is_err());
        assert!(
            parse_curve(
                r#"<powermodel pid="0x1"><discharge>1, 4.0, 0</discharge></powermodel>"#,
                "x"
            )
            .is_err()
        );
        // rising voltage overall = wrong order
        let bad = r#"<powermodel pid="0x1"><discharge>0, 3.5, 0
1, 4.0, 5</discharge></powermodel>"#;
        assert!(parse_curve(bad, "x").unwrap_err().contains("order"));
        // falling mAh = wrong order
        let bad = r#"<powermodel pid="0x1"><discharge>0, 4.0, 5
1, 3.5, 4</discharge></powermodel>"#;
        assert!(parse_curve(bad, "x").unwrap_err().contains("order"));
        // a 1 mV wiggle (as in real measured curves) is accepted
        let wiggle = r#"<powermodel pid="0x1"><discharge>0, 4.000, 0
1, 3.950, 10
2, 3.951, 20
3, 3.500, 100</discharge></powermodel>"#;
        assert_eq!(parse_curve(wiggle, "x").unwrap().percent(3951), 90);
        // no milliamp blocks -> no hours estimate, percent still works
        let no_blocks = r#"<powermodel pid="0x1"><discharge>0, 4.0, 0
1, 3.5, 50</discharge></powermodel>"#;
        let c = parse_curve(no_blocks, "x").unwrap();
        assert_eq!((c.percent(3600), c.tenths_of_hours_left(3600)), (0, None));
    }

    #[test]
    fn curve_found_in_folder_by_pid() {
        let dir = std::env::temp_dir().join(format!("lghub-lite-curve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("other.xml"), "<not a power model/>").unwrap();
        std::fs::write(dir.join("model.xml"), SYNTHETIC).unwrap();
        assert_eq!(find_curve(&dir, 0x1234).map(|c| c.file), Some("model.xml".into()));
        assert!(find_curve(&dir, 0x0ABA).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn eq_from_hw() {
        let i = parse_eq_info(&[0x0A, 0x0C, 0x01, 0x00, 0x00, 0]).unwrap();
        assert_eq!((i.bands, i.db_range), (10, 12));
        let p0 = [
            0x00, 0x00, 0x20, 0x00, 0x40, 0x00, 0x7D, 0x00, 0xFA, 0x01, 0xF4, 0x03, 0xE8, 0x07, 0xD0, 0x00,
        ];
        assert_eq!(
            parse_frequencies(&p0, 0, 10).unwrap(),
            [32, 64, 125, 250, 500, 1000, 2000]
        );
        let p7 = [0x07, 0x0F, 0xA0, 0x1F, 0x40, 0x3E, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(parse_frequencies(&p7, 7, 3).unwrap(), [4000, 8000, 16000]);
        assert!(parse_frequencies(&p7, 0, 3).is_err());
    }
}
