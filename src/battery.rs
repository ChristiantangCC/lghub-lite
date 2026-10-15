//! Battery percentage for devices that only report a voltage (0x1F20 headsets).
//!
//! A voltage only becomes a percentage through a discharge curve. Sources, best first:
//! 1. `curve-<model>.txt`: **our own measurement** of a full discharge, built from the voltage
//!    recording `watch`/the app keep (`voltage-<model>.csv`) and refined with every cycle.
//! 2. A G HUB power-model XML the user copied from their own install (never bundled).
//! 3. A built-in measured curve shipped with lghub-lite (our own data, none yet).
//! 4. A generic single-cell Li-ion estimate [PUB], always marked as approximate.
//!
//! All arithmetic is fixed-point integers (Rust's float code added ~50 KB to the exe).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Measured,
    GHub,
    BuiltIn,
    Approx,
}

#[derive(Debug, Clone)]
pub struct Curve {
    pub model: u16,
    /// (millivolts, remaining charge in permille) with falling voltage.
    points: Vec<(u32, u32)>,
    /// Runtime of a full discharge, in tenths of an hour, when known.
    pub runtime_tenths: Option<u32>,
    pub source: Source,
    /// File name, or a description for built-in/approximate curves.
    pub file: String,
    /// Unix time of the last recorded discharge merged into a measured curve.
    pub session_end: u64,
}

impl Curve {
    /// Remaining charge at `mv`: the first point at or below that voltage. Above the first
    /// point is full; below the last is the last point's value. This "first row at or below"
    /// rule reproduces G HUB's number from its own file [HW: 3919 mV -> 80.5 %, G HUB 80 %].
    pub fn permille(&self, mv: u16) -> u32 {
        self.points
            .iter()
            .find(|p| p.0 <= mv as u32)
            .map_or_else(|| self.points.last().map_or(0, |p| p.1), |p| p.1)
    }

    pub fn percent(&self, mv: u16) -> u8 {
        (self.permille(mv) / 10).min(100) as u8
    }

    pub fn tenths_of_hours_left(&self, mv: u16) -> Option<u32> {
        self.runtime_tenths.map(|t| t * self.permille(mv) / 1000)
    }

    pub fn is_approx(&self) -> bool {
        self.source == Source::Approx
    }

    /// Short description for display, e.g. "your measurement" or "G HUB file x.xml".
    pub fn label(&self) -> String {
        match self.source {
            Source::Measured => "your measured curve".into(),
            Source::GHub => format!("curve {}", self.file),
            Source::BuiltIn => "built-in measured curve".into(),
            Source::Approx => "generic Li-ion estimate".into(),
        }
    }
}

// ---------------------------------------------------------------- fixed-point parsing

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

// ---------------------------------------------------------------- G HUB power-model XML

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

/// Parse a power-model XML [OBS protocol.md §10.6]: `<powermodel pid="0x0aba">`, a
/// `<discharge>` block of `minutes, volts, mAh used` rows in time order, and
/// `<blocks default_unit="milliamps">` with `power="N"` currents (for the runtime).
pub fn parse_ghub_xml(text: &str, file: &str) -> Result<Curve, String> {
    let pm = text.find("<powermodel").ok_or("no <powermodel> element")?;
    let tag = &text[pm..pm + text[pm..].find('>').ok_or("unterminated <powermodel>")?];
    let pid = attr(tag, "pid").ok_or("no pid attribute")?;
    let model = u16::from_str_radix(pid.trim_start_matches("0x"), 16).map_err(|_| "bad pid attribute")?;
    let body = section(text, "<discharge", "</discharge>").ok_or("no <discharge> block")?;
    let mut rows: Vec<(u32, u64)> = Vec::new();
    for line in body.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if let [_minutes, volts, mah] = f[..]
            && let (Some(mv), Some(uah)) = (fixed(volts, 3), fixed(mah, 3))
        {
            rows.push((mv as u32, uah));
        }
    }
    if rows.len() < 2 {
        return Err("discharge block has fewer than 2 rows".into());
    }
    // mAh used must never decrease; voltage only has to fall overall, because real measured
    // curves wiggle by a millivolt here and there [HW file check].
    if rows.windows(2).any(|w| w[1].1 < w[0].1) || rows[0].0 <= rows[rows.len() - 1].0 {
        return Err("discharge rows are not in falling-voltage / rising-mAh order".into());
    }
    let total = rows[rows.len() - 1].1.max(1);
    let draw_ua: Option<u64> = section(text, "<blocks", "</blocks>")
        .filter(|_| text.contains("default_unit=\"milliamps\""))
        .map(|b| {
            b.split("power=\"")
                .skip(1)
                .filter_map(|s| fixed(s.split('"').next()?, 3))
                .sum()
        })
        .filter(|&ua: &u64| ua > 0);
    Ok(Curve {
        model,
        points: rows
            .iter()
            .map(|&(mv, used)| (mv, ((total - used) * 1000 / total) as u32))
            .collect(),
        // [HW: total/draw matches G HUB's `maxLifeSpan` 23.98 h]
        runtime_tenths: draw_ua.map(|ua| (total * 10 / ua) as u32),
        source: Source::GHub,
        file: file.to_string(),
        session_end: 0,
    })
}

// ---------------------------------------------------------------- generic estimate

/// Typical single-cell Li-ion/LiPo resting voltage vs charge, as widely published [PUB].
/// Under load the voltage reads lower, so this tends to under-report; shown as approximate.
const GENERIC_LIION: [(u32, u32); 21] = [
    (4200, 1000),
    (4150, 950),
    (4110, 900),
    (4080, 850),
    (4020, 800),
    (3980, 750),
    (3950, 700),
    (3910, 650),
    (3870, 600),
    (3850, 550),
    (3840, 500),
    (3820, 450),
    (3800, 400),
    (3790, 350),
    (3770, 300),
    (3750, 250),
    (3730, 200),
    (3710, 150),
    (3690, 100),
    (3610, 50),
    (3270, 0),
];

/// (mV, remaining permille) with falling voltage.
type Points = &'static [(u32, u32)];

/// Measured curves shipped with lghub-lite: (model, runtime tenths, points). Our own data only.
const BUILTIN: &[(u16, u32, Points)] = &[];

fn approx(model: u16) -> Curve {
    Curve {
        model,
        points: GENERIC_LIION.to_vec(),
        runtime_tenths: None,
        source: Source::Approx,
        file: String::new(),
        session_end: 0,
    }
}

// ---------------------------------------------------------------- measured curve file

fn measured_path(dir: &Path, model: u16) -> PathBuf {
    dir.join(format!("curve-{model:04x}.txt"))
}

pub fn format_measured(c: &Curve) -> String {
    let mut s = format!(
        "# lghub-lite measured discharge curve: mV,remaining permille (own measurement)\nmodel {:04x}\nruntime_tenths {}\nsession_end {}\n",
        c.model,
        c.runtime_tenths.unwrap_or(0),
        c.session_end
    );
    for (mv, pm) in &c.points {
        s.push_str(&format!("{mv},{pm}\n"));
    }
    s
}

pub fn parse_measured(text: &str, file: &str) -> Result<Curve, String> {
    let (mut model, mut runtime, mut session_end, mut points) = (None, None, 0, Vec::new());
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        if let Some(v) = line.strip_prefix("model ") {
            model = u16::from_str_radix(v.trim(), 16).ok();
        } else if let Some(v) = line.strip_prefix("runtime_tenths ") {
            runtime = v.trim().parse::<u32>().ok().filter(|&t| t > 0);
        } else if let Some(v) = line.strip_prefix("session_end ") {
            session_end = v.trim().parse().unwrap_or(0);
        } else if let Some((a, b)) = line.split_once(',') {
            let (mv, pm) = (a.trim().parse::<u32>(), b.trim().parse::<u32>());
            match (mv, pm) {
                (Ok(mv), Ok(pm)) if pm <= 1000 => points.push((mv, pm)),
                _ => return Err(format!("bad line `{line}`")),
            }
        }
    }
    if points.len() < 2 || points.windows(2).any(|w| w[1].0 >= w[0].0) {
        return Err("points must have falling voltage".into());
    }
    Ok(Curve {
        model: model.ok_or("no model line")?,
        points,
        runtime_tenths: runtime,
        source: Source::Measured,
        file: file.to_string(),
        session_end,
    })
}

// ---------------------------------------------------------------- lookup

/// The best curve for `model`: measured file, then a G HUB XML in `dir`, then built-in, then
/// the generic estimate.
pub fn find_curve(dir: &Path, model: u16) -> Curve {
    let m = measured_path(dir, model);
    if let Ok(text) = fs::read_to_string(&m) {
        match parse_measured(&text, &m.file_name().unwrap_or_default().to_string_lossy()) {
            Ok(c) if c.model == model => return c,
            Ok(_) => {}
            Err(e) => crate::verbose(format_args!("{}: {e}", m.display())),
        }
    }
    if let Some(c) = find_xml(dir, model) {
        return c;
    }
    if let Some((_, rt, pts)) = BUILTIN.iter().find(|b| b.0 == model) {
        return Curve {
            model,
            points: pts.to_vec(),
            runtime_tenths: Some(*rt),
            source: Source::BuiltIn,
            file: String::new(),
            session_end: 0,
        };
    }
    approx(model)
}

/// A G HUB power-model XML for `model` among the *.xml files in `dir` (over 4 MB ignored).
fn find_xml(dir: &Path, model: u16) -> Option<Curve> {
    for e in fs::read_dir(dir).ok()?.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("xml") || e.metadata().map_or(true, |m| m.len() > 4 << 20)
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match parse_ghub_xml(&text, &name) {
            Ok(c) if c.model == model => return Some(c),
            Ok(_) => {}
            Err(err) => crate::verbose(format_args!("{}: {err}", path.display())),
        }
    }
    None
}

/// Where G HUB keeps its downloaded device resources (power models among them).
pub const GHUB_DEPOTS: &str = r"C:\ProgramData\LGHUB\depots";

/// Look for a power model for `model` in a local G HUB install (`GHUB_DEPOTS`, at most three
/// folders deep, file names containing "battery"). Only used when the user asks to import one;
/// lghub-lite never reads G HUB's folders otherwise.
pub fn find_ghub_curve(model: u16) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u8, model: u16) -> Option<PathBuf> {
        for e in fs::read_dir(dir).ok()?.flatten() {
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth > 0
                    && let Some(p) = walk(&path, depth - 1, model)
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
            if let Ok(text) = fs::read_to_string(&path)
                && parse_ghub_xml(&text, &name).is_ok_and(|c| c.model == model)
            {
                return Some(path);
            }
        }
        None
    }
    walk(Path::new(GHUB_DEPOTS), 3, model)
}

/// Copy a power-model file into `dest_dir` (the user's own copy).
pub fn import_curve(src: &Path, dest_dir: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.join(src.file_name().unwrap_or_default());
    fs::copy(src, &dest)?;
    Ok(dest)
}

// ---------------------------------------------------------------- steady readings

/// A plausible single-cell reading. The headset sometimes answers 0 mV with flags 0x00, e.g.
/// while it reconnects [HW 2026-10-16]; such readings must not reach the display or the
/// recording (a 0 followed by a normal reading would even look like a charge).
pub fn valid_mv(mv: u16) -> bool {
    (2500..=5000).contains(&mv)
}

/// Steadies voltage readings for display. The voltage wanders by ±5–10 mV with the audio load
/// [HW: 3822–3840 mV within minutes], which on a steep curve flips the percentage up and down.
/// The shown value is the median of the last 10 minutes, and it only rises again by a clear
/// step (charging, or rest after a heavy load), so the percentage does not flicker.
#[derive(Debug, Default, Clone)]
pub struct Steady {
    recent: Vec<(u64, u16)>,
    shown: Option<u16>,
}

impl Steady {
    const WINDOW_S: u64 = 10 * 60;
    const KEEP: usize = 9;
    pub const RISE_MV: u16 = 15;

    /// Add a reading taken at unix time `t`; returns the value to show (None until a valid
    /// reading arrived).
    pub fn push(&mut self, t: u64, mv: u16) -> Option<u16> {
        if !valid_mv(mv) {
            return self.shown;
        }
        self.recent.push((t, mv));
        self.recent.retain(|r| r.0 + Self::WINDOW_S >= t);
        if self.recent.len() > Self::KEEP {
            self.recent.remove(0);
        }
        let mut v: Vec<u16> = self.recent.iter().map(|r| r.1).collect();
        v.sort_unstable();
        let median = v[(v.len() - 1) / 2];
        let shown = match self.shown {
            Some(s) if median > s && median < s + Self::RISE_MV => s,
            _ => median,
        };
        self.shown = Some(shown);
        self.shown
    }

    pub fn shown(&self) -> Option<u16> {
        self.shown
    }
}

// ---------------------------------------------------------------- voltage recording

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Unix time, seconds.
    pub t: u64,
    pub mv: u16,
}

fn log_path(dir: &Path, model: u16) -> PathBuf {
    dir.join(format!("voltage-{model:04x}.csv"))
}

const LOG_MAX: u64 = 2 << 20;

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Append one reading to `voltage-<model>.csv` (moved to `.old` once over 2 MB).
pub fn record(dir: &Path, model: u16, mv: u16, flags: u8) -> std::io::Result<()> {
    let path = log_path(dir, model);
    if fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = fs::rename(&path, path.with_extension("csv.old"));
    }
    let new = !path.exists();
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if new {
        writeln!(f, "# lghub-lite voltage recording: unix_time,mV,flags")?;
    }
    writeln!(f, "{},{mv},{flags:#04x}", unix_now())
}

pub fn read_log(dir: &Path, model: u16) -> Vec<Sample> {
    let Ok(text) = fs::read_to_string(log_path(dir, model)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split(',');
            Some(Sample {
                t: f.next()?.trim().parse().ok()?,
                mv: f.next()?.trim().parse().ok()?,
            })
        })
        .filter(|s| valid_mv(s.mv))
        .collect()
}

// ---------------------------------------------------------------- building a curve

/// A discharge counts as complete from at or above FULL down to at or below EMPTY, once
/// charging has started after it.
pub const FULL_MV: u16 = 4000;
pub const EMPTY_MV: u16 = 3550;
/// A rise of this much over the running minimum means charging: the discharge ended.
pub const CHARGE_RISE_MV: u16 = 60;
/// Readings further apart than this are not counted as use (headset off, PC asleep).
const MAX_GAP_S: u64 = 15 * 60;

/// Split the recording into discharges (a charging rise ends one) and return the newest
/// complete one as an index range. A discharge still running is never used: built early it
/// would end at whatever voltage was last read, and be counted again once it really ends.
fn latest_complete(samples: &[Sample]) -> Option<(usize, usize)> {
    let mut best = None;
    let mut start = 0;
    let mut min_mv = u16::MAX;
    for i in 0..samples.len() {
        if samples[i].mv > min_mv.saturating_add(CHARGE_RISE_MV) {
            if i > start {
                let s = &samples[start..i];
                let top = s.iter().map(|x| x.mv).max().unwrap_or(0);
                let low = s.iter().map(|x| x.mv).min().unwrap_or(u16::MAX);
                if top >= FULL_MV && low <= EMPTY_MV {
                    best = Some((start, i));
                }
            }
            start = i;
            min_mv = u16::MAX;
        }
        min_mv = min_mv.min(samples[i].mv);
    }
    best
}

/// Seconds of use up to each sample (gaps over MAX_GAP_S are not counted).
fn active_seconds(s: &[Sample]) -> Vec<u64> {
    let mut acc = 0;
    let mut out = Vec::with_capacity(s.len());
    for (i, x) in s.iter().enumerate() {
        if i > 0 {
            let dt = x.t.saturating_sub(s[i - 1].t);
            if dt <= MAX_GAP_S {
                acc += dt;
            }
        }
        out.push(acc);
    }
    out
}

/// Build a curve from the newest complete discharge in `samples`: for each 10 mV step, the
/// remaining charge is the share of the discharge's use time still ahead when the voltage
/// first reached that step. That assumes roughly constant power draw, which is how
/// manufacturers' curves are measured too.
pub fn build_from(samples: &[Sample], model: u16) -> Result<Curve, String> {
    let (a, b) = latest_complete(samples).ok_or_else(|| {
        format!(
            "no complete discharge recorded yet (needs one from {FULL_MV} mV or above down to {EMPTY_MV} mV or below, followed by charging)"
        )
    })?;
    let s = &samples[a..b];
    let act = active_seconds(s);
    let total = *act.last().unwrap_or(&0);
    if total < 3600 || s.len() < 20 {
        return Err("the recorded discharge is too short (needs at least 1 hour and 20 readings)".into());
    }
    let top = s.iter().map(|x| x.mv).max().unwrap_or(0) as u32 / 10 * 10;
    let low = s.iter().map(|x| x.mv).min().unwrap_or(0) as u32;
    let mut points = Vec::new();
    let mut v = top;
    while v >= low {
        if let Some(i) = s.iter().position(|x| x.mv as u32 <= v) {
            let left = total - act[i];
            points.push((v, (left * 1000 / total) as u32));
        }
        if v < 10 {
            break;
        }
        v -= 10;
    }
    // Make the remaining charge never increase as the voltage falls (noise guard).
    for i in 1..points.len() {
        points[i].1 = points[i].1.min(points[i - 1].1);
    }
    points.dedup_by_key(|p| p.0);
    if points.len() < 2 {
        return Err("not enough distinct voltages in the recorded discharge".into());
    }
    Ok(Curve {
        model,
        points,
        runtime_tenths: Some((total * 10 / 3600) as u32),
        source: Source::Measured,
        file: format!("curve-{model:04x}.txt"),
        session_end: s.last().map_or(0, |x| x.t),
    })
}

/// Average a new measurement into an older one (per 10 mV step and runtime), so the curve
/// follows the battery as it ages instead of jumping with every cycle.
pub fn merge(old: &Curve, new: &Curve) -> Curve {
    let mut volts: Vec<u32> = old.points.iter().chain(&new.points).map(|p| p.0).collect();
    volts.sort_unstable_by(|a, b| b.cmp(a));
    volts.dedup();
    let mut points: Vec<(u32, u32)> = volts
        .into_iter()
        .map(|v| (v, (old.permille(v as u16) + new.permille(v as u16)) / 2))
        .collect();
    for i in 1..points.len() {
        points[i].1 = points[i].1.min(points[i - 1].1);
    }
    Curve {
        model: new.model,
        points,
        runtime_tenths: match (old.runtime_tenths, new.runtime_tenths) {
            (Some(a), Some(b)) => Some((a + b) / 2),
            (a, b) => b.or(a),
        },
        source: Source::Measured,
        file: new.file.clone(),
        session_end: new.session_end,
    }
}

/// Build (or refine) `curve-<model>.txt` from the recording if it holds a complete discharge
/// newer than the saved curve. Returns the new curve when something changed.
pub fn update_measured(dir: &Path, model: u16) -> Result<Option<Curve>, String> {
    let samples = read_log(dir, model);
    let new = build_from(&samples, model)?;
    let path = measured_path(dir, model);
    let old = fs::read_to_string(&path).ok().and_then(|t| parse_measured(&t, "").ok());
    let curve = match old {
        Some(o) if o.session_end >= new.session_end => return Ok(None),
        Some(o) => merge(&o, &new),
        None => new,
    };
    crate::config::write_atomic(&path, &format_measured(&curve))?;
    Ok(Some(curve))
}

/// One line about the recording, for the app and `lghub-lite curve`.
pub fn recording_status(dir: &Path, model: u16) -> String {
    let s = read_log(dir, model);
    if s.is_empty() {
        return "no voltage recording yet; it starts while `watch`, the background watcher or the app runs".into();
    }
    let hours = active_seconds(&s).last().copied().unwrap_or(0) / 360;
    let top = s.iter().map(|x| x.mv).max().unwrap_or(0);
    let low = s.iter().map(|x| x.mv).min().unwrap_or(0);
    let complete = if latest_complete(&s).is_some() {
        "a complete discharge is recorded"
    } else {
        "no complete discharge yet"
    };
    format!(
        "{} readings over {}.{} h of use, {top}–{low} mV; {complete} (needs ≥ {FULL_MV} mV down to ≤ {EMPTY_MV} mV, then charging)",
        s.len(),
        hours / 10,
        hours % 10
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn ghub_xml_parse_and_lookup() {
        let c = parse_ghub_xml(SYNTHETIC, "synthetic.xml").unwrap();
        assert_eq!(c.model, 0x1234);
        assert_eq!(c.runtime_tenths, Some(100)); // 100 mAh / 10 mA = 10.0 h
        assert_eq!(c.percent(4200), 100);
        assert_eq!(c.percent(4050), 90);
        assert_eq!(c.percent(3800), 60);
        assert_eq!(c.percent(3700), 20);
        assert_eq!(c.percent(3000), 0);
        assert_eq!(c.tenths_of_hours_left(3800), Some(60));
        assert!(!c.is_approx());
    }

    #[test]
    fn ghub_xml_rejects_bad_files() {
        assert!(parse_ghub_xml("<foo/>", "x").is_err());
        assert!(
            parse_ghub_xml(
                r#"<powermodel pid="0x1"><discharge>1, 4.0, 0</discharge></powermodel>"#,
                "x"
            )
            .is_err()
        );
        let bad = "<powermodel pid=\"0x1\"><discharge>0, 3.5, 0\n1, 4.0, 5</discharge></powermodel>";
        assert!(parse_ghub_xml(bad, "x").unwrap_err().contains("order"));
        let bad = "<powermodel pid=\"0x1\"><discharge>0, 4.0, 5\n1, 3.5, 4</discharge></powermodel>";
        assert!(parse_ghub_xml(bad, "x").unwrap_err().contains("order"));
        let wiggle = "<powermodel pid=\"0x1\"><discharge>0, 4.000, 0\n1, 3.950, 10\n2, 3.951, 20\n3, 3.500, 100</discharge></powermodel>";
        assert_eq!(parse_ghub_xml(wiggle, "x").unwrap().percent(3951), 90);
        let no_blocks = "<powermodel pid=\"0x1\"><discharge>0, 4.0, 0\n1, 3.5, 50</discharge></powermodel>";
        let c = parse_ghub_xml(no_blocks, "x").unwrap();
        assert_eq!((c.percent(3600), c.tenths_of_hours_left(3600)), (0, None));
    }

    #[test]
    fn fixed_point_parsing() {
        assert_eq!(fixed("4.102", 3), Some(4102));
        assert_eq!(fixed(" 1271.357184 ", 3), Some(1_271_357));
        assert_eq!(fixed("53", 3), Some(53_000));
        assert_eq!(fixed(".25", 3), Some(250));
        for bad in ["", ".", "-1", "1e3", "4.1.2", "abc", "99999999999999999999"] {
            assert_eq!(fixed(bad, 3), None, "{bad}");
        }
    }

    #[test]
    fn generic_estimate_is_monotonic_and_marked() {
        let c = approx(0x0ABA);
        assert!(c.is_approx());
        assert_eq!(c.percent(4300), 100);
        assert_eq!(c.percent(3840), 50);
        assert_eq!(c.percent(3000), 0);
        assert!(GENERIC_LIION.windows(2).all(|w| w[1].0 < w[0].0 && w[1].1 < w[0].1));
        assert_eq!(c.tenths_of_hours_left(3900), None);
    }

    /// A synthetic discharge: 10 h of use, one reading every 5 min, voltage falling linearly
    /// from 4100 to 3500 mV, with a 3-hour pause (headset off) in the middle and a charge after.
    fn synthetic_discharge(t0: u64) -> Vec<Sample> {
        let mut v = Vec::new();
        let mut t = t0;
        for i in 0..=120u64 {
            if i == 60 {
                t += 3 * 3600; // pause: not counted as use
            }
            v.push(Sample {
                t,
                mv: (4100 - i * 5) as u16,
            });
            t += 300;
        }
        v.push(Sample { t, mv: 3800 }); // charging rise ends the discharge
        v
    }

    #[test]
    fn builds_curve_from_recorded_discharge() {
        let s = synthetic_discharge(1_000_000);
        let c = build_from(&s, 0x0ABA).unwrap();
        assert_eq!(c.source, Source::Measured);
        assert_eq!(c.runtime_tenths, Some(99)); // 119 counted 5-min steps = 9.9 h: the step over the pause is not use
        assert_eq!(c.percent(4100), 100);
        assert_eq!(c.percent(3800), 50); // half-way in use time
        assert_eq!(c.percent(3500), 0);
        assert_eq!(c.tenths_of_hours_left(3800), Some(49));
        assert!(c.points.windows(2).all(|w| w[1].0 < w[0].0 && w[1].1 <= w[0].1));
        // round trip through the file format
        let back = parse_measured(&format_measured(&c), "f").unwrap();
        assert_eq!(
            (back.model, back.runtime_tenths, back.points.len()),
            (0x0ABA, Some(99), c.points.len())
        );
        assert_eq!(back.percent(3800), 50);
    }

    #[test]
    fn incomplete_or_short_discharges_are_refused() {
        let mut s = synthetic_discharge(0);
        s.retain(|x| x.mv >= 3700); // never reached empty
        assert!(build_from(&s, 1).unwrap_err().contains("no complete discharge"));
        let mut running = synthetic_discharge(0);
        running.pop(); // reached empty, but charging has not started: still running
        assert!(build_from(&running, 1).unwrap_err().contains("no complete discharge"));
        let short: Vec<Sample> = (0..10)
            .map(|i| Sample {
                t: i * 300,
                mv: 4100 - i as u16 * 60,
            })
            .collect();
        assert!(build_from(&short, 1).is_err());
    }

    #[test]
    fn steady_ignores_bad_readings_and_noise() {
        let mut s = Steady::default();
        assert_eq!(s.push(0, 0), None); // nothing valid yet
        // the recorded noise from the headset [HW]: no flicker upwards
        let seq = [3836, 3831, 3832, 3827, 3823, 3826, 3826, 3824, 3826];
        let mut out = Vec::new();
        for (i, mv) in seq.into_iter().enumerate() {
            out.push(s.push(i as u64 * 60, mv).unwrap());
        }
        assert!(out.windows(2).all(|w| w[1] <= w[0]), "{out:?}");
        // a 0 mV reading keeps the last value
        assert_eq!(s.push(600, 0), s.shown());
        // a clear rise (charging) shows once it is the median
        s.push(2000, 3900);
        assert_eq!(s.push(2010, 3905), Some(3900));
        // one low outlier does not drag the value down
        s.push(2020, 3902);
        assert_eq!(s.push(2030, 3700), Some(3900));
        assert!(!valid_mv(0) && valid_mv(3800) && !valid_mv(65535));
    }

    #[test]
    fn merging_averages_old_and_new() {
        let a = build_from(&synthetic_discharge(0), 1).unwrap();
        let mut b = a.clone();
        b.runtime_tenths = Some(81); // an aged battery: shorter runtime
        b.session_end += 1;
        let m = merge(&a, &b);
        assert_eq!(m.runtime_tenths, Some(90));
        assert_eq!(m.percent(3800), 50);
        assert_eq!(m.session_end, b.session_end);
    }

    #[test]
    fn record_read_update_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("lghub-lite-batt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // find_curve falls back to the estimate when nothing is there
        assert!(find_curve(&dir, 0x1234).is_approx());
        // a G HUB XML in the folder is used next
        fs::write(dir.join("model.xml"), SYNTHETIC).unwrap();
        assert_eq!(find_curve(&dir, 0x1234).source, Source::GHub);
        // a measured curve wins over both
        let lines: String = synthetic_discharge(1_000)
            .iter()
            .map(|s| format!("{},{},0x01\n", s.t, s.mv))
            .collect();
        fs::write(log_path(&dir, 0x1234), format!("# header\n{lines}")).unwrap();
        assert_eq!(read_log(&dir, 0x1234).len(), 122);
        assert!(update_measured(&dir, 0x1234).unwrap().is_some());
        assert!(update_measured(&dir, 0x1234).unwrap().is_none()); // same discharge: no change
        assert_eq!(find_curve(&dir, 0x1234).source, Source::Measured);
        record(&dir, 0x1234, 4000, 1).unwrap();
        assert_eq!(read_log(&dir, 0x1234).len(), 123);
        assert!(recording_status(&dir, 0x1234).contains("a complete discharge is recorded"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
