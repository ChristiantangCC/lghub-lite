//! Per-model DPI / report-rate presets, applied by `apply` and by `watch` on every connect.
//!
//! File format, one device model per line (model ID in hex, from FirmwareInfo):
//! ```text
//! # comment
//! 40a9 dpi=1600 rate=1000
//! ```

use std::path::{Path, PathBuf};

use crate::device;
use crate::hidpp::{Error, Hidpp};

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub model: u16,
    pub dpi: Option<u16>,
    pub rate: Option<u32>,
}

/// `presets.txt` in the settings folder: `%APPDATA%\lghub-lite` on Windows,
/// `$XDG_CONFIG_HOME/lghub-lite` (or `~/.config/lghub-lite`) on Linux,
/// `~/Library/Application Support/lghub-lite` on macOS.
pub fn default_path() -> Option<PathBuf> {
    crate::platform::config_dir().map(|d| d.join("presets.txt"))
}

/// A report rate the HID++ 0x8061 bitmap can express (125 << bit, bit 0..=6). [PUB]
fn valid_rate(hz: u32) -> bool {
    (0..=6).any(|b| device::rate_hz(b) == hz)
}

pub fn parse(text: &str) -> Result<Vec<Preset>, String> {
    let mut out: Vec<Preset> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let line = line.split('#').next().unwrap_or("").trim();
        let mut words = line.split_whitespace();
        let Some(model) = words.next() else { continue };
        let model = u16::from_str_radix(model, 16).map_err(|_| format!("line {n}: `{model}` is not a hex model ID"))?;
        if out.iter().any(|p| p.model == model) {
            return Err(format!("line {n}: model {model:04x} listed twice"));
        }
        let mut p = Preset {
            model,
            dpi: None,
            rate: None,
        };
        for w in words {
            let (k, v) = w
                .split_once('=')
                .ok_or_else(|| format!("line {n}: expected key=value, got `{w}`"))?;
            match k {
                "dpi" => p.dpi = Some(v.parse().map_err(|_| format!("line {n}: bad dpi `{v}`"))?),
                "rate" => {
                    let hz: u32 = v.parse().map_err(|_| format!("line {n}: bad rate `{v}`"))?;
                    if !valid_rate(hz) {
                        return Err(format!(
                            "line {n}: rate must be 125, 250, 500, 1000, 2000, 4000 or 8000"
                        ));
                    }
                    p.rate = Some(hz);
                }
                _ => return Err(format!("line {n}: unknown key `{k}` (use dpi= or rate=)")),
            }
        }
        out.push(p);
    }
    Ok(out)
}

/// One preset as a file line, e.g. `40a9 dpi=1600 rate=1000`.
pub fn line(p: &Preset) -> String {
    let mut s = format!("{:04x}", p.model);
    if let Some(d) = p.dpi {
        s.push_str(&format!(" dpi={d}"));
    }
    if let Some(r) = p.rate {
        s.push_str(&format!(" rate={r}"));
    }
    s
}

pub fn format(presets: &[Preset]) -> String {
    let mut s =
        String::from("# lghub-lite presets: <model id> [dpi=N] [rate=Hz]\n# Rewritten by `lghub-lite preset`.\n");
    for p in presets {
        s.push_str(&line(p));
        s.push('\n');
    }
    s
}

/// Load presets; a missing file means no presets.
pub fn load(path: &Path) -> Result<Vec<Preset>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => parse(&t).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn save(path: &Path, presets: &[Preset]) -> Result<(), String> {
    write_atomic(path, &format(presets))
}

/// Atomic write: write a temp file next to the target, then rename over it, so a crash never
/// leaves a half-written file. Used for presets and onboard memory dumps.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, contents).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Apply a preset: switch to host mode if the device is in onboard mode (writes are refused
/// there [HW]), then set DPI and report rate if they differ, verifying each by read-back.
/// A rate the active connection does not offer is skipped (e.g. 2000 Hz over the cable).
/// Returns a summary of what was done.
pub fn apply(h: &mut Hidpp, dev: u8, p: &Preset) -> Result<String, Error> {
    let mut done = Vec::new();
    if p.dpi.is_none() && p.rate.is_none() {
        return Ok("nothing to apply".into());
    }
    if let Some(m) = device::onboard_mode(h, dev)?
        && m != device::MODE_HOST
    {
        device::set_onboard_mode(h, dev, device::MODE_HOST)?;
        done.push("host mode".to_string());
    }
    if let Some(v) = p.dpi {
        let d = device::dpi(h, dev)?;
        if d.current != v || d.y.is_some_and(|y| y != v) {
            device::set_dpi(h, dev, &d, v)?;
            let got = device::dpi(h, dev)?.current;
            if got != v {
                return Err(Error::Other(format!("dpi read-back {got}, wanted {v}")));
            }
            done.push(format!("dpi {v}"));
        } else {
            done.push(format!("dpi {v} (already)"));
        }
    }
    if let Some(hz) = p.rate {
        let r = device::report_rate(h, dev)?;
        if device::rate_bit(&r, hz).is_none() {
            done.push(format!("rate {hz} not offered on this connection, skipped"));
        } else if device::rate_hz(r.current) != hz {
            device::set_report_rate(h, dev, &r, hz)?;
            let got = device::rate_hz(device::report_rate(h, dev)?.current);
            if got != hz {
                return Err(Error::Other(format!("rate read-back {got}, wanted {hz}")));
            }
            done.push(format!("rate {hz}"));
        } else {
            done.push(format!("rate {hz} (already)"));
        }
    }
    Ok(done.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats() {
        let t = "# hi\n\n40a9 dpi=1600 rate=1000  # SL2\n0aba\nc09b rate=500\n";
        let p = parse(t).unwrap();
        assert_eq!(
            p,
            [
                Preset {
                    model: 0x40A9,
                    dpi: Some(1600),
                    rate: Some(1000)
                },
                Preset {
                    model: 0x0ABA,
                    dpi: None,
                    rate: None
                },
                Preset {
                    model: 0xC09B,
                    dpi: None,
                    rate: Some(500)
                },
            ]
        );
        assert_eq!(parse(&format(&p)).unwrap(), p);
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(parse("zz dpi=1").unwrap_err().contains("line 1"));
        assert!(parse("40a9 dpi=abc").is_err());
        assert!(parse("40a9 dpi=70000").is_err()); // > u16
        assert!(parse("40a9 rate=300").unwrap_err().contains("rate must be"));
        assert!(parse("40a9 speed=3").unwrap_err().contains("unknown key"));
        assert!(parse("40a9 dpi").is_err());
        assert!(parse("40a9\n40A9").unwrap_err().contains("twice"));
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("lghub-lite-test-{}", std::process::id()));
        let path = dir.join("presets.txt");
        let p = vec![Preset {
            model: 0x40A9,
            dpi: Some(800),
            rate: Some(1000),
        }];
        save(&path, &p).unwrap();
        assert_eq!(load(&path).unwrap(), p);
        assert!(!dir.join("presets.txt.tmp").exists());
        // overwrite existing file
        let q = vec![Preset {
            model: 0x40A9,
            dpi: Some(400),
            rate: None,
        }];
        save(&path, &q).unwrap();
        assert_eq!(load(&path).unwrap(), q);
        assert_eq!(load(&dir.join("missing.txt")).unwrap(), []);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
