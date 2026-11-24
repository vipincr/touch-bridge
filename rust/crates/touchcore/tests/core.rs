//! Tests ported from the Swift TouchCore suite, plus the WCH-specific cases
//! proven on the live JN-MD-IPST105FP panel.

use std::collections::BTreeMap;

use touchcore::filter::SingleTouchFilter;
use touchcore::mapper::{CoordinateMapper, ScreenRect};
use touchcore::mode::{Action, PanelModeWatch};
use touchcore::report::{parse, Contact, Contacts, Source, TouchFrame};
use touchcore::tracker::{ContactTracker, ContactEvent, Phase};
use touchcore::tuio;

fn contacts(list: &[(u8, bool, f64, f64)]) -> Contacts {
    list.iter().map(|&(id, touching, x, y)| Contact::new(id, touching, x, y)).collect()
}

// ---- report decoders ----

fn wch_digitizer_report(slots: &[(u8, bool, u16, u16)], count: Option<u8>, scan_time: u16) -> Vec<u8> {
    let mut r = vec![0u8; 54];
    r[0] = 0x0D;
    for (i, &(id, tip, x, y)) in slots.iter().enumerate() {
        let base = 1 + i * 5;
        r[base] = (id << 4) | u8::from(tip);
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

fn ilitek_digitizer_report(slots: &[(u8, bool, u16, u16)], count: Option<u8>, scan_time: u32) -> Vec<u8> {
    let mut r = vec![0u8; 64];
    r[0] = 0x04;
    for (i, &(id, tip, x, y)) in slots.iter().enumerate() {
        let base = 1 + i * 5;
        r[base] = (id & 0x3F) | u8::from(tip) * 0x40;
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

#[test]
fn parses_wch_digitizer_report() {
    let frame = parse(&wch_digitizer_report(&[(2, true, 16383, 9599)], None, 777)).unwrap();
    assert_eq!(frame.source, Source::Digitizer);
    assert_eq!(frame.scan_time, Some(777));
    assert_eq!(frame.contacts, contacts(&[(2, true, 1.0, 1.0)]));
}

#[test]
fn parses_wch_contact_id_and_tip_nibble_separately() {
    let frame = parse(&wch_digitizer_report(&[(7, false, 0, 0)], None, 0)).unwrap();
    assert_eq!(frame.contacts, contacts(&[(7, false, 0.0, 0.0)]));
}

#[test]
fn parses_only_the_wch_reported_number_of_contacts() {
    let report = wch_digitizer_report(&[(0, true, 0, 0), (1, true, 8192, 4800), (2, true, 16383, 9599)], Some(2), 0);
    let frame = parse(&report).unwrap();
    assert_eq!(frame.contacts.iter().map(|c| c.id).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(frame.contacts[1].x, 8192.0 / 16383.0);
}

#[test]
fn caps_wch_contact_count_at_the_ten_slots() {
    let frame = parse(&wch_digitizer_report(&[(0, true, 1, 1)], Some(99), 0)).unwrap();
    assert_eq!(frame.contacts.len(), 10);
}

#[test]
fn rejects_short_wch_reports() {
    let mut short = vec![0x0D];
    short.extend(vec![0; 52]);
    assert!(parse(&short).is_none());
}

#[test]
fn parses_wch_mouse_report_as_one_contact() {
    // RID 7 as captured from the panel: buttons @1, X LE16 @2, Y LE16 @4, wheel @6.
    let frame = parse(&[0x07, 0x01, 0x44, 0x26, 0x01, 0x15, 0x00]).unwrap();
    assert_eq!(frame.source, Source::Mouse);
    assert_eq!(frame.contacts, contacts(&[(0, true, 9796.0 / 16383.0, 5377.0 / 9599.0)]));
}

#[test]
fn parses_wch_mouse_lift() {
    let frame = parse(&[0x07, 0x00, 0x44, 0x26, 0x01, 0x15, 0x00]).unwrap();
    assert_eq!(frame.contacts, contacts(&[(0, false, 9796.0 / 16383.0, 5377.0 / 9599.0)]));
}

#[test]
fn parses_ilitek_single_contact_as_normalized_point() {
    let frame = parse(&ilitek_digitizer_report(&[(3, true, 8192, 4800)], None, 1234)).unwrap();
    assert_eq!(frame.source, Source::Digitizer);
    assert_eq!(frame.scan_time, Some(1234));
    assert_eq!(frame.contacts, contacts(&[(3, true, 0.5, 0.5)]));
}

#[test]
fn caps_ilitek_contact_count_at_the_ten_slots() {
    let frame = parse(&ilitek_digitizer_report(&[(0, true, 1, 1)], Some(99), 0)).unwrap();
    assert_eq!(frame.contacts.len(), 10);
}

#[test]
fn parses_ilitek_mouse_fallback_as_one_contact() {
    let frame = parse(&[0x05, 0x01, 0x00, 0x20, 0xC0, 0x12]).unwrap();
    assert_eq!(frame.source, Source::Mouse);
    assert_eq!(frame.contacts, contacts(&[(0, true, 0.5, 0.5)]));
}

#[test]
fn rejects_unknown_report_ids_and_empty_input() {
    let mut unknown = vec![0x03];
    unknown.extend(vec![0; 63]);
    assert!(parse(&unknown).is_none());
    assert!(parse(&[0x04, 0x00]).is_none());
    assert!(parse(&[]).is_none());
}

// ---- tracker ----

fn frame(source: Source, list: &[(u8, bool, f64, f64)]) -> TouchFrame {
    TouchFrame { source, contacts: contacts(list), scan_time: None }
}

#[test]
fn tracker_emits_began_moved_ended() {
    let mut tracker = ContactTracker::new(100_000_000);
    let mut events = Vec::new();
    tracker.update_into(&frame(Source::Digitizer, &[(1, true, 0.1, 0.1)]), 0, &mut events);
    assert_eq!(events, vec![ContactEvent { id: 1, phase: Phase::Began, x: 0.1, y: 0.1 }]);
    tracker.update_into(&frame(Source::Digitizer, &[(1, true, 0.2, 0.1)]), 1, &mut events);
    assert_eq!(events.first().map(|e| e.phase), Some(Phase::Moved));
    tracker.update_into(&frame(Source::Digitizer, &[(1, false, 0.2, 0.1)]), 2, &mut events);
    assert_eq!(events.first().map(|e| e.phase), Some(Phase::Ended));
}

#[test]
fn tracker_expires_silent_contacts() {
    let mut tracker = ContactTracker::new(100_000_000);
    let mut events = Vec::new();
    tracker.update_into(&frame(Source::Digitizer, &[(1, true, 0.5, 0.5)]), 0, &mut events);
    tracker.expire_into(1, &mut events);
    assert!(events.is_empty());
    tracker.expire_into(200_000_001, &mut events);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].phase, Phase::Ended);
}

// ---- single-touch filter ----

fn mouse_frame(x: f64, y: f64, touching: bool) -> TouchFrame {
    frame(Source::Mouse, &[(0, touching, x, y)])
}

#[test]
fn small_mouse_movement_passes_through() {
    let mut filter = SingleTouchFilter::new(25.0);
    let (a, b) = filter.filter(mouse_frame(0.5, 0.5, true), 221.9, 147.9);
    assert_eq!(a, mouse_frame(0.5, 0.5, true));
    assert!(b.is_none());
    let (a, b) = filter.filter(mouse_frame(0.52, 0.5, true), 221.9, 147.9);
    assert_eq!(a, mouse_frame(0.52, 0.5, true));
    assert!(b.is_none());
}

#[test]
fn a_teleport_splits_into_lift_and_fresh_touch() {
    let mut filter = SingleTouchFilter::new(25.0);
    _ = filter.filter(mouse_frame(0.1, 0.1, true), 221.9, 147.9);
    let (lift, follow) = filter.filter(mouse_frame(0.8, 0.7, true), 221.9, 147.9);
    assert_eq!(lift.contacts, contacts(&[(0, false, 0.1, 0.1)]));
    assert_eq!(follow.unwrap().contacts, contacts(&[(0, true, 0.8, 0.7)]));
}

#[test]
fn a_lift_then_touch_elsewhere_is_not_split() {
    let mut filter = SingleTouchFilter::new(25.0);
    _ = filter.filter(mouse_frame(0.1, 0.1, true), 221.9, 147.9);
    _ = filter.filter(mouse_frame(0.1, 0.1, false), 221.9, 147.9);
    let (a, b) = filter.filter(mouse_frame(0.8, 0.7, true), 221.9, 147.9);
    assert_eq!(a, mouse_frame(0.8, 0.7, true));
    assert!(b.is_none());
}

#[test]
fn digitizer_frames_pass_through_untouched() {
    let mut filter = SingleTouchFilter::new(25.0);
    let digitizer = frame(Source::Digitizer, &[(1, true, 0.1, 0.1), (2, true, 0.9, 0.9)]);
    let (a, b) = filter.filter(digitizer, 221.9, 147.9);
    assert_eq!(a, frame(Source::Digitizer, &[(1, true, 0.1, 0.1), (2, true, 0.9, 0.9)]));
    assert!(b.is_none());
}

// ---- mode watch ----

const S: u64 = 1_000_000_000;

#[test]
fn a_mouse_report_asks_the_panel_for_multitouch() {
    let mut watch = PanelModeWatch::new();
    assert_eq!(watch.observe(Source::Mouse, 0), Some(Action::RequestMultitouch));
}

#[test]
fn requests_are_rate_limited() {
    let mut watch = PanelModeWatch::new();
    _ = watch.observe(Source::Mouse, 0);
    assert_eq!(watch.observe(Source::Mouse, S), None);
    assert_eq!(watch.observe(Source::Mouse, 6 * S), Some(Action::RequestMultitouch));
}

#[test]
fn mouse_mode_is_normal_never_asks_for_multitouch() {
    let mut watch = PanelModeWatch::new();
    watch.mouse_mode_is_normal = true;
    assert_eq!(watch.observe(Source::Mouse, 0), None);
    assert_eq!(watch.observe(Source::Mouse, 6 * S), None);
    assert_eq!(watch.observe(Source::Digitizer, 7 * S), Some(Action::MultitouchRestored));
}

// ---- mapper ----

#[test]
fn mapper_places_points_inside_the_bounds() {
    let mapper = CoordinateMapper::new(ScreenRect { x: 100.0, y: 50.0, width: 960.0, height: 640.0 }, 0.0);
    assert_eq!(mapper.point(0.0, 0.0).x, 100.0);
    assert_eq!(mapper.point(1.0, 1.0).y, 690.0);
    let clamped = mapper.clamped_point(1.0, 1.0);
    assert!(clamped.x < 1060.0 && clamped.y < 690.0);
}

// ---- TUIO ----

#[test]
fn tuio_frame_round_trips() {
    let cursors = vec![
        tuio::Cursor { session: 1, x: 0.25, y: 0.5, vx: 0.0, vy: 0.0, acceleration: 0.0 },
        tuio::Cursor { session: 2, x: 0.75, y: 0.5, vx: 0.1, vy: -0.1, acceleration: 0.0 },
    ];
    let decoded = tuio::decode(&tuio::frame(&cursors, 7, "touchnext")).unwrap();
    assert_eq!(decoded.alive, vec![1, 2]);
    assert_eq!(decoded.sequence, Some(7));
    assert_eq!(decoded.cursors.len(), 2);
    assert_eq!(decoded.cursors[0].x, 0.25);
    assert_eq!(decoded.cursors[1].vx, 0.1);
}

#[test]
fn tuio_frame_into_matches_frame() {
    let cursors = vec![tuio::Cursor { session: 1, x: 0.5, y: 0.5, vx: 0.0, vy: 0.0, acceleration: 0.0 }];
    let mut buf = Vec::new();
    tuio::frame_into(&cursors, 3, "touchnext", &mut buf);
    assert_eq!(buf, tuio::frame(&cursors, 3, "touchnext"));
    assert_eq!(tuio::decode(&buf).unwrap().alive, vec![1]);
}

#[test]
fn tuio_decode_rejects_non_bundles() {
    assert!(tuio::decode(&[0, 1, 2, 3]).is_none());
    assert!(tuio::decode(&[]).is_none());
}

#[test]
fn sessions_assign_fresh_ids_and_velocity() {
    let mut sessions = tuio::Sessions::new();
    let mut touched = BTreeMap::new();
    touched.insert(3u8, tuio::PanelPoint { x: 0.5, y: 0.5 });
    let first = sessions.update(&touched, 0);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].session, 1);
    touched.insert(3u8, tuio::PanelPoint { x: 0.6, y: 0.5 });
    let second = sessions.update(&touched, 500_000_000);
    assert_eq!(second[0].session, 1);
    assert!(second[0].vx > 0.0);
}
