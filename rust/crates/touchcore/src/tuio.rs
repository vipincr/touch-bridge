//! TUIO 1.1 "2Dcur": the multitouch-table protocol that creative apps read
//! (TouchDesigner, Processing, openFrameworks, Unity, Max, Kivy), usually over
//! UDP on port 3333. OSC encoding: big-endian numbers, strings null-terminated
//! and padded to a multiple of four bytes, messages grouped in a bundle.

use std::collections::BTreeMap;

/// A point on the panel in its native orientation, normalized to 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub session: i32,
    /// Position on the touch surface, 0..=1, origin top-left.
    pub x: f32,
    pub y: f32,
    /// Velocity in surface widths / heights per second.
    pub vx: f32,
    pub vy: f32,
    pub acceleration: f32,
}

pub const ADDRESS: &str = "/tuio/2Dcur";

enum Argument {
    String(String),
    Int(i32),
    Float(f32),
}

fn osc_string(s: &str) -> Vec<u8> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
    bytes
}

fn osc_int32(v: i32) -> [u8; 4] {
    v.to_be_bytes()
}

fn osc_float32(v: f32) -> [u8; 4] {
    v.to_bits().to_be_bytes()
}

fn osc_message(address: &str, arguments: &[Argument]) -> Vec<u8> {
    let mut tags = String::from(",");
    let mut payload = Vec::new();
    for argument in arguments {
        match argument {
            Argument::String(s) => {
                tags.push('s');
                payload.extend(osc_string(s));
            }
            Argument::Int(i) => {
                tags.push('i');
                payload.extend(osc_int32(*i));
            }
            Argument::Float(f) => {
                tags.push('f');
                payload.extend(osc_float32(*f));
            }
        }
    }
    let mut bytes = osc_string(address);
    bytes.extend(osc_string(&tags));
    bytes.extend(payload);
    bytes
}

/// One frame: which cursors are alive (an empty list removes them all), one
/// `set` per cursor, and the frame number.
pub fn frame(cursors: &[Cursor], sequence: i32, source: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(64 + cursors.len() * 32);
    frame_into(cursors, sequence, source, &mut bytes);
    bytes
}

/// [`frame`] into a caller-owned buffer, for send loops that reuse one.
pub fn frame_into(cursors: &[Cursor], sequence: i32, source: &str, bytes: &mut Vec<u8>) {
    bytes.clear();
    bytes.extend(osc_string("#bundle"));
    bytes.extend([0, 0, 0, 0, 0, 0, 0, 1]); // handled immediately
    let source = osc_message(ADDRESS, &[Argument::String(String::from("source")), Argument::String(String::from(source))]);
    let mut alive_args = vec![Argument::String(String::from("alive"))];
    alive_args.extend(cursors.iter().map(|c| Argument::Int(c.session)));
    let alive = osc_message(ADDRESS, &alive_args);
    bytes.extend(osc_int32(source.len() as i32));
    bytes.extend(&source);
    bytes.extend(osc_int32(alive.len() as i32));
    bytes.extend(&alive);
    for c in cursors {
        let set = osc_message(
            ADDRESS,
            &[
                Argument::String(String::from("set")),
                Argument::Int(c.session),
                Argument::Float(c.x),
                Argument::Float(c.y),
                Argument::Float(c.vx),
                Argument::Float(c.vy),
                Argument::Float(c.acceleration),
            ],
        );
        bytes.extend(osc_int32(set.len() as i32));
        bytes.extend(&set);
    }
    let fseq = osc_message(ADDRESS, &[Argument::String(String::from("fseq")), Argument::Int(sequence)]);
    bytes.extend(osc_int32(fseq.len() as i32));
    bytes.extend(&fseq);
}

/// One decoded "2Dcur" bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub alive: Vec<i32>,
    pub cursors: Vec<Cursor>,
    pub sequence: Option<i32>,
}

/// Reads a TUIO 1.1 "2Dcur" bundle, from [`frame`] or any other sender.
/// `None` for anything else; malformed input never traps.
pub fn decode(bytes: &[u8]) -> Option<Frame> {
    let mut reader = OscReader { bytes };
    reader.string()?; // "#bundle"
    reader.skip(8)?;
    let mut alive = Vec::new();
    let mut cursors = Vec::new();
    let mut sequence = None;
    let mut saw_tuio = false;
    while !reader.at_end() {
        let size = reader.int32()?;
        if size <= 0 {
            return None;
        }
        let body = reader.take(size as usize)?;
        let mut message = OscReader { bytes: body };
        if message.string()? != ADDRESS {
            continue;
        }
        let tags = message.string()?;
        let mut tag_bytes = tags.bytes();
        if tag_bytes.next() != Some(b',') {
            continue;
        }
        #[derive(Debug)]
        enum Arg {
            String(String),
            Int(i32),
            Float(f32),
        }
        let mut args = Vec::new();
        for tag in tag_bytes {
            match tag {
                b's' => args.push(Arg::String(message.string()?)),
                b'i' => args.push(Arg::Int(message.int32()?)),
                b'f' => args.push(Arg::Float(f32::from_bits(message.int32()? as u32))),
                _ => return None,
            }
        }
        saw_tuio = true;
        let Arg::String(command) = args.first()? else { continue };
        match command.as_str() {
            "alive" => alive = args.iter().skip(1).filter_map(|a| if let Arg::Int(i) = a { Some(*i) } else { None }).collect(),
            "set" => {
                if args.len() >= 7 {
                    if let (Arg::Int(s), Arg::Float(x), Arg::Float(y), Arg::Float(vx), Arg::Float(vy), Arg::Float(a)) =
                        (&args[1], &args[2], &args[3], &args[4], &args[5], &args[6])
                    {
                        cursors.push(Cursor { session: *s, x: *x, y: *y, vx: *vx, vy: *vy, acceleration: *a });
                    }
                }
            }
            "fseq" => {
                if args.len() >= 2 {
                    if let Arg::Int(f) = args[1] {
                        sequence = Some(f);
                    }
                }
            }
            _ => {}
        }
    }
    saw_tuio.then_some(Frame { alive, cursors, sequence })
}

struct OscReader<'a> {
    bytes: &'a [u8],
}

impl<'a> OscReader<'a> {
    fn at_end(&self) -> bool {
        self.bytes.is_empty()
    }

    fn string(&mut self) -> Option<String> {
        let end = self.bytes.iter().position(|&b| b == 0)?;
        let consumed = (end + 4) & !3;
        if consumed > self.bytes.len() {
            return None;
        }
        let s = String::from_utf8_lossy(&self.bytes[..end]).into_owned();
        self.bytes = &self.bytes[consumed..];
        Some(s)
    }

    fn int32(&mut self) -> Option<i32> {
        if self.bytes.len() < 4 {
            return None;
        }
        let v = i32::from_be_bytes([self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3]]);
        self.bytes = &self.bytes[4..];
        Some(v)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        if self.bytes.len() < n {
            return None;
        }
        self.bytes = &self.bytes[n..];
        Some(())
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.bytes.len() < n {
            return None;
        }
        let taken = &self.bytes[..n];
        self.bytes = &self.bytes[n..];
        Some(taken)
    }
}

/// Turns the panel's contacts into TUIO cursors. The panel reuses contact ids
/// for new touches, but TUIO needs a fresh session id per touch, plus velocity
/// and acceleration.
#[derive(Debug)]
pub struct Sessions {
    inner: BTreeMap<u8, Session>,
    next: i32,
}

#[derive(Debug, Clone, Copy)]
struct Session {
    id: i32,
    last: PanelPoint,
    ns: u64,
    speed: f64,
}

impl Sessions {
    pub fn new() -> Self {
        Self { inner: BTreeMap::new(), next: 1 }
    }

    pub fn update(&mut self, contacts: &BTreeMap<u8, PanelPoint>, now_ns: u64) -> Vec<Cursor> {
        self.inner.retain(|id, _| contacts.contains_key(id));
        let mut cursors = Vec::new();
        for (&contact, &p) in contacts {
            match self.inner.get_mut(&contact) {
                None => {
                    self.inner.insert(contact, Session { id: self.next, last: p, ns: now_ns, speed: 0.0 });
                    cursors.push(Cursor { session: self.next, x: p.x as f32, y: p.y as f32, vx: 0.0, vy: 0.0, acceleration: 0.0 });
                    self.next = if self.next == i32::MAX { 1 } else { self.next + 1 };
                }
                Some(session) => {
                    let dt = now_ns.wrapping_sub(session.ns) as f64 / 1e9;
                    let (vx, vy) = if dt > 0.0 { ((p.x - session.last.x) / dt, (p.y - session.last.y) / dt) } else { (0.0, 0.0) };
                    let speed = (vx * vx + vy * vy).sqrt();
                    let acceleration = if dt > 0.0 { (speed - session.speed) / dt } else { 0.0 };
                    *session = Session { id: session.id, last: p, ns: now_ns, speed };
                    cursors.push(Cursor {
                        session: session.id,
                        x: p.x as f32,
                        y: p.y as f32,
                        vx: vx as f32,
                        vy: vy as f32,
                        acceleration: acceleration as f32,
                    });
                }
            }
        }
        cursors.sort_by_key(|c| c.session);
        cursors
    }
}

impl Default for Sessions {
    fn default() -> Self {
        Self::new()
    }
}
