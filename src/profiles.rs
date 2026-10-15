//! Onboard profile memory (HID++ 0x8100), read-only: info, directory, sector dump, CRC checks.
//! Nothing here writes to the device. Evidence: NOTES.md "Onboard profiles".

use crate::hidpp::{Error, Hidpp};

/// First ROM (factory) sector; ROM profile n lives at 0x0100 + n and the ROM directory at
/// 0x0100. [PUB libratbag] [HW SL2: active profile 0x0101, ROM directory lists 0x0101]
pub const ROM_BASE: u16 = 0x0100;

/// Sector 0 holds the user profile directory. [PUB libratbag]
pub const USER_DIRECTORY: u16 = 0x0000;

/// f5 ReadData returns 16 bytes per call. [OBS dec_all.c FUN_140d0ef20: reply >= 16 bytes]
const CHUNK: usize = 16;

#[derive(Debug, PartialEq)]
pub struct Info {
    pub mem_model: u8,
    pub profile_format: u8,
    pub macro_format: u8,
    pub profile_count: u8,
    pub factory_count: u8,
    pub button_count: u8,
    pub sector_count: u8,
    pub sector_size: u16,
    /// `&3 == 2`: G-shift; `&0xC == 8`: DPI-shift. [OBS FUN_140d0e7b0]
    pub layout: u8,
}

/// f0 GetOnboardProfilesInfo reply: 7 single bytes, sector size BE16, layout byte.
/// [OBS dec_all.c FUN_140d0e7b0] [HW SL2: `01 07 01 05 01 05 10 00 FF 0A 04`]
pub fn parse_info(p: &[u8]) -> Result<Info, Error> {
    if p.len() < 10 {
        return Err(Error::BadReply("onboard profiles info shorter than 10 bytes"));
    }
    Ok(Info {
        mem_model: p[0],
        profile_format: p[1],
        macro_format: p[2],
        profile_count: p[3],
        factory_count: p[4],
        button_count: p[5],
        sector_count: p[6],
        sector_size: u16::from_be_bytes([p[7], p[8]]),
        layout: p[9],
    })
}

/// Returns (feature index, raw reply, parsed info).
pub fn info(h: &mut Hidpp, dev: u8) -> Result<(u8, Vec<u8>, Info), Error> {
    let fi = h.feature(dev, 0x8100)?;
    let p = h.call(dev, fi, 0, &[], &[])?;
    let i = parse_info(&p)?;
    Ok((fi, p, i))
}

/// f4 GetActiveProfile -> BE16 sector of the active profile. [OBS FUN_140d0eda0] [HW SL2: 0x0101]
pub fn active_profile(h: &mut Hidpp, dev: u8, fi: u8) -> Result<u16, Error> {
    let p = h.call(dev, fi, 4, &[], &[])?;
    if p.len() < 2 {
        return Err(Error::BadReply("active profile reply too short"));
    }
    Ok(u16::from_be_bytes([p[0], p[1]]))
}

/// Offsets of the 16-byte reads covering a sector of `size` bytes. The last read is moved back
/// to end exactly at `size` so nothing past the sector is requested. [INF; PUB libratbag does
/// the same]
pub fn read_offsets(size: usize) -> Vec<usize> {
    if size <= CHUNK {
        return vec![0];
    }
    let mut v: Vec<usize> = (0..size).step_by(CHUNK).filter(|o| o + CHUNK <= size).collect();
    if !size.is_multiple_of(CHUNK) {
        v.push(size - CHUNK);
    }
    v
}

/// Read a whole sector with f5 ReadData [sector BE16, offset BE16] -> 16 bytes.
/// [OBS FUN_140d0ef20] [HW SL2]
pub fn read_sector(h: &mut Hidpp, dev: u8, fi: u8, sector: u16, size: u16) -> Result<Vec<u8>, Error> {
    let size = size as usize;
    let mut out = vec![0u8; size];
    for off in read_offsets(size) {
        let [s0, s1] = sector.to_be_bytes();
        let [o0, o1] = (off as u16).to_be_bytes();
        let p = h.call(dev, fi, 5, &[s0, s1, o0, o1], &[])?;
        if p.len() < CHUNK {
            return Err(Error::BadReply("onboard memory read shorter than 16 bytes"));
        }
        let n = CHUNK.min(size - off);
        out[off..off + n].copy_from_slice(&p[..n]);
    }
    Ok(out)
}

/// f10 GetCRC [sector BE16] -> the device's CRC of that flash sector (first BE16 of the reply).
/// [OBS FUN_140d0fa30 sends the sector; reply layout INF]
/// [HW SL2: user sector 0 (erased) -> `B9 2A …` = crc_ccitt(253 × FF); ROM sectors -> InvalidArgument]
pub fn device_crc(h: &mut Hidpp, dev: u8, fi: u8, sector: u16) -> Result<u16, Error> {
    let p = h.call(dev, fi, 10, &sector.to_be_bytes(), &[])?;
    if p.len() < 2 {
        return Err(Error::BadReply("CRC reply too short"));
    }
    Ok(u16::from_be_bytes([p[0], p[1]]))
}

/// CRC-CCITT, init 0xFFFF, polynomial 0x1021, no reflection. [PUB libratbag]
/// [HW: matches the device's GetCRC for an erased 255-byte sector, 0xB92A over 253 bytes]
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut c: u16 = 0xFFFF;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

#[derive(Debug, PartialEq)]
pub enum SectorState {
    Erased,
    /// Factory (ROM) sector: carries no CRC.
    Rom,
    CrcOk,
    /// (computed, stored)
    CrcBad(u16, u16),
}

/// Flash (user) sectors end with a BE16 CRC over the bytes before it [PUB libratbag; algorithm
/// HW-confirmed via GetCRC]. ROM sectors carry none [HW SL2: no prefix of 0x0100/0x0101 has
/// its CRC stored after it, 0x0101 ends in FF FF, and GetCRC refuses ROM sectors].
pub fn sector_state(sector: u16, data: &[u8]) -> SectorState {
    if data.iter().all(|&b| b == 0xFF) {
        return SectorState::Erased;
    }
    if sector >= ROM_BASE {
        return SectorState::Rom;
    }
    let n = data.len();
    if n < 2 {
        return SectorState::CrcBad(0, 0);
    }
    let computed = crc_ccitt(&data[..n - 2]);
    let stored = u16::from_be_bytes([data[n - 2], data[n - 1]]);
    if computed == stored {
        SectorState::CrcOk
    } else {
        SectorState::CrcBad(computed, stored)
    }
}

/// Directory entries `[sector BE16, enabled, reserved]` until sector 0xFFFF. [PUB libratbag]
/// [HW SL2 ROM directory: `01 01 01 FF FF FF …` -> one entry, 0x0101 enabled]
pub fn parse_directory(data: &[u8]) -> Vec<(u16, bool)> {
    let mut out = Vec::new();
    for e in data.as_chunks::<4>().0 {
        let sector = u16::from_be_bytes([e[0], e[1]]);
        if sector == 0xFFFF {
            break;
        }
        out.push((sector, e[2] != 0));
    }
    out
}

/// Text backup: header lines, then `sector XXXX <hex bytes>` per sector. Text so it can be
/// inspected and diffed; restoring it is not implemented.
pub fn format_dump(model: u16, info_raw: &[u8], sectors: &[(u16, Vec<u8>)]) -> String {
    let mut s = String::from("# lghub-lite onboard memory dump (HID++ 0x8100), read-only backup\n");
    s.push_str(&format!("model {model:04x}\n"));
    s.push_str(&format!("info {}\n", crate::hidpp::hex(info_raw)));
    for (sec, data) in sectors {
        s.push_str(&format!("sector {sec:04x} {}\n", crate::hidpp::hex(data)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_from_hw() {
        let p = [
            0x01, 0x07, 0x01, 0x05, 0x01, 0x05, 0x10, 0x00, 0xFF, 0x0A, 0x04, 0, 0, 0, 0, 0,
        ];
        let i = parse_info(&p).unwrap();
        assert_eq!(
            (i.profile_count, i.factory_count, i.sector_count, i.sector_size),
            (5, 1, 16, 255)
        );
        assert_eq!((i.layout & 3, i.layout & 0xC), (2, 8));
        assert!(parse_info(&p[..9]).is_err());
    }

    #[test]
    fn crc_matches_device_for_erased_sector() {
        // [HW] GetCRC of an erased 255-byte sector returned B9 2A
        assert_eq!(crc_ccitt(&[0xFF; 253]), 0xB92A);
        assert_eq!(crc_ccitt(b"123456789"), 0x29B1); // CRC-16/CCITT-FALSE check value
    }

    #[test]
    fn sector_states() {
        assert_eq!(sector_state(0, &[0xFF; 255]), SectorState::Erased);
        let mut s = vec![0x11u8; 255];
        let c = crc_ccitt(&s[..253]).to_be_bytes();
        s[253] = c[0];
        s[254] = c[1];
        assert_eq!(sector_state(3, &s), SectorState::CrcOk);
        assert_eq!(sector_state(0x0101, &s), SectorState::Rom);
        s[0] = 0x12;
        assert!(matches!(sector_state(3, &s), SectorState::CrcBad(_, _)));
    }

    #[test]
    fn offsets_cover_sector_without_overrun() {
        let o = read_offsets(255);
        assert_eq!(o.len(), 16);
        assert_eq!(*o.last().unwrap(), 239); // [HW] read at 0x00EF returned the sector tail
        assert!(o.iter().all(|x| x + 16 <= 255));
        assert_eq!(read_offsets(32), [0, 16]);
        assert_eq!(read_offsets(16), [0]);
    }

    #[test]
    fn directory_from_hw() {
        // [HW] ROM directory sector 0x0100, first bytes
        let d = [0x01, 0x01, 0x01, 0xFF, 0xFF, 0xFF, 0x03, 0x04, 0x05, 0x06, 0x00, 0x01];
        assert_eq!(parse_directory(&d), [(0x0101, true)]);
        assert_eq!(parse_directory(&[0xFF; 16]), []);
    }

    #[test]
    fn dump_format() {
        let d = format_dump(0x40A9, &[1, 7], &[(0x0101, vec![0xAB, 0x01])]);
        assert!(d.contains("model 40a9\n") && d.contains("info 01 07\n") && d.contains("sector 0101 AB 01\n"));
    }
}
