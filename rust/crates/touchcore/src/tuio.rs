//! TUIO 1.1 "2Dcur": the multitouch-table protocol that creative apps read
//! (TouchDesigner, Processing, openFrameworks, Unity, Max, Kivy), usually over
//! UDP on port 3333. Open Sound Control encoding, as TUIO sends it:
//! big-endian numbers, strings null-terminated and padded to a multiple of
//! four bytes, messages grouped in a bundle.

use std::collections::BTreeMap;

use crate::gestures::PanelPoint;

/// One OSC argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Argument {
    String(String),
    Int(i32),
    Float(f32),
}

/// OSC encoding.
pub struct OSC;

impl OSC {
    /// A string: the UTF-8 bytes, null-terminated and padded to a multiple of four bytes.
    pub fn string(s: &str) -> Vec<u8> {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        while !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        bytes
    }

    /// A big-endian 32-bit integer.
    pub fn int32(v: i32) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    /// A big-endian 32-bit float.
    pub fn float32(v: f32) -> Vec<u8> {
        v.to_bits().to_be_bytes().to_vec()
    }

    /// One message: the address, then the type-tag string, then the arguments.
    pub fn message(address: &str, arguments: &[Argument]) -> Vec<u8> {
        let mut tags = String::from(",");
        let mut payload = Vec::new();
        for argument in arguments {
            match argument {
                Argument::String(s) => {
                    tags.push('s');
                    payload.extend(Self::string(s));
                }
                Argument::Int(i) => {
                    tags.push('i');
                    payload.extend(Self::int32(*i));
                }
                Argument::Float(f) => {
                    tags.push('f');
                    payload.extend(Self::float32(*f));
                }
            }
        }
        let mut bytes = Self::string(address);
        bytes.extend(Self::string(&tags));
        bytes.extend(payload);
        bytes
    }

    /// A bundle to be handled immediately (time tag 1), each message preceded by its size.
    pub fn bundle(messages: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = Self::string("#bundle");
        bytes.extend([0, 0, 0, 0, 0, 0, 0, 1]);
        for message in messages {
            bytes.extend(Self::int32(message.len() as i32));
            bytes.extend(message);
        }
        bytes
    }
}

/// TUIO 1.1 "2Dcur" frames.
pub struct TUIO;

impl TUIO {
    /// The address every frame message carries.
    pub const ADDRESS: &'static str = "/tuio/2Dcur";

    /// One frame: who sends it, which cursors are alive (an empty list removes
    /// them all), one `set` per cursor, and the frame number. Sent under the
    /// source name Swift uses by default, "TouchNext".
    pub fn frame(cursors: &[Cursor], sequence: i32) -> Vec<u8> {
        let mut messages = vec![
            OSC::message(Self::ADDRESS, &[Argument::String(String::from("source")), Argument::String(String::from("TouchNext"))]),
        ];
        let mut alive = vec![Argument::String(String::from("alive"))];
        alive.extend(cursors.iter().map(|c| Argument::Int(c.session)));
        messages.push(OSC::message(Self::ADDRESS, &alive));
        for c in cursors {
            messages.push(OSC::message(
                Self::ADDRESS,
                &[
                    Argument::String(String::from("set")),
                    Argument::Int(c.session),
                    Argument::Float(c.x),
                    Argument::Float(c.y),
                    Argument::Float(c.vx),
                    Argument::Float(c.vy),
                    Argument::Float(c.acceleration),
                ],
            ));
        }
        messages.push(OSC::message(Self::ADDRESS, &[Argument::String(String::from("fseq")), Argument::Int(sequence)]));
        OSC::bundle(&messages)
    }
}

/// One decoded "2Dcur" bundle: the alive list, the `set` cursors, and the
/// frame number when the sender sent one.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub alive: Vec<i32>,
    pub cursors: Vec<Cursor>,
    pub sequence: Option<i32>,
}

impl TUIO {
    /// Reads a TUIO 1.1 "2Dcur" bundle, from [`TUIO::frame`] or any other
    /// sender. `None` for anything else; malformed input (anyone on this Mac
    /// can send to the port) never traps: a message that doesn't parse is
    /// skipped, one whose arguments run out ends the decode.
    pub fn decode(bytes: &[u8]) -> Option<Frame> {
        let mut reader = OscReader { bytes };
        if reader.string().as_deref() != Some("#bundle") || reader.skip(8).is_none() {
            return None;
        }
        let mut alive: Vec<i32> = Vec::new();
        let mut cursors: Vec<Cursor> = Vec::new();
        let mut sequence: Option<i32> = None;
        let mut saw_tuio = false;
        while !reader.at_end() {
            let size = reader.int32()?;
            if size <= 0 {
                return None;
            }
            let body = reader.take(size as usize)?;
            let mut message = OscReader { bytes: body };
            if message.string().as_deref() != Some(Self::ADDRESS) {
                continue;
            }
            let Some(tags) = message.string() else { continue };
            if !tags.starts_with(',') {
                continue;
            }
            let mut args: Vec<Argument> = Vec::new();
            for tag in tags.chars().skip(1) {
                match tag {
                    's' => args.push(Argument::String(message.string()?)),
                    'i' => args.push(Argument::Int(message.int32()?)),
                    'f' => args.push(Argument::Float(message.float32()?)),
                    _ => return None,
                }
            }
            saw_tuio = true;
            let Some(Argument::String(command)) = args.first() else { continue };
            match command.as_str() {
                "alive" => {
                    alive = args
                        .iter()
                        .skip(1)
                        .filter_map(|a| if let Argument::Int(i) = a { Some(*i) } else { None })
                        .collect();
                }
                "set" => {
                    if args.len() >= 7
                        && let (Some(&Argument::Int(s)), Some(&Argument::Float(x)), Some(&Argument::Float(y)), Some(&Argument::Float(vx)), Some(&Argument::Float(vy)), Some(&Argument::Float(a)))
                            = (args.get(1), args.get(2), args.get(3), args.get(4), args.get(5), args.get(6))
                    {
                        cursors.push(Cursor { session: s, x, y, vx, vy, acceleration: a });
                    }
                }
                "fseq" => {
                    if args.len() >= 2
                        && let Some(&Argument::Int(f)) = args.get(1)
                    {
                        sequence = Some(f);
                    }
                }
                _ => {}
            }
        }
        saw_tuio.then_some(Frame { alive, cursors, sequence })
    }
}

/// One TUIO cursor as a frame carries it.
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

/// Reads OSC values from a byte slice, failing (`None`) instead of reading past the end.
struct OscReader<'a> {
    bytes: &'a [u8],
}

impl<'a> OscReader<'a> {
    fn at_end(&self) -> bool {
        self.bytes.is_empty()
    }

    /// A padded, null-terminated string; consumes the terminator and its padding.
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

    fn float32(&mut self) -> Option<f32> {
        self.int32().map(|v| f32::from_bits(v as u32))
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
pub struct TUIOSessions {
    sessions: BTreeMap<u8, Session>,
    next: i32,
}

#[derive(Debug, Clone, Copy)]
struct Session {
    id: i32,
    last: PanelPoint,
    ns: u64,
    speed: f64,
}

impl TUIOSessions {
    pub fn new() -> Self {
        Self { sessions: BTreeMap::new(), next: 1 }
    }

    /// One frame's cursors, in session order. Contacts that vanished lose
    /// their session; new contacts get the next fresh session id (1 after
    /// Int32's max, though no panel reaches that).
    pub fn update(&mut self, contacts: &BTreeMap<u8, PanelPoint>, now_ns: u64) -> Vec<Cursor> {
        self.sessions.retain(|id, _| contacts.contains_key(id));
        let mut cursors = Vec::new();
        for (&contact, &p) in contacts {
            match self.sessions.get_mut(&contact) {
                None => {
                    let id = self.next;
                    self.sessions.insert(contact, Session { id, last: p, ns: now_ns, speed: 0.0 });
                    cursors.push(Cursor { session: id, x: p.x as f32, y: p.y as f32, vx: 0.0, vy: 0.0, acceleration: 0.0 });
                    self.next = if self.next == i32::MAX { 1 } else { self.next + 1 };
                }
                Some(session) => {
                    let dt = now_ns.wrapping_sub(session.ns) as f64 / 1e9;
                    let (vx, vy) = if dt > 0.0 {
                        ((p.x - session.last.x) / dt, (p.y - session.last.y) / dt)
                    } else {
                        (0.0, 0.0)
                    };
                    let speed = vx.hypot(vy);
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

impl Default for TUIOSessions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal OSC reader, as a TUIO client would parse the bytes.
    #[derive(Debug, Clone, PartialEq)]
    enum Arg {
        S(String),
        I(i32),
        F(f32),
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Message {
        address: String,
        args: Vec<Arg>,
    }

    fn read_string(b: &[u8], i: &mut usize) -> String {
        let end = *i + b[*i..].iter().position(|&c| c == 0).expect("string is null-terminated");
        let s = String::from_utf8_lossy(&b[*i..end]).into_owned();
        *i = (end + 4) & !3;
        s
    }

    fn read_int(b: &[u8], i: &mut usize) -> i32 {
        let v = i32::from_be_bytes([b[*i], b[*i + 1], b[*i + 2], b[*i + 3]]);
        *i += 4;
        v
    }

    fn read_bundle(b: &[u8]) -> Vec<Message> {
        let mut i = 0;
        assert_eq!(read_string(b, &mut i), "#bundle");
        assert_eq!(&b[i..i + 8], &[0, 0, 0, 0, 0, 0, 0, 1], "time tag: immediately");
        i += 8;
        let mut messages: Vec<Message> = Vec::new();
        while i < b.len() {
            let size = read_int(b, &mut i) as usize;
            let end = i + size;
            let address = read_string(b, &mut i);
            let tags = read_string(b, &mut i);
            let mut args: Vec<Arg> = Vec::new();
            for tag in tags.chars().skip(1) {
                match tag {
                    's' => args.push(Arg::S(read_string(b, &mut i))),
                    'i' => args.push(Arg::I(read_int(b, &mut i))),
                    'f' => args.push(Arg::F(f32::from_bits(read_int(b, &mut i) as u32))),
                    _ => panic!("unexpected tag {tag}"),
                }
            }
            assert_eq!(i, end, "message size matches its contents");
            messages.push(Message { address, args });
        }
        messages
    }

    #[test]
    fn osc_strings_are_null_terminated_and_padded_to_four_bytes() {
        assert_eq!(OSC::string("alive"), b"alive".iter().copied().chain([0, 0, 0]).collect::<Vec<_>>());
        assert_eq!(OSC::string("set"), b"set".iter().copied().chain([0]).collect::<Vec<_>>());
        assert_eq!(OSC::string("fseq"), b"fseq".iter().copied().chain([0, 0, 0, 0]).collect::<Vec<_>>());
        assert_eq!(OSC::int32(258), [0, 0, 1, 2]);
        assert_eq!(OSC::float32(1.0), [0x3F, 0x80, 0, 0]);
    }

    #[test]
    fn a_frame_carries_source_alive_sets_and_sequence() {
        let cursors = [
            Cursor { session: 4, x: 0.25, y: 0.5, vx: 0.1, vy: -0.2, acceleration: 0.0 },
            Cursor { session: 9, x: 0.75, y: 0.125, vx: 0.0, vy: 0.0, acceleration: 0.0 },
        ];
        let messages = read_bundle(&TUIO::frame(&cursors, 7));
        assert!(messages.iter().all(|m| m.address == "/tuio/2Dcur"));
        let first_args: Vec<Arg> = messages.iter().map(|m| m.args[0].clone()).collect();
        assert_eq!(first_args, [Arg::S("source".into()), Arg::S("alive".into()), Arg::S("set".into()), Arg::S("set".into()), Arg::S("fseq".into())]);
        assert_eq!(messages[0].args, [Arg::S("source".into()), Arg::S("TouchNext".into())]);
        assert_eq!(messages[1].args, [Arg::S("alive".into()), Arg::I(4), Arg::I(9)]);
        assert_eq!(messages[2].args, [Arg::S("set".into()), Arg::I(4), Arg::F(0.25), Arg::F(0.5), Arg::F(0.1), Arg::F(-0.2), Arg::F(0.0)]);
        assert_eq!(messages[4].args, [Arg::S("fseq".into()), Arg::I(7)]);
    }

    #[test]
    fn an_empty_frame_tells_clients_every_cursor_is_gone() {
        let messages = read_bundle(&TUIO::frame(&[], 8));
        let all_args: Vec<Arg> = messages.iter().flat_map(|m| m.args.iter().cloned()).collect();
        assert_eq!(
            all_args,
            [Arg::S("source".into()), Arg::S("TouchNext".into()), Arg::S("alive".into()), Arg::S("fseq".into()), Arg::I(8)]
        );
    }

    const MS: u64 = 1_000_000;

    fn contacts(pairs: &[(u8, f64, f64)]) -> BTreeMap<u8, PanelPoint> {
        pairs.iter().map(|&(id, x, y)| (id, PanelPoint { x, y })).collect()
    }

    #[test]
    fn each_touch_gets_its_own_session_even_when_the_panel_reuses_the_id() {
        let mut sessions = TUIOSessions::new();
        let got = sessions.update(&contacts(&[(0, 0.1, 0.1)]), 0);
        assert_eq!(got.iter().map(|c| c.session).collect::<Vec<_>>(), [1]);
        let got = sessions.update(&contacts(&[(0, 0.1, 0.1), (1, 0.5, 0.5)]), 10 * MS);
        assert_eq!(got.iter().map(|c| c.session).collect::<Vec<_>>(), [1, 2]);
        let got = sessions.update(&contacts(&[(1, 0.5, 0.5)]), 20 * MS);
        assert_eq!(got.iter().map(|c| c.session).collect::<Vec<_>>(), [2], "the first touch lifted");
        let got = sessions.update(&contacts(&[(0, 0.3, 0.3), (1, 0.5, 0.5)]), 30 * MS);
        assert_eq!(got.iter().map(|c| c.session).collect::<Vec<_>>(), [2, 3], "the panel reused id 0 for a new touch");
    }

    #[test]
    fn cursors_carry_their_velocity() {
        let mut sessions = TUIOSessions::new();
        sessions.update(&contacts(&[(0, 0.1, 0.2)]), 0);
        let moved = sessions.update(&contacts(&[(0, 0.2, 0.2)]), 100 * MS);
        assert!((moved[0].vx - 1.0).abs() < 1e-4, "0.1 of the surface in 0.1 s");
        assert_eq!(moved[0].vy, 0.0);
        assert_eq!(moved[0].x, 0.2);
    }

    // The Event Sink reads TUIO to paint every finger (owner: "a valid test environment" for multitouch).
    #[test]
    fn a_frame_decodes_back_to_its_cursors() {
        let cursors = [
            Cursor { session: 4, x: 0.25, y: 0.5, vx: 0.1, vy: -0.2, acceleration: 0.0 },
            Cursor { session: 9, x: 0.75, y: 0.125, vx: 0.0, vy: 0.0, acceleration: 3.0 },
        ];
        let frame = TUIO::decode(&TUIO::frame(&cursors, 7));
        assert_eq!(frame, Some(Frame { alive: vec![4, 9], cursors: cursors.to_vec(), sequence: Some(7) }));
    }

    #[test]
    fn an_empty_frame_decodes_to_no_cursors() {
        assert_eq!(TUIO::decode(&TUIO::frame(&[], 3)), Some(Frame { alive: vec![], cursors: vec![], sequence: Some(3) }));
    }

    #[test]
    fn junk_never_crashes_the_decoder() {
        let good = TUIO::frame(&[Cursor { session: 1, x: 0.5, y: 0.5, vx: 0.0, vy: 0.0, acceleration: 0.0 }], 1);
        assert_eq!(TUIO::decode(&[]), None);
        assert_eq!(TUIO::decode(b"hello"), None);
        for cut in (1..good.len()).step_by(3) {
            TUIO::decode(&good[..cut]); // truncated
        }
        let mut corrupt = good.clone();
        corrupt[17] = 0x7F; // a huge message size
        TUIO::decode(&corrupt);
        assert!(TUIO::decode(&good).is_some(), "the intact frame still decodes");
    }
}
