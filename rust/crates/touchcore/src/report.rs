//! HID multitouch report decoders, one per controller family. The report ID
//! says which panel is talking. Layouts taken from the panels' live HID report
//! descriptors. Decoding is allocation-free: a frame carries at most ten
//! contacts, so they live inline in [`Contacts`].

/// Known touch controller USB ids.
pub mod usb {
    pub const WCH_VID: u16 = 0x27C0;
    pub const WCH_PID: u16 = 0x0859;
    pub const ILITEK_VID: u16 = 0x222A;
    pub const ILITEK_PID: u16 = 0x0001;
}

/// The most contacts any supported panel reports (HID multitouch digitizers
/// expose at most ten slots).
pub const MAX_CONTACTS: usize = 10;

/// One finger in a frame. Coordinates are normalized to 0..=1 across the panel.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Contact {
    pub id: u8,
    pub touching: bool,
    pub x: f64,
    pub y: f64,
}

impl Contact {
    pub const fn new(id: u8, touching: bool, x: f64, y: f64) -> Self {
        Self { id, touching, x, y }
    }
}

/// Inline, fixed-capacity contact storage: parsing a report never touches the
/// heap.
#[derive(Debug, Clone, Copy, Default)]
pub struct Contacts {
    slots: [Contact; MAX_CONTACTS],
    len: usize,
}

impl Contacts {
    pub const fn new() -> Self {
        Self { slots: [Contact::new(0, false, 0.0, 0.0); MAX_CONTACTS], len: 0 }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, contact: Contact) {
        if self.len < MAX_CONTACTS {
            self.slots[self.len] = contact;
            self.len += 1;
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    pub fn first(&self) -> Option<Contact> {
        (self.len > 0).then_some(self.slots[0])
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Contact> {
        self.slots[..self.len].iter()
    }
}

impl std::ops::Index<usize> for Contacts {
    type Output = Contact;

    fn index(&self, index: usize) -> &Contact {
        &self.slots[index]
    }
}

impl<'a> IntoIterator for &'a Contacts {
    type Item = &'a Contact;
    type IntoIter = std::slice::Iter<'a, Contact>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl PartialEq for Contacts {
    fn eq(&self, other: &Self) -> bool {
        self.slots[..self.len] == other.slots[..other.len]
    }
}

impl FromIterator<Contact> for Contacts {
    fn from_iter<I: IntoIterator<Item = Contact>>(iter: I) -> Self {
        let mut contacts = Self::new();
        for contact in iter {
            contacts.push(contact);
        }
        contacts
    }
}

/// All contacts reported by one HID input report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TouchFrame {
    pub source: Source,
    pub contacts: Contacts,
    /// Device scan time, when the format carries one.
    pub scan_time: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Digitizer,
    Mouse,
}

// WCH (wch.cn, USB 0x27C0:0x0859 — the JAPANNEXT JN-MD-IPST105FP and other
// WingCoolTouch panels): RID 0x0D, ten 5-byte slots (tip switch at bit 0,
// contact id at bits 4-7, X and Y as little-endian 16-bit up to 16383 / 9599),
// then scan time LE16 and contact count; 54 bytes in total.
const WCH_REPORT_ID: u8 = 0x0D;
const WCH_REPORT_LENGTH: usize = 54;
const WCH_SLOT_SIZE: usize = 5;
const WCH_SCAN_TIME_OFFSET: usize = 51;
const WCH_CONTACT_COUNT_OFFSET: usize = 53;
const WCH_MAX_SLOTS: usize = 10;
const WCH_LOGICAL_MAX_X: f64 = 16383.0;
const WCH_LOGICAL_MAX_Y: f64 = 9599.0;

// WCH mouse interface (single touch; the digitizer interface before the Input
// Mode unlock): RID 7, buttons @1, X LE16 @2, Y LE16 @4, wheel @6, over the
// same logical extents.
const WCH_MOUSE_REPORT_ID: u8 = 0x07;
const WCH_MOUSE_REPORT_LENGTH: usize = 7;

// ILITEK (USB 0x222A:0x0001): RID 4, N slots of 5 bytes (contact id at bits
// 0-5, tip switch at 0x40, X and Y LE16 up to 16384 / 9600), then scan time
// LE32 and contact count. N varies with the panel's contact limit, so the slot
// count is derived from each report's own length and capped at 10.
const ILITEK_REPORT_ID: u8 = 0x04;
const ILITEK_REPORT_MIN_LENGTH: usize = 11;
const ILITEK_SLOT_SIZE: usize = 5;
const ILITEK_MAX_SLOTS: usize = 10;
const ILITEK_LOGICAL_MAX_X: f64 = 16384.0;
const ILITEK_LOGICAL_MAX_Y: f64 = 9600.0;

// ILITEK mouse fallback: RID 5, buttons @1, X LE16 @2, Y LE16 @4.
const ILITEK_MOUSE_REPORT_ID: u8 = 0x05;
const ILITEK_MOUSE_REPORT_LENGTH: usize = 6;

/// Decodes one raw input report (report ID included at index 0).
pub fn parse(bytes: &[u8]) -> Option<TouchFrame> {
    let report_id = *bytes.first()?;
    match report_id {
        WCH_REPORT_ID if bytes.len() >= WCH_REPORT_LENGTH => Some(parse_wch_digitizer(bytes)),
        WCH_MOUSE_REPORT_ID if bytes.len() >= WCH_MOUSE_REPORT_LENGTH => Some(parse_wch_mouse(bytes)),
        ILITEK_REPORT_ID if bytes.len() >= ILITEK_REPORT_MIN_LENGTH => Some(parse_ilitek(bytes)),
        ILITEK_MOUSE_REPORT_ID if bytes.len() >= ILITEK_MOUSE_REPORT_LENGTH => Some(parse_ilitek_mouse(bytes)),
        _ => None,
    }
}

fn parse_wch_digitizer(r: &[u8]) -> TouchFrame {
    let count = (r[WCH_CONTACT_COUNT_OFFSET] as usize).min(WCH_MAX_SLOTS);
    let mut contacts = Contacts::new();
    for slot in 0..count {
        let base = 1 + slot * WCH_SLOT_SIZE;
        contacts.push(Contact::new(
            r[base] >> 4,
            r[base] & 0x01 != 0,
            normalize(le16(r, base + 1), WCH_LOGICAL_MAX_X),
            normalize(le16(r, base + 3), WCH_LOGICAL_MAX_Y),
        ));
    }
    TouchFrame { source: Source::Digitizer, contacts, scan_time: Some(u32::from(le16(r, WCH_SCAN_TIME_OFFSET))) }
}

fn parse_wch_mouse(r: &[u8]) -> TouchFrame {
    TouchFrame {
        source: Source::Mouse,
        contacts: Contacts::from_iter([Contact::new(
            0,
            r[1] & 0x01 != 0,
            normalize(le16(r, 2), WCH_LOGICAL_MAX_X),
            normalize(le16(r, 4), WCH_LOGICAL_MAX_Y),
        )]),
        scan_time: None,
    }
}

fn parse_ilitek(r: &[u8]) -> TouchFrame {
    let slot_count = ((r.len() - 6) / ILITEK_SLOT_SIZE).min(ILITEK_MAX_SLOTS);
    let scan_time_offset = 1 + slot_count * ILITEK_SLOT_SIZE;
    let contact_count_offset = scan_time_offset + 4;
    let count = (r[contact_count_offset] as usize).min(slot_count);
    let mut contacts = Contacts::new();
    for slot in 0..count {
        let base = 1 + slot * ILITEK_SLOT_SIZE;
        contacts.push(Contact::new(
            r[base] & 0x3F,
            r[base] & 0x40 != 0,
            normalize(le16(r, base + 1), ILITEK_LOGICAL_MAX_X),
            normalize(le16(r, base + 3), ILITEK_LOGICAL_MAX_Y),
        ));
    }
    let mut scan_time = 0u32;
    for b in 0..4 {
        scan_time |= (r[scan_time_offset + b] as u32) << (8 * b);
    }
    TouchFrame { source: Source::Digitizer, contacts, scan_time: Some(scan_time) }
}

fn parse_ilitek_mouse(r: &[u8]) -> TouchFrame {
    TouchFrame {
        source: Source::Mouse,
        contacts: Contacts::from_iter([Contact::new(
            0,
            r[1] & 0x01 != 0,
            normalize(le16(r, 2), ILITEK_LOGICAL_MAX_X),
            normalize(le16(r, 4), ILITEK_LOGICAL_MAX_Y),
        )]),
        scan_time: None,
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
