//! HID multitouch report decoders, one per controller family. The report ID
//! says which panel is talking. Layouts are taken from the panels' live HID
//! report descriptors. Parsing never allocates: a frame carries at most ten
//! contacts, so they live inline in the frame's [`smallvec::SmallVec`].

use smallvec::{SmallVec, smallvec};

/// WCH (wch.cn, USB 0x27C0:0x0859, the JAPANNEXT JN-MD-IPST105FP) and ILITEK
/// (USB 0x222A:0x0001, the upstream ARZOPA reference panel) USB ids.
pub mod usb {
    pub const WCH_VID: u16 = 0x27C0;
    pub const WCH_PID: u16 = 0x0859;
    pub const ILITEK_VID: u16 = 0x222A;
    pub const ILITEK_PID: u16 = 0x0001;
}

/// One finger in a frame. Coordinates are normalized to 0..=1 across the panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub id: u8,
    pub touching: bool,
    pub x: f64,
    pub y: f64,
}

/// All contacts reported by one HID input report.
#[derive(Debug, Clone, PartialEq)]
pub struct TouchFrame {
    pub source: Source,
    pub contacts: SmallVec<[Contact; 10]>,
    /// Device scan time (WCH and ILITEK digitizer reports only); units are
    /// confirmed by the M1 probe.
    pub scan_time: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Digitizer,
    Mouse,
}

/// Decodes the multitouch digitizer input reports of the two supported
/// controller families. One decoder per wire format; the report ID says which
/// panel is talking.
pub enum ReportParser {
    // WCH and ILITEK both key their wire formats off the report ID, so the
    // enum stays uninhabited and the decoders live in the associated items.
}

// The layout constants mirror the Swift statics' names (snake_case), which
// other modules in the port reference verbatim.
#[allow(non_upper_case_globals)]
impl ReportParser {
    /// WCH (wch.cn, USB 0x27C0:0x0859, the JAPANNEXT JN-MD-IPST105FP), from its
    /// HID report descriptor: RID 0x0D, ten 5-byte slots (tip switch at bit 0,
    /// contact id at bits 4-7, X and Y as little-endian 16-bit up to 16383 /
    /// 9599), then scan time LE16 and contact count; 54 bytes in total.
    pub const wch_report_id: u8 = 0x0D;
    pub const wch_report_length: usize = 54;
    pub const wch_scan_time_offset: usize = 51;
    pub const wch_contact_count_offset: usize = 53;
    pub const wch_slot_size: usize = 5;
    pub const wch_max_slots: usize = 10;
    pub const wch_logical_max_x: f64 = 16383.0;
    pub const wch_logical_max_y: f64 = 9599.0;

    /// ILITEK (USB 0x222A:0x0001, the upstream ARZOPA reference panel): RID 4,
    /// N slots of 5 bytes (contact id at bits 0-5, tip switch at 0x40, X and Y
    /// LE16 up to 16384 / 9600), then scan time LE32 and contact count. N is
    /// the panel's contact limit and varies: the 10-contact ARZOPA pads its
    /// report to 64 bytes; shorter ILITEK-family panels send shorter reports,
    /// so the slot count is derived from each report's own length and capped
    /// at 10.
    pub const ilitek_report_id: u8 = 0x04;
    pub const ilitek_report_min_length: usize = 11;
    pub const ilitek_slot_size: usize = 5;
    pub const ilitek_max_slots: usize = 10;
    pub const ilitek_logical_max_x: f64 = 16384.0;
    pub const ilitek_logical_max_y: f64 = 9600.0;

    /// WCH mouse interface (single touch on macOS, the digitizer interface
    /// being firmware-gated): RID 7, buttons @1, X LE16 @2 (logical 0-16383),
    /// Y LE16 @4 (logical 0-9599), wheel @6. Extents confirmed from the
    /// interface's own report descriptor.
    pub const wch_mouse_report_id: u8 = 0x07;
    pub const wch_mouse_report_length: usize = 7;

    /// ILITEK mouse fallback: RID 5, buttons @1, X LE16 @2, Y LE16 @4.
    pub const mouse_report_id: u8 = 0x05;
    pub const mouse_report_length: usize = 6;

    /// Decodes one raw input report (report ID at index 0). Unknown report IDs
    /// and reports shorter than their format needs decode to nothing.
    pub fn parse(bytes: &[u8]) -> Option<TouchFrame> {
        let report_id = *bytes.first()?;
        match report_id {
            Self::wch_report_id if bytes.len() >= Self::wch_report_length => {
                Some(Self::parse_wch_digitizer(bytes))
            }
            Self::wch_mouse_report_id if bytes.len() >= Self::wch_mouse_report_length => {
                Some(Self::parse_wch_mouse(bytes))
            }
            Self::ilitek_report_id if bytes.len() >= Self::ilitek_report_min_length => {
                Some(Self::parse_ilitek(bytes))
            }
            Self::mouse_report_id if bytes.len() >= Self::mouse_report_length => {
                Some(Self::parse_mouse(bytes))
            }
            _ => None,
        }
    }

    fn parse_wch_mouse(r: &[u8]) -> TouchFrame {
        TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact {
                id: 0,
                touching: r[1] & 0x01 != 0,
                x: normalize(le16(r, 2), Self::wch_logical_max_x),
                y: normalize(le16(r, 4), Self::wch_logical_max_y),
            }],
            scan_time: None,
        }
    }

    fn parse_wch_digitizer(r: &[u8]) -> TouchFrame {
        let count = (r[Self::wch_contact_count_offset] as usize).min(Self::wch_max_slots);
        let mut contacts = SmallVec::with_capacity(count);
        for slot in 0..count {
            let base = 1 + slot * Self::wch_slot_size;
            contacts.push(Contact {
                id: r[base] >> 4,
                touching: r[base] & 0x01 != 0,
                x: normalize(le16(r, base + 1), Self::wch_logical_max_x),
                y: normalize(le16(r, base + 3), Self::wch_logical_max_y),
            });
        }
        TouchFrame {
            source: Source::Digitizer,
            contacts,
            scan_time: Some(u32::from(le16(r, Self::wch_scan_time_offset))),
        }
    }

    fn parse_ilitek(r: &[u8]) -> TouchFrame {
        let slot_count = ((r.len() - 6) / Self::ilitek_slot_size).min(Self::ilitek_max_slots);
        let scan_time_offset = 1 + slot_count * Self::ilitek_slot_size;
        let contact_count_offset = scan_time_offset + 4;
        let count = (r[contact_count_offset] as usize).min(slot_count);
        let mut contacts = SmallVec::with_capacity(count);
        for slot in 0..count {
            let base = 1 + slot * Self::ilitek_slot_size;
            contacts.push(Contact {
                id: r[base] & 0x3F,
                touching: r[base] & 0x40 != 0,
                x: normalize(le16(r, base + 1), Self::ilitek_logical_max_x),
                y: normalize(le16(r, base + 3), Self::ilitek_logical_max_y),
            });
        }
        let mut scan_time = 0u32;
        for b in 0..4 {
            scan_time |= u32::from(r[scan_time_offset + b]) << (8 * b);
        }
        TouchFrame { source: Source::Digitizer, contacts, scan_time: Some(scan_time) }
    }

    fn parse_mouse(r: &[u8]) -> TouchFrame {
        TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact {
                id: 0,
                touching: r[1] & 0x01 != 0,
                x: normalize(le16(r, 2), Self::ilitek_logical_max_x),
                y: normalize(le16(r, 4), Self::ilitek_logical_max_y),
            }],
            scan_time: None,
        }
    }
}

#[inline]
fn le16(r: &[u8], offset: usize) -> u16 {
    u16::from(r[offset]) | (u16::from(r[offset + 1]) << 8)
}

#[inline]
fn normalize(value: u16, max: f64) -> f64 {
    (f64::from(value) / max).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a 64-byte ILITEK RID 4 report: 10 slots of (id|tip<<6, X LE16,
    /// Y LE16), scan time LE32 @51, contact count @55.
    fn digitizer_report(slots: &[(u8, bool, u16, u16)], count: Option<u8>, scan_time: u32) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x04;
        for (i, &(id, tip, x, y)) in slots.iter().enumerate() {
            let base = 1 + i * 5;
            r[base] = (id & 0x3F) | if tip { 0x40 } else { 0 };
            r[base + 1] = (x & 0xFF) as u8;
            r[base + 2] = (x >> 8) as u8;
            r[base + 3] = (y & 0xFF) as u8;
            r[base + 4] = (y >> 8) as u8;
        }
        for b in 0..4 {
            r[51 + b] = ((scan_time >> (8 * b)) & 0xFF) as u8;
        }
        r[55] = count.unwrap_or(slots.len() as u8);
        r
    }

    /// Builds a 54-byte WCH RID 0x0D report: 10 slots of (tip|id<<4, X LE16,
    /// Y LE16), scan time LE16 @51, contact count @53.
    fn wch_report(slots: &[(u8, bool, u16, u16)], count: Option<u8>, scan_time: u16) -> Vec<u8> {
        let mut r = vec![0u8; 54];
        r[0] = 0x0D;
        for (i, &(id, tip, x, y)) in slots.iter().enumerate() {
            let base = 1 + i * 5;
            r[base] = (id << 4) | if tip { 0x01 } else { 0 };
            r[base + 1] = (x & 0xFF) as u8;
            r[base + 2] = (x >> 8) as u8;
            r[base + 3] = (y & 0xFF) as u8;
            r[base + 4] = (y >> 8) as u8;
        }
        r[51] = (scan_time & 0xFF) as u8;
        r[52] = (scan_time >> 8) as u8;
        r[53] = count.unwrap_or(slots.len() as u8);
        r
    }

    #[test]
    fn parses_single_contact_as_normalized_point() {
        let frame = ReportParser::parse(&digitizer_report(&[(3, true, 8192, 4800)], None, 1234)).unwrap();
        assert_eq!(frame.source, Source::Digitizer);
        assert_eq!(frame.scan_time, Some(1234));
        let expected: SmallVec<[Contact; 10]> = smallvec![Contact { id: 3, touching: true, x: 0.5, y: 0.5 }];
        assert_eq!(frame.contacts, expected);
    }

    #[test]
    fn parses_only_the_reported_number_of_contacts() {
        let report = digitizer_report(
            &[(0, true, 0, 0), (1, true, 16384, 9600), (2, true, 100, 100)],
            Some(2),
            0,
        );
        let frame = ReportParser::parse(&report).unwrap();
        let expected: SmallVec<[Contact; 10]> = smallvec![
            Contact { id: 0, touching: true, x: 0.0, y: 0.0 },
            Contact { id: 1, touching: true, x: 1.0, y: 1.0 },
        ];
        assert_eq!(frame.contacts, expected);
    }

    #[test]
    fn keeps_lifted_contacts_so_the_tracker_sees_the_lift() {
        let frame = ReportParser::parse(&digitizer_report(&[(5, false, 8192, 4800)], None, 0)).unwrap();
        assert_eq!(frame.contacts.first().map(|c| c.touching), Some(false));
    }

    #[test]
    fn clamps_out_of_range_coordinates() {
        let frame = ReportParser::parse(&digitizer_report(&[(0, true, 20000, 12000)], None, 0)).unwrap();
        assert_eq!(frame.contacts.first().map(|c| c.x), Some(1.0));
        assert_eq!(frame.contacts.first().map(|c| c.y), Some(1.0));
    }

    #[test]
    fn caps_contact_count_at_the_ten_slots() {
        let frame = ReportParser::parse(&digitizer_report(&[(0, true, 1, 1)], Some(99), 0)).unwrap();
        assert_eq!(frame.contacts.len(), 10);
    }

    #[test]
    fn parses_mouse_fallback_report_as_one_contact() {
        // RID 5: buttons @1, X LE16 @2, Y LE16 @4.
        let report = [0x05, 0x01, 0x00, 0x20, 0xC0, 0x12];
        let frame = ReportParser::parse(&report).unwrap();
        assert_eq!(frame.source, Source::Mouse);
        let expected: SmallVec<[Contact; 10]> = smallvec![Contact { id: 0, touching: true, x: 0.5, y: 0.5 }];
        assert_eq!(frame.contacts, expected);
    }

    #[test]
    fn rejects_unknown_report_ids_and_short_reports() {
        let mut unknown = vec![0x03u8];
        unknown.extend(std::iter::repeat_n(0u8, 63));
        assert_eq!(ReportParser::parse(&unknown), None);
        assert_eq!(ReportParser::parse(&[0x04, 0x00]), None);
        assert_eq!(ReportParser::parse(&[]), None);
    }

    #[test]
    fn parses_wch_digitizer_report() {
        let frame = ReportParser::parse(&wch_report(&[(2, true, 16383, 9599)], None, 777)).unwrap();
        assert_eq!(frame.source, Source::Digitizer);
        assert_eq!(frame.scan_time, Some(777));
        let expected: SmallVec<[Contact; 10]> = smallvec![Contact { id: 2, touching: true, x: 1.0, y: 1.0 }];
        assert_eq!(frame.contacts, expected);
    }

    #[test]
    fn parses_wch_contact_id_and_tip_nibble_separately() {
        let frame = ReportParser::parse(&wch_report(&[(7, false, 0, 0)], None, 0)).unwrap();
        let expected: SmallVec<[Contact; 10]> = smallvec![Contact { id: 7, touching: false, x: 0.0, y: 0.0 }];
        assert_eq!(frame.contacts, expected);
    }

    #[test]
    fn parses_only_the_wch_reported_number_of_contacts() {
        let report = wch_report(
            &[(0, true, 0, 0), (1, true, 8192, 4800), (2, true, 16383, 9599)],
            Some(2),
            0,
        );
        let frame = ReportParser::parse(&report).unwrap();
        let ids: Vec<u8> = frame.contacts.iter().map(|c| c.id).collect();
        let xs: Vec<f64> = frame.contacts.iter().map(|c| c.x).collect();
        assert_eq!(ids, vec![0, 1]);
        assert_eq!(xs, vec![0.0, 8192.0 / 16383.0]);
    }

    #[test]
    fn caps_wch_contact_count_at_the_ten_slots() {
        let frame = ReportParser::parse(&wch_report(&[(0, true, 1, 1)], Some(99), 0)).unwrap();
        assert_eq!(frame.contacts.len(), 10);
    }

    #[test]
    fn rejects_short_wch_reports() {
        let mut short = vec![0x0Du8];
        short.extend(std::iter::repeat_n(0u8, 52));
        assert_eq!(ReportParser::parse(&short), None);
    }

    #[test]
    fn parses_wch_mouse_report_as_one_contact() {
        // RID 7 as captured from the panel: buttons @1, X LE16 @2, Y LE16 @4, wheel @6.
        let report = [0x07, 0x01, 0x44, 0x26, 0x01, 0x15, 0x00];
        let frame = ReportParser::parse(&report).unwrap();
        assert_eq!(frame.source, Source::Mouse);
        let expected: SmallVec<[Contact; 10]> = smallvec![Contact {
            id: 0,
            touching: true,
            x: 9796.0 / 16383.0,
            y: 5377.0 / 9599.0,
        }];
        assert_eq!(frame.contacts, expected);
    }

    #[test]
    fn parses_wch_mouse_lift() {
        let frame = ReportParser::parse(&[0x07, 0x00, 0x44, 0x26, 0x01, 0x15, 0x00]).unwrap();
        let expected: SmallVec<[Contact; 10]> = smallvec![Contact {
            id: 0,
            touching: false,
            x: 9796.0 / 16383.0,
            y: 5377.0 / 9599.0,
        }];
        assert_eq!(frame.contacts, expected);
    }
}
