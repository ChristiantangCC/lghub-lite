//! Live window and tray icons, drawn from the battery levels (pixel art from the "Battery
//! Tracker" design, redrawn in code so it can follow the devices).
//!
//! * Window / taskbar: a battery with four cells for the device with the lowest charge;
//!   one cell per started 25 %, red at or under that device's alert level.
//! * Tray: two bars, one per device: fill = charge, green, red at or under the alert level,
//!   a grey checkerboard when the device is offline or missing.
//!
//! The design's icons are exact nearest-neighbour enlargements of a 32 px battery and a 16 px
//! tray image, so those two are drawn and then scaled to the size Windows asks for.

/// A device's battery for the icons: None = offline / no device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub pct: u8,
    pub low: bool,
}

type Rgba = [u8; 4];

const CLEAR: Rgba = [0, 0, 0, 0];
const BLACK: Rgba = [0, 0, 0, 255];
const TRACK: Rgba = [64, 64, 64, 255];
const CHECK: Rgba = [128, 128, 128, 255];
const OK_HI: Rgba = [144, 255, 144, 255];
const OK: Rgba = [0, 208, 0, 255];
const LOW_HI: Rgba = [255, 144, 144, 255];
const LOW: Rgba = [255, 0, 0, 255];

/// The design's 32 px battery ("app-3c-32.png") with the four cells left empty; `A`–`H` are
/// its colours, `.` is transparent.
const APP: [&str; 32] = [
    "................................",
    "................................",
    "...........AAAAAAAAAA...........",
    "...........ABBBBBBBCA...........",
    "...........ABDDDDDDCA...........",
    "...........ABDDDDDDCA...........",
    "......AAAAAAAAAAAAAAAAAAAA......",
    "......ABBBBBBBBBBBBBBBBBCA......",
    "......ABDDDDDDDDDDDDDDDDCA......",
    "......ABDCCCCCCCCCCCCCBDCA......",
    "......ABDCEEEEEEEEEEEEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEEEEEEEEEEEEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEEEEEEEEEEEEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEEEEEEEEEEEEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEFFFFFFFFFFEBDCA......",
    "......ABDCEEEEEEEEEEEEBDCA......",
    "......ABDBBBBBBBBBBBBBBDCA......",
    "......ABDDDDDDDDDDDDDDDDCA......",
    "......ACCCCCCCCCCCCCCCCCCA......",
    "......AAAAAAAAAAAAAAAAAAAA......",
    "................................",
];

fn app_colour(c: u8) -> Rgba {
    match c {
        b'A' => BLACK,
        b'B' => [108, 112, 122, 255],
        b'C' => [20, 21, 24, 255],
        b'D' => [58, 61, 68, 255],
        b'E' => [5, 8, 6, 255],
        b'F' => [29, 58, 40, 255],
        _ => CLEAR,
    }
}

/// Lit cells for a charge: one per started 25 %.
pub fn cells(pct: u8) -> usize {
    (pct as usize).div_ceil(25).min(4)
}

/// The 32 px window icon for the lowest battery (None: no device).
fn app32(lowest: Option<Level>) -> Vec<Rgba> {
    let mut px: Vec<Rgba> = APP.iter().flat_map(|r| r.bytes().map(app_colour)).collect();
    let (lit, low) = lowest.map_or((0, false), |l| (cells(l.pct), l.low));
    let (hi, fill) = if low {
        (LOW_HI, LOW)
    } else {
        ([191, 245, 207, 255], [61, 220, 132, 255])
    };
    // cells from the bottom: rows 23–25, 19–21, 15–17, 11–13; x 11..=20
    for k in 0..lit {
        let top = 23 - 4 * k;
        for (dy, c) in [(0, hi), (1, fill), (2, fill)] {
            for x in 11..=20 {
                px[(top + dy) * 32 + x] = c;
            }
        }
    }
    px
}

/// The 16 px tray image: `top` and `bottom` bars.
fn tray16(top: Option<Level>, bottom: Option<Level>) -> Vec<Rgba> {
    let mut px = vec![CLEAR; 16 * 16];
    for (y0, level) in [(1usize, top), (9, bottom)] {
        // 6 rows: border, highlight, 3 × fill, border; columns 0 and 15 are border
        for y in y0..y0 + 6 {
            for x in 0..16 {
                let edge = y == y0 || y == y0 + 5 || x == 0 || x == 15;
                px[y * 16 + x] = if edge { BLACK } else { TRACK };
            }
        }
        match level {
            Some(l) => {
                let fill = if l.pct == 0 {
                    0
                } else {
                    ((l.pct as usize * 14 + 50) / 100).clamp(1, 14)
                };
                let (hi, body) = if l.low { (LOW_HI, LOW) } else { (OK_HI, OK) };
                for x in 1..1 + fill {
                    px[(y0 + 1) * 16 + x] = hi;
                    for y in y0 + 2..y0 + 5 {
                        px[y * 16 + x] = body;
                    }
                }
            }
            None => {
                for y in y0 + 1..y0 + 5 {
                    for x in 1..15 {
                        px[y * 16 + x] = if (x - 1 + y - y0 - 1) % 2 == 0 { TRACK } else { CHECK };
                    }
                }
            }
        }
    }
    px
}

/// Nearest-neighbour scale of a square image to `size`, as RGBA bytes.
fn scale(src: &[Rgba], from: usize, size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            out.extend_from_slice(&src[(y * from / size) * from + x * from / size]);
        }
    }
    out
}

pub fn app(size: u32, lowest: Option<Level>) -> Vec<u8> {
    scale(&app32(lowest), 32, size as usize)
}

pub fn tray(size: u32, top: Option<Level>, bottom: Option<Level>) -> Vec<u8> {
    scale(&tray16(top, bottom), 16, size as usize)
}

/// Icon sizes Windows uses at the system DPI: (tray / small, window / taskbar).
pub fn sizes() -> (u32, u32) {
    use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SM_CXICON, SM_CXSMICON};
    // SAFETY: plain queries without pointers.
    unsafe {
        let dpi = GetDpiForSystem();
        let small = GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16) as u32;
        let big = GetSystemMetricsForDpi(SM_CXICON, dpi).max(32) as u32;
        (small, big)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes the generated icons (raw RGBA) for a byte-for-byte check against the design's
    /// PNGs, when LGHUB_LITE_ICON_DUMP names a folder (see NOTES.md).
    #[test]
    fn dump_for_comparison() {
        let Ok(dir) = std::env::var("LGHUB_LITE_ICON_DUMP") else {
            return;
        };
        let ok = |pct| Some(Level { pct, low: false });
        let low = |pct| Some(Level { pct, low: true });
        let cases: [(&str, Vec<u8>); 4] = [
            ("app-3c-32", app(32, ok(75))),
            ("tray-4b-ok-ok-16", tray(16, ok(50), ok(64))),
            ("tray-4b-low-ok-32", tray(32, low(14), ok(64))),
            ("tray-4b-off-ok-48", tray(48, None, ok(64))),
        ];
        for (name, px) in cases {
            std::fs::write(format!("{dir}/{name}.rgba"), px).unwrap();
        }
    }

    #[test]
    fn cells_and_bars_follow_the_level() {
        assert_eq!((cells(0), cells(1), cells(25), cells(26), cells(100)), (0, 1, 1, 2, 4));
        let full = app32(Some(Level { pct: 100, low: false }));
        assert_eq!(full[11 * 32 + 11], [191, 245, 207, 255]); // top cell lit
        let empty = app32(None);
        assert_eq!(empty[24 * 32 + 15], [29, 58, 40, 255]); // bottom cell dark
        let t = tray16(Some(Level { pct: 100, low: false }), None);
        assert_eq!((t[3 * 16 + 14], t[11 * 16 + 1], t[11 * 16 + 2]), (OK, CHECK, TRACK));
        assert_eq!(tray(48, None, None).len(), 48 * 48 * 4);
    }
}
