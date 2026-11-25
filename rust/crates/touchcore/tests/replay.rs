//! Replays real panel recordings from the M1 probe through the parser, the
//! tracker and the recognizer. The fixtures live once, beside the Swift tests
//! they also serve.

use std::collections::{BTreeMap, BTreeSet};

use touchcore::gestures::{GestureAction, GesturePhase, GestureRecognizer};
use touchcore::report::ReportParser;
use touchcore::tracker::{ContactEvent, ContactPhase, ContactTracker};

/// One M1 probe recording: a JSON line per input report, carrying `t_ns` and
/// `hex`. The lines are regular, so they are read with plain string finds
/// instead of a JSON crate; only these two keys matter here.
struct Recording {
    frames: Vec<(u64, Vec<u8>)>,
}

impl Recording {
    fn new(source: &'static str) -> Self {
        let frames = source
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let ns = t_ns_of(line);
                let hex = hex_of(line);
                let mut bytes = Vec::with_capacity(hex.len() / 2);
                for i in (0..hex.len()).step_by(2) {
                    bytes.push(u8::from_str_radix(&hex[i..i + 2], 16).expect("bad hex byte"));
                }
                (ns, bytes)
            })
            .collect();
        Self { frames }
    }
}

fn t_ns_of(line: &str) -> u64 {
    let key = "\"t_ns\":";
    let rest = line[line.find(key).expect("recording line without t_ns") + key.len()..].trim_start();
    let digits = rest.split(|c: char| !c.is_ascii_digit()).next().unwrap_or("");
    digits.parse().expect("bad t_ns")
}

fn hex_of(line: &str) -> &str {
    let key = "\"hex\":";
    let rest = line[line.find(key).expect("recording line without hex") + key.len()..].trim_start();
    let start = rest.find('"').expect("unquoted hex") + 1;
    let end = rest[start..].find('"').expect("unterminated hex") + start;
    &rest[start..end]
}

#[test]
fn every_recorded_frame_parses() {
    let recording = Recording::new(include_str!(
        "../../../../Tests/TouchCoreTests/Fixtures/probe-session-1.jsonl"
    ));
    assert_eq!(recording.frames.len(), 3941);
    assert!(recording.frames.iter().all(|(_, bytes)| ReportParser::parse(bytes).is_some()));
}

#[test]
fn tracker_keeps_a_consistent_lifecycle_on_real_data() {
    let recording = Recording::new(include_str!(
        "../../../../Tests/TouchCoreTests/Fixtures/probe-session-1.jsonl"
    ));
    let mut tracker = ContactTracker::default();
    let mut active = BTreeSet::new();
    let mut begins = 0;
    let mut ends = 0;

    fn apply(events: &[ContactEvent], active: &mut BTreeSet<u8>, begins: &mut usize, ends: &mut usize) {
        for e in events {
            match e.phase {
                ContactPhase::Began => {
                    assert!(!active.contains(&e.id), "id {} began twice", e.id);
                    active.insert(e.id);
                    *begins += 1;
                }
                ContactPhase::Moved => {
                    assert!(active.contains(&e.id), "id {} moved before beginning", e.id);
                }
                ContactPhase::Ended => {
                    assert!(active.contains(&e.id), "id {} ended without beginning", e.id);
                    active.remove(&e.id);
                    *ends += 1;
                }
            }
        }
    }

    for &(ns, ref bytes) in &recording.frames {
        apply(&tracker.expire(ns), &mut active, &mut begins, &mut ends);
        let frame = ReportParser::parse(bytes).expect("probe recording holds only decodable reports");
        apply(&tracker.update(&frame, ns), &mut active, &mut begins, &mut ends);
        assert!(tracker.active_count() <= 10);
    }
    let last_ns = recording.frames.last().expect("empty recording").0;
    apply(&tracker.expire(last_ns + 1_000_000_000), &mut active, &mut begins, &mut ends);

    assert!(active.is_empty());
    assert_eq!(begins, ends);
    assert!(begins > 40); // the session had 40+ separate touches
}

#[test]
fn recognizer_balances_every_gesture_on_real_data() {
    let recording = Recording::new(include_str!(
        "../../../../Tests/TouchCoreTests/Fixtures/probe-session-1.jsonl"
    ));
    let mut tracker = ContactTracker::default();
    let mut recognizer = GestureRecognizer::default();
    let mut actions: Vec<GestureAction> = Vec::new();
    for &(ns, ref bytes) in &recording.frames {
        actions.extend(recognizer.handle(&tracker.expire(ns), ns));
        let frame = ReportParser::parse(bytes).expect("probe recording holds only decodable reports");
        actions.extend(recognizer.handle(&tracker.update(&frame, ns), ns));
    }
    let end = recording.frames.last().expect("empty recording").0 + 1_000_000_000;
    actions.extend(recognizer.handle(&tracker.expire(end), end));

    let mut open: Option<&'static str> = None;
    let mut histogram: BTreeMap<&'static str, usize> = BTreeMap::new();
    for action in &actions {
        let (name, opens, closes): (&str, bool, bool) = match action {
            GestureAction::Tap { .. } => ("tap", false, false),
            GestureAction::RightClick { .. } => ("rightClick", false, false),
            GestureAction::DragBegan { .. } => ("drag", true, false),
            GestureAction::DragMoved { .. } => ("drag", false, false),
            GestureAction::DragEnded { .. } => ("drag", false, true),
            GestureAction::Scroll { phase, .. } => ("scroll", *phase == GesturePhase::Began, false),
            GestureAction::ScrollEnded { .. } => ("scroll", false, true),
            GestureAction::Magnify { phase, .. } => {
                ("magnify", *phase == GesturePhase::Began, *phase == GesturePhase::Ended)
            }
            GestureAction::Rotate { phase, .. } => {
                ("rotate", *phase == GesturePhase::Began, *phase == GesturePhase::Ended)
            }
            GestureAction::DockSwipe { phase, .. } => {
                ("dockSwipe", *phase == GesturePhase::Began, *phase == GesturePhase::Ended)
            }
            GestureAction::DockSwipeCancelled { .. } => ("dockSwipe", false, true),
            GestureAction::NotificationCenter => ("notificationCenter", false, false),
        };
        if opens {
            assert!(open.is_none(), "{} began while {} was open", name, open.unwrap_or("nothing"));
            open = Some(name);
            *histogram.entry(name).or_insert(0) += 1;
        } else if closes {
            assert!(open == Some(name), "{} ended while {} was open", name, open.unwrap_or("nothing"));
            open = None;
        } else if matches!(name, "tap" | "rightClick" | "notificationCenter") {
            assert!(open.is_none(), "{} outside a gesture", name);
            *histogram.entry(name).or_insert(0) += 1;
        } else {
            assert!(open == Some(name), "{} update outside its gesture", name);
        }
    }
    assert!(open.is_none());
    println!("recognized in probe-session-1: {:?}", histogram);
}

// probe-multifinger-2026-11-24 holds 24 s of two- and three-finger gestures and two one-finger
// drags on the JN-MD-IPST105FP: 842 mouse reports, 70 touches. Five of its lift reports carry the
// coordinates (0, 0).

#[test]
fn every_ended_event_sits_where_its_finger_last_was() {
    let recording = Recording::new(include_str!(
        "../../../../Tests/TouchCoreTests/Fixtures/probe-multifinger-2026-11-24.jsonl"
    ));
    assert_eq!(recording.frames.len(), 842);
    let mut tracker = ContactTracker::default();
    let mut last: BTreeMap<u8, (f64, f64)> = BTreeMap::new();
    let mut lifts = 0;
    for &(ns, ref bytes) in &recording.frames {
        let mut events = tracker.expire(ns);
        let frame = ReportParser::parse(bytes).expect("probe recording holds only decodable reports");
        events.extend(tracker.update(&frame, ns));
        for event in events {
            if event.phase == ContactPhase::Ended {
                assert_eq!(
                    last.get(&event.id).copied(),
                    Some((event.x, event.y)),
                    "id {} ended away from where its finger was",
                    event.id
                );
                lifts += 1;
            } else {
                last.insert(event.id, (event.x, event.y));
            }
        }
    }
    assert!(lifts > 0);
}

#[test]
fn no_tap_lands_at_the_panels_origin() {
    let recording = Recording::new(include_str!(
        "../../../../Tests/TouchCoreTests/Fixtures/probe-multifinger-2026-11-24.jsonl"
    ));
    let mut tracker = ContactTracker::default();
    let mut recognizer = GestureRecognizer::default();
    let mut actions: Vec<GestureAction> = Vec::new();
    for &(ns, ref bytes) in &recording.frames {
        actions.extend(recognizer.handle(&tracker.expire(ns), ns));
        let frame = ReportParser::parse(bytes).expect("probe recording holds only decodable reports");
        actions.extend(recognizer.handle(&tracker.update(&frame, ns), ns));
    }
    let end = recording.frames.last().expect("empty recording").0 + 1_000_000_000;
    actions.extend(recognizer.handle(&tracker.expire(end), end));
    let at_origin = actions.iter().any(|action| matches!(
        action,
        GestureAction::Tap { at, .. } if at.x == 0.0 && at.y == 0.0
    ));
    assert!(!at_origin, "the panel's origin is the Apple menu on a Mac");
}
