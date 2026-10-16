//! Logitech "Centurion" protocol, used by newer headsets such as the PRO X 2 LIGHTSPEED
//! (USB 046d:0af7) instead of plain HID++: HID++-style features (Root, feature indices,
//! functions) inside 64-byte frames with report ID 0x51, on vendor page 0xFFA0 usage 1 of
//! interface 3. The headset sits behind a "bridge" feature (0x0003) of its dongle, so requests
//! for the headset are wrapped in a bridge message.
//!
//! [PUB] Protocol facts from HeadsetControl (github.com/Sapd/HeadsetControl, GPL-3.0),
//! `lib/devices/protocols/logitech_centurion_protocol.hpp` and
//! `lib/devices/logitech_gpro_x2_lightspeed.hpp`: read, described, re-implemented here; no
//! code copied. [INF] Untested on hardware by us (no such headset here). Read-only: only Root
//! GetFeature lookups and the battery read are sent.
//!
//! Frame (1-byte prefix): `[0x51, length, flags, payload…]`, zero-padded to 64 bytes.
//! - direct (to the dongle): payload `[feature index, function<<4 | swid 1, params…]`, length
//!   byte 0x41 (HeadsetControl's default: the full buffer + 1); reply `[0x51, len, flags,
//!   index, function|swid, data…]`.
//! - bridged (to the headset): payload `[bridge index, 0x11 (send, swid 1), size hi nibble,
//!   size lo, 0x00, sub index, function<<4 | swid 1, params…]`, length = payload + 1. The dongle
//!   first acknowledges (byte 4 low nibble = swid 1), then the answer comes as an event
//!   (low nibble 0): byte 7 status (0 = ok), byte 8 the sub index echoed (0xFF = rejected),
//!   data from byte 10.

use std::time::{Duration, Instant};

use crate::hid;
use crate::hidpp::{Error, hex};

pub const PAGE: u16 = 0xFFA0;
const FRAME: usize = 64;
const PREFIX: u8 = 0x51;
const SWID: u8 = 0x01;
const BRIDGE_SEND: u8 = 0x11;

pub const FEATURE_BRIDGE: u16 = 0x0003;
pub const FEATURE_BATTERY: u16 = 0x0104;
/// Headset features HeadsetControl names, for the report's lookup table. [PUB]
pub const KNOWN: [(u16, &str); 7] = [
    (0x0104, "BatterySoC"),
    (0x0108, "AutoSleep"),
    (0x0601, "MicMuteLED"),
    (0x0604, "Sidetone"),
    (0x060B, "VoicePrompts"),
    (0x0636, "OnboardEQ"),
    (0x020D, "AdvancedParaEQ"),
];

/// Is this the Centurion collection (page 0xFFA0, usage 1, 64-byte reports)?
pub fn is_centurion(i: &hid::Info) -> bool {
    i.usage_page == PAGE && i.usage == 1 && i.out_len as usize == FRAME
}

/// Centurion interfaces present: (pid, product, collection).
pub fn interfaces() -> Vec<(u16, String, hid::Info)> {
    hid::enumerate(crate::LOGITECH_VID)
        .unwrap_or_default()
        .into_iter()
        .filter(is_centurion)
        .map(|i| (i.pid, i.product.clone(), i))
        .collect()
}

pub fn direct_frame(index: u8, function: u8, params: &[u8]) -> [u8; FRAME] {
    let mut f = [0u8; FRAME];
    f[0] = PREFIX;
    f[1] = (FRAME + 1) as u8; // 0x41
    f[3] = index;
    f[4] = (function << 4) | SWID;
    let n = params.len().min(FRAME - 5);
    f[5..5 + n].copy_from_slice(&params[..n]);
    f
}

pub fn bridged_frame(bridge: u8, sub_index: u8, function: u8, params: &[u8]) -> [u8; FRAME] {
    let params = &params[..params.len().min(FRAME - 10)];
    let sub_len = 3 + params.len(); // [0x00, sub index, function|swid, params…]
    let mut f = [0u8; FRAME];
    f[0] = PREFIX;
    f[1] = (4 + sub_len + 1) as u8;
    f[3] = bridge;
    f[4] = BRIDGE_SEND;
    f[5] = ((sub_len >> 8) & 0x0F) as u8;
    f[6] = sub_len as u8;
    f[7] = 0x00;
    f[8] = sub_index;
    f[9] = (function << 4) | SWID;
    f[10..10 + params.len()].copy_from_slice(params);
    f
}

/// A direct reply to a request for `index`: its data, or an HID++-style error
/// (`[0xFF, index, function, code]` after the header), or None for unrelated frames.
fn parse_direct(f: &[u8], index: u8) -> Option<Result<Vec<u8>, Error>> {
    if f.len() < 6 || f[0] != PREFIX || f[1] < 2 {
        return None;
    }
    if f[3] == 0xFF && f[4] == index {
        return Some(Err(Error::Hpp20(*f.get(6).unwrap_or(&0))));
    }
    (f[3] == index).then(|| Ok(f[5..].to_vec()))
}

enum Bridged {
    Ack,
    Data(Vec<u8>),
    Rejected(u8),
    Unrelated,
}

fn parse_bridged(f: &[u8], bridge: u8, sub_index: u8) -> Bridged {
    if f.len() < 10 || f[0] != PREFIX || f[3] != bridge {
        return Bridged::Unrelated;
    }
    if f[4] & 0x0F == SWID {
        return Bridged::Ack;
    }
    if f[8] == 0xFF || f[7] != 0 {
        return Bridged::Rejected(f[7]);
    }
    if f[8] == sub_index {
        Bridged::Data(f[10..].to_vec())
    } else {
        Bridged::Unrelated
    }
}

/// An open Centurion channel to a dongle and the headset behind it.
pub struct Centurion {
    h: hid::Handle,
    pub trace: bool,
    pub timeout: Duration,
    bridge: Option<u8>,
}

impl Centurion {
    pub fn open(info: &hid::Info, trace: bool) -> Result<Centurion, Error> {
        let h = hid::Handle::open(info).map_err(|e| Error::Io(format!("open {}: {e}", info.path)))?;
        Ok(Centurion {
            h,
            trace,
            timeout: Duration::from_millis(2000),
            bridge: None,
        })
    }

    fn send(&self, f: &[u8]) -> Result<(), Error> {
        if self.trace {
            eprintln!("-> {}", hex(&f[..16]));
        }
        self.h.write(f, 1000).map_err(|e| Error::Io(e.to_string()))
    }

    fn read(&mut self, deadline: Instant) -> Result<Option<Vec<u8>>, Error> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        let ms = left.as_millis().min(u32::MAX as u128 - 1) as u32;
        let r = hid::read_any(&mut [&mut self.h], ms).map_err(|e| Error::Io(e.to_string()))?;
        let r = r.map(|(_, b)| b);
        if self.trace
            && let Some(b) = &r
        {
            eprintln!("<- {}", hex(&b[..b.len().min(16)]));
        }
        Ok(r)
    }

    /// A request to the dongle itself.
    pub fn direct(&mut self, index: u8, function: u8, params: &[u8]) -> Result<Vec<u8>, Error> {
        self.send(&direct_frame(index, function, params))?;
        let deadline = Instant::now() + self.timeout;
        while let Some(f) = self.read(deadline)? {
            if let Some(r) = parse_direct(&f, index) {
                return r;
            }
        }
        Err(Error::Timeout)
    }

    /// The dongle's bridge feature index (looked up once).
    pub fn bridge(&mut self) -> Result<u8, Error> {
        if let Some(b) = self.bridge {
            return Ok(b);
        }
        let r = self.direct(0, 0, &FEATURE_BRIDGE.to_be_bytes())?;
        match r.first() {
            Some(&i) if i != 0 => {
                self.bridge = Some(i);
                Ok(i)
            }
            _ => Err(Error::NoFeature(FEATURE_BRIDGE)),
        }
    }

    /// A request to the headset behind the bridge.
    pub fn headset(&mut self, sub_index: u8, function: u8, params: &[u8]) -> Result<Vec<u8>, Error> {
        let bridge = self.bridge()?;
        self.send(&bridged_frame(bridge, sub_index, function, params))?;
        let deadline = Instant::now() + self.timeout;
        while let Some(f) = self.read(deadline)? {
            match parse_bridged(&f, bridge, sub_index) {
                Bridged::Data(d) => return Ok(d),
                Bridged::Rejected(code) => return Err(Error::Hpp20(code)),
                Bridged::Ack | Bridged::Unrelated => {}
            }
        }
        Err(Error::Timeout)
    }

    /// The headset's index for feature `id` (Root GetFeature through the bridge).
    pub fn feature(&mut self, id: u16) -> Result<u8, Error> {
        let r = self.headset(0, 0, &id.to_be_bytes())?;
        match r.first() {
            Some(&i) if i != 0 => Ok(i),
            _ => Err(Error::NoFeature(id)),
        }
    }

    /// Battery: (percent, charging). [PUB] reply byte 0 = percent, byte 2 = charging state
    /// (1 or 2 = charging).
    pub fn battery(&mut self) -> Result<Battery, Error> {
        let i = self.feature(FEATURE_BATTERY)?;
        parse_battery(&self.headset(i, 0, &[])?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

impl std::fmt::Display for Battery {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let state = if self.charging { "charging" } else { "discharging" };
        write!(f, "{}% {state}", self.percent)
    }
}

pub fn parse_battery(d: &[u8]) -> Result<Battery, Error> {
    match d {
        [pct, _, state, ..] if *pct <= 100 => Ok(Battery {
            percent: *pct,
            charging: matches!(state, 1 | 2),
        }),
        _ => Err(Error::BadReply("battery reply out of range")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_follow_the_published_layout() {
        // direct Root GetFeature(0x0003) to the dongle
        let d = direct_frame(0, 0, &[0x00, 0x03]);
        assert_eq!(&d[..7], &[0x51, 0x41, 0x00, 0x00, 0x01, 0x00, 0x03]);
        assert!(d[7..].iter().all(|&b| b == 0));
        // bridged Root GetFeature(0x0104) to the headset, bridge at index 2
        let b = bridged_frame(2, 0, 0, &[0x01, 0x04]);
        assert_eq!(
            &b[..12],
            &[0x51, 0x0A, 0x00, 0x02, 0x11, 0x00, 0x05, 0x00, 0x00, 0x01, 0x01, 0x04]
        );
        assert!(b[12..].iter().all(|&x| x == 0));
    }

    #[test]
    fn replies_ack_data_and_errors() {
        let mut ok = [0u8; 64];
        ok[..6].copy_from_slice(&[0x51, 0x08, 0x00, 0x00, 0x01, 0x02]);
        assert_eq!(parse_direct(&ok, 0).unwrap().unwrap()[0], 0x02);
        assert!(parse_direct(&ok, 5).is_none());
        let mut err = [0u8; 64];
        err[..7].copy_from_slice(&[0x51, 0x08, 0x00, 0xFF, 0x00, 0x01, 0x05]);
        assert!(matches!(parse_direct(&err, 0), Some(Err(Error::Hpp20(5)))));

        let mut ack = [0u8; 64];
        ack[..5].copy_from_slice(&[0x51, 0x05, 0x00, 0x02, 0x11]);
        assert!(matches!(parse_bridged(&ack, 2, 7), Bridged::Ack));
        let mut data = [0u8; 64];
        data[..13].copy_from_slice(&[
            0x51, 0x0C, 0x00, 0x02, 0x10, 0x00, 0x06, 0x00, 0x07, 0x01, 0x4B, 0x00, 0x02,
        ]);
        match parse_bridged(&data, 2, 7) {
            Bridged::Data(d) => assert_eq!(
                parse_battery(&d).unwrap(),
                Battery {
                    percent: 75,
                    charging: true
                }
            ),
            _ => panic!("expected data"),
        }
        data[8] = 0xFF;
        assert!(matches!(parse_bridged(&data, 2, 7), Bridged::Rejected(_)));
        assert!(matches!(parse_bridged(&data, 3, 7), Bridged::Unrelated));
        assert!(parse_battery(&[101, 0, 0]).is_err());
    }

    #[test]
    fn only_the_ffa0_collection_matches() {
        let info = |page, usage, out| hid::Info {
            path: String::new(),
            vid: 0x46D,
            pid: 0x0AF7,
            usage_page: page,
            usage,
            in_len: out,
            out_len: out,
            product: "PRO X 2 LIGHTSPEED".into(),
            iface: String::new(),
        };
        // [HW friend's report] 0af7: FFA0/1 64/64 yes; FF13/1 62/62 and 000C/1 no
        assert!(is_centurion(&info(0xFFA0, 1, 64)));
        assert!(!is_centurion(&info(0xFF13, 1, 62)));
        assert!(!is_centurion(&info(0x000C, 1, 0)));
    }
}
