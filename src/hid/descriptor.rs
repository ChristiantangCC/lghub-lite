//! A small HID report-descriptor parser: the top-level collections with their usage and the
//! longest input/output report in each, report ID byte included. That is what Windows'
//! HidP_GetCaps reports per collection; on Linux hidraw only offers the raw descriptor, so
//! this turns it into the same `Info` shape. Follows the HID 1.11 spec, §6.2.2 (items).

/// One top-level collection.
#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    pub usage_page: u16,
    pub usage: u16,
    /// Longest input / output report in bytes, including the report ID byte when IDs are used.
    pub in_len: u16,
    pub out_len: u16,
    /// Report IDs used in this collection (0 = none).
    pub report_ids: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

/// Bits per (report ID, input/output) inside the current top-level collection.
#[derive(Default)]
struct Sizes {
    input: Vec<(u8, u32)>,
    output: Vec<(u8, u32)>,
}

fn add(list: &mut Vec<(u8, u32)>, id: u8, bits: u32) {
    match list.iter_mut().find(|e| e.0 == id) {
        Some(e) => e.1 += bits,
        None => list.push((id, bits)),
    }
}

fn longest(list: &[(u8, u32)]) -> u16 {
    list.iter()
        .map(|&(id, bits)| bits.div_ceil(8) + (id != 0) as u32)
        .max()
        .unwrap_or(0)
        .min(u16::MAX as u32) as u16
}

/// Parse `desc`. Malformed trailing bytes end the parse; what was complete is returned.
pub fn parse(desc: &[u8]) -> Vec<Collection> {
    let mut out = Vec::new();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new(); // local usages since the last main item
    let mut depth = 0u32;
    let mut top: Option<(u16, u16, Sizes, Vec<u8>)> = None;
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // long item: [0xFE, size, tag, data…]
            let size = *desc.get(i + 1).unwrap_or(&0) as usize;
            i += 3 + size;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let (kind, tag) = ((prefix >> 2) & 3, prefix >> 4);
        let Some(bytes) = desc.get(i + 1..i + 1 + size) else {
            break;
        };
        let data = bytes.iter().rev().fold(0u32, |v, b| (v << 8) | *b as u32);
        i += 1 + size;
        match (kind, tag) {
            // main items
            (0, 0x8) | (0, 0x9) => {
                if let Some((_, _, sizes, ids)) = top.as_mut() {
                    let bits = g.report_size * g.report_count;
                    let list = if tag == 0x8 {
                        &mut sizes.input
                    } else {
                        &mut sizes.output
                    };
                    add(list, g.report_id, bits);
                    if !ids.contains(&g.report_id) {
                        ids.push(g.report_id);
                    }
                }
                usages.clear();
            }
            (0, 0xA) => {
                if depth == 0 {
                    let u = usages.first().copied().unwrap_or(0);
                    let page = if u > 0xFFFF { u >> 16 } else { g.usage_page };
                    top = Some((page as u16, u as u16, Sizes::default(), Vec::new()));
                }
                depth += 1;
                usages.clear();
            }
            (0, 0xC) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some((usage_page, usage, sizes, report_ids)) = top.take()
                {
                    out.push(Collection {
                        usage_page,
                        usage,
                        in_len: longest(&sizes.input),
                        out_len: longest(&sizes.output),
                        report_ids,
                    });
                }
                usages.clear();
            }
            (0, _) => usages.clear(), // feature and reserved main items
            // global items
            (1, 0x0) => g.usage_page = data,
            (1, 0x7) => g.report_size = data,
            (1, 0x8) => g.report_id = data as u8,
            (1, 0x9) => g.report_count = data,
            (1, 0xA) => stack.push(g),
            (1, 0xB) => g = stack.pop().unwrap_or(g),
            // local items: Usage (a 4-byte one carries its own page)
            (2, 0x0) => usages.push(if size == 4 { data } else { data & 0xFFFF }),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A receiver's HID++ interface as the HID spec describes one (synthetic bytes): the short
    /// (ID 0x10, 6 data bytes) and long (ID 0x11, 19 data bytes) collections, plus a DJ-style
    /// collection with two input reports.
    const RECEIVER: &[u8] = &[
        0x06, 0x00, 0xFF, // Usage Page (0xFF00)
        0x09, 0x01, // Usage (1)
        0xA1, 0x01, // Collection (Application)
        0x85, 0x10, //   Report ID (0x10)
        0x75, 0x08, //   Report Size (8)
        0x95, 0x06, //   Report Count (6)
        0x15, 0x00, 0x26, 0xFF, 0x00, // Logical 0..255
        0x09, 0x01, 0x81, 0x00, // Usage, Input
        0x09, 0x01, 0x91, 0x00, // Usage, Output
        0xC0, // End Collection
        0x06, 0x00, 0xFF, 0x09, 0x02, 0xA1, 0x01, // page FF00, usage 2, collection
        0x85, 0x11, 0x75, 0x08, 0x95, 0x13, // ID 0x11, 8 bits × 19
        0x09, 0x02, 0x81, 0x00, 0x09, 0x02, 0x91, 0x00, 0xC0, //
        0x06, 0x00, 0xFF, 0x09, 0x04, 0xA1, 0x01, // page FF00, usage 4
        0x85, 0x20, 0x95, 0x0E, 0x09, 0x41, 0x81, 0x00, // ID 0x20, 14 bytes in
        0x85, 0x21, 0x95, 0x1F, 0x09, 0x42, 0x81, 0x00, // ID 0x21, 31 bytes in
        0xC0,
    ];

    #[test]
    fn receiver_collections_match_windows_caps() {
        let c = parse(RECEIVER);
        let short = (c[0].usage_page, c[0].usage, c[0].in_len, c[0].out_len);
        let long = (c[1].usage_page, c[1].usage, c[1].in_len, c[1].out_len);
        assert_eq!(short, (0xFF00, 1, 7, 7)); // [HW] Windows: page=ff00 usage=1 in=7 out=7
        assert_eq!(long, (0xFF00, 2, 20, 20));
        assert_eq!(
            (c[2].usage, c[2].in_len, c[2].out_len, c[2].report_ids.clone()),
            (4, 32, 0, vec![0x20, 0x21])
        );
    }

    #[test]
    fn nested_collections_globals_and_extended_usages() {
        let desc = [
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, // Generic Desktop / Mouse, application
            0x09, 0x01, 0xA1, 0x00, // Pointer, physical (nested)
            0xA4, // Push
            0x75, 0x01, 0x95, 0x05, 0x81, 0x02, // 5 buttons × 1 bit
            0xB4, // Pop
            0x75, 0x03, 0x95, 0x01, 0x81, 0x01, // 3 bits padding
            0x75, 0x10, 0x95, 0x02, 0x81, 0x06, // X, Y 16 bit
            0xC0, 0xC0, //
            // a headset-style collection with a 4-byte extended usage (page FF43, usage 0x0202)
            0x0B, 0x02, 0x02, 0x43, 0xFF, 0xA1, 0x01, 0x85, 0x11, 0x75, 0x08, 0x95, 0x13, //
            0x81, 0x00, 0x91, 0x00, 0xC0,
        ];
        let c = parse(&desc);
        assert_eq!(c.len(), 2, "the nested physical collection is not top-level");
        assert_eq!((c[0].usage_page, c[0].usage, c[0].in_len, c[0].out_len), (1, 2, 5, 0));
        assert_eq!(
            (c[1].usage_page, c[1].usage, c[1].in_len, c[1].out_len),
            (0xFF43, 0x0202, 20, 20)
        );
    }

    #[test]
    fn truncated_input_keeps_complete_collections() {
        let mut d = RECEIVER.to_vec();
        d.truncate(RECEIVER.len() - 3);
        assert_eq!(parse(&d).len(), 2);
        assert!(parse(&[0x06, 0x00]).is_empty());
    }
}
