//! Battery percentage history per device, for the 24-hour bars and the drain rate.
//! Kept in `<preset folder>\history-<device>.csv` as `unix_time,percent` lines.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub const BARS: usize = 24;
const HOUR: u64 = 3600;
/// Keep two days in memory; the file is trimmed to that once it grows past 64 KB.
const KEEP: u64 = 48 * HOUR;
const FILE_MAX: u64 = 64 << 10;
/// Unchanged readings are still stored this often, so the bars stay filled.
const REPEAT: u64 = 30 * 60;

pub fn now() -> u64 {
    lghub_lite::battery::unix_now()
}

pub struct History {
    path: Option<PathBuf>,
    samples: Vec<(u64, u8)>,
}

impl History {
    pub fn load(dir: &Path, sel: &str) -> History {
        let path = dir.join(format!("history-{}.csv", sel.replace(':', "-")));
        let cut = now().saturating_sub(KEEP);
        let samples = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (t, p) = l.split_once(',')?;
                Some((t.trim().parse().ok()?, p.trim().parse::<u8>().ok()?.min(100)))
            })
            .filter(|s: &(u64, u8)| s.0 >= cut)
            .collect();
        History {
            path: Some(path),
            samples,
        }
    }

    /// A history that is not stored (demo mode, tests).
    pub fn memory(samples: Vec<(u64, u8)>) -> History {
        History { path: None, samples }
    }

    /// Store a reading when it differs from the last one or the last is older than REPEAT.
    pub fn add(&mut self, t: u64, pct: u8) {
        if self
            .samples
            .last()
            .is_some_and(|&(lt, lp)| lp == pct && t < lt + REPEAT)
        {
            return;
        }
        self.samples.push((t, pct.min(100)));
        let cut = t.saturating_sub(KEEP);
        self.samples.retain(|s| s.0 >= cut);
        if let Some(p) = &self.path {
            let _ = append(p, t, pct, &self.samples);
        }
    }

    /// One value per hour for the last 24 hours (oldest first): the last reading in or before
    /// that hour. None before the first reading.
    pub fn bars(&self, now: u64) -> [Option<u8>; BARS] {
        let mut out = [None; BARS];
        for (i, slot) in out.iter_mut().enumerate() {
            let end = now - (BARS - 1 - i) as u64 * HOUR;
            *slot = self.samples.iter().rev().find(|s| s.0 <= end).map(|s| s.1);
        }
        out
    }

    /// Drain in tenths of a percent per hour over the current discharge (at most the last 6 h):
    /// needs at least an hour of readings and a 1 % drop.
    pub fn drain_tenths(&self, now: u64) -> Option<u32> {
        let recent: Vec<&(u64, u8)> = self.samples.iter().filter(|s| s.0 + 6 * HOUR >= now).collect();
        let mut start = recent.len().checked_sub(1)?;
        // walk back while the level does not rise (a rise means it was charging)
        while start > 0 && recent[start - 1].1 >= recent[start].1 {
            start -= 1;
        }
        let (first, last) = (recent[start], recent[recent.len() - 1]);
        let span = last.0.saturating_sub(first.0);
        let drop = first.1.saturating_sub(last.1) as u64;
        if span < HOUR || drop == 0 {
            return None;
        }
        Some((drop * 10 * HOUR / span) as u32)
    }
}

fn append(path: &Path, t: u64, pct: u8, all: &[(u64, u8)]) -> std::io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() > FILE_MAX) {
        let text: String = all.iter().map(|(t, p)| format!("{t},{p}\n")).collect();
        return lghub_lite::config::write_atomic(path, &text).map_err(std::io::Error::other);
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{t},{pct}")
}

/// "≈ 41 h left" style text from tenths of an hour.
pub fn hours_text(tenths: u32) -> String {
    if tenths >= 100 {
        format!("≈ {} h left", (tenths + 5) / 10)
    } else {
        format!("≈ {}.{} h left", tenths / 10, tenths % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_and_drain() {
        let now = 100 * HOUR;
        // 88 % six hours ago, falling 2 % per hour
        let h = History::memory((0..=6).map(|i| (now - (6 - i) * HOUR, 88 - 2 * i as u8)).collect());
        let b = h.bars(now);
        assert_eq!(b[BARS - 1], Some(76));
        assert_eq!(b[BARS - 7], Some(88));
        assert_eq!(b[0], None);
        assert_eq!(h.drain_tenths(now), Some(20));
        // a charge in between: only the part after it counts
        let h = History::memory(vec![
            (now - 5 * HOUR, 50),
            (now - 3 * HOUR, 90),
            (now - HOUR, 86),
            (now, 84),
        ]);
        assert_eq!(h.drain_tenths(now), Some(20));
        // too short or no drop
        assert_eq!(
            History::memory(vec![(now - 600, 50), (now, 49)]).drain_tenths(now),
            None
        );
        assert_eq!(
            History::memory(vec![(now - 2 * HOUR, 50), (now, 50)]).drain_tenths(now),
            None
        );
        assert_eq!(History::memory(vec![]).drain_tenths(now), None);
    }

    #[test]
    fn add_skips_repeats_and_texts() {
        let mut h = History::memory(vec![]);
        h.add(1000, 50);
        h.add(1100, 50);
        h.add(1200, 49);
        h.add(1000 + REPEAT + 200, 49);
        assert_eq!(h.samples.len(), 3);
        assert_eq!(hours_text(410), "≈ 41 h left");
        assert_eq!(hours_text(91), "≈ 9.1 h left");
    }
}
