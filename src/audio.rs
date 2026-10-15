//! Headset features: 0x1F20 ADC measurement (battery voltage), 0x8300 sidetone, 0x8310
//! hardware equalizer. Layouts were read from G HUB's own request code (described in
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

/// Voltage -> percent for the PRO X Wireless headset (model 0aba), piecewise linear.
/// Our own table, NOT G HUB's curve file: ends are generic single-cell Li-ion values [PUB]
/// (4.2 V full, 3.3 V empty); inner points are our measurements paired with the percentage
/// G HUB showed at the same moment (tools/battery-calibration.csv). Discharging only; while
/// charging the voltage reads high, so the estimate is too. An estimate, labelled as such.
const PRO_X_WIRELESS_CURVE: [(u16, u8); 3] = [(3300, 0), (3919, 80), (4200, 100)];

pub fn estimate_percent(model: u16, mv: u16) -> Option<u8> {
    let curve: &[(u16, u8)] = match model {
        0x0ABA => &PRO_X_WIRELESS_CURVE,
        _ => return None,
    };
    let (first, last) = (curve[0], curve[curve.len() - 1]);
    if mv <= first.0 {
        return Some(first.1);
    }
    if mv >= last.0 {
        return Some(last.1);
    }
    let w = curve.windows(2).find(|w| mv >= w[0].0 && mv <= w[1].0)?;
    let ((v0, p0), (v1, p1)) = (w[0], w[1]);
    let p = p0 as u32 + (mv - v0) as u32 * (p1 - p0) as u32 / (v1 - v0) as u32;
    Some(p as u8)
}

// ---------------------------------------------------------------- 0x8300 sidetone

/// f0 GetSidetoneLevel -> reply[0]. [OBS 0x140d23a70] [HW headset: 0, matching G HUB gain 0]
pub fn sidetone(h: &mut Hidpp, dev: u8) -> Result<u8, Error> {
    let fi = h.feature(dev, 0x8300)?;
    let p = h.call(dev, fi, 0, &[], &[])?;
    need(&p, 1)?;
    Ok(p[0])
}

/// Largest sidetone level we send. G HUB treats it as a 0-100 gain [INF from the agent's
/// `Volume.gain` field]; UNVERIFIED beyond what is logged in NOTES.md.
pub const SIDETONE_MAX: u8 = 100;

/// f1 SetSidetoneLevel [level]. [OBS 0x140d23bb0: one parameter byte, reply unused]
pub fn set_sidetone(h: &mut Hidpp, dev: u8, level: u8) -> Result<(), Error> {
    if level > SIDETONE_MAX {
        return Err(Error::Other(format!("sidetone level must be 0..={SIDETONE_MAX}")));
    }
    let fi = h.feature(dev, 0x8300)?;
    h.call(dev, fi, 1, &[level], &[])?;
    Ok(())
}

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

    #[test]
    fn battery_estimate() {
        assert_eq!(estimate_percent(0x0ABA, 3919), Some(80)); // the calibration point itself
        assert_eq!(estimate_percent(0x0ABA, 4300), Some(100));
        assert_eq!(estimate_percent(0x0ABA, 3000), Some(0));
        assert_eq!(estimate_percent(0x0ABA, 4060), Some(90));
        assert_eq!(estimate_percent(0x40A9, 3900), None); // no curve for other models
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
