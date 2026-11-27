//! Replays real panel recordings through the parser, the contact stream and the recognizer, fed
//! the way the pipeline feeds them. The recordings are in `fixtures/`, beside this file.

use std::collections::BTreeMap;

use touchcore::gestures::{GestureAction, GestureConfig, GesturePhase, GestureRecognizer, OneFingerDragMode};
use touchcore::report::ReportParser;
use touchcore::stream::ContactStream;
use touchcore::tracker::{ContactEvent, ContactPhase};

/// One recording: a JSON line per input report, carrying `t_ns` and `hex`. The lines are
/// regular, so they are read with plain string finds instead of a JSON crate; only these two
/// keys matter here.
struct Recording {
    frames: Vec<(u64, Vec<u8>)>,
}

impl Recording {
    fn new(source: &str) -> Self {
        let frames = source
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let hex = hex_of(line);
                let mut bytes = Vec::with_capacity(hex.len() / 2);
                for i in (0..hex.len()).step_by(2) {
                    bytes.push(u8::from_str_radix(&hex[i..i + 2], 16).expect("bad hex byte"));
                }
                (t_ns_of(line), bytes)
            })
            .collect();
        Self { frames }
    }

    fn len(&self) -> usize {
        self.frames.len()
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

/// One contact as the stream reported it: when and where it began, where its last frame was,
/// and when and where it ended. Positions are in units of the panel; times are nanoseconds into
/// the recording.
struct Contact {
    /// When the stream announced the contact.
    began_ns: u64,
    /// When its first frame came: the stream dates a touch it held from then.
    landed_ns: u64,
    began: (f64, f64),
    /// The position and time of its last frame with the tip down.
    last: (f64, f64),
    last_ns: u64,
    /// How many frames of the tip down it had.
    frames: usize,
    ended_ns: u64,
    /// The position its end event names.
    ended: (f64, f64),
    /// The stream's verdict: the touch was the panel's misread of the landing that followed it.
    misread: bool,
}

/// What a recording became: its contacts, and the recognizer's actions with the time of the
/// events that made them.
struct Replay {
    contacts: Vec<Contact>,
    actions: Vec<(u64, GestureAction)>,
    /// How many frames went out with a later frame of their own: the filter held them until
    /// then, and the contact stood still and jumped.
    held_frames: usize,
}

/// Feeds a recording the way the pipeline does: each report at its recorded time, and before
/// it whatever deadline the contact stream has passed by then. The recognizer's thresholds are
/// `config`; the stream measures in `panel`.
///
/// Every contact is checked as it goes: one that begins twice, moves before it begins, or
/// ends without having begun fails the test.
fn replay(source: &str, panel: (f64, f64), config: GestureConfig) -> Replay {
    let recording = Recording::new(source);
    let mut stream = ContactStream::for_panel(panel.0, panel.1);
    let mut recognizer = GestureRecognizer::new(config);
    let mut replay = Replay { contacts: Vec::new(), actions: Vec::new(), held_frames: 0 };
    let mut open: BTreeMap<u8, usize> = BTreeMap::new();

    fn take(
        events: &[ContactEvent],
        at_ns: u64,
        recognizer: &mut GestureRecognizer,
        replay: &mut Replay,
        open: &mut BTreeMap<u8, usize>,
    ) {
        replay.actions.extend(recognizer.handle(events, at_ns).into_iter().map(|action| (at_ns, action)));
        if !events.iter().any(|event| event.phase == ContactPhase::Began) {
            let moved = events.iter().filter(|event| event.phase == ContactPhase::Moved).count();
            replay.held_frames += moved.saturating_sub(1);
        }
        for event in events {
            match event.phase {
                ContactPhase::Began => {
                    assert!(!open.contains_key(&event.id), "contact {} began twice at {at_ns} ns", event.id);
                    open.insert(event.id, replay.contacts.len());
                    replay.contacts.push(Contact {
                        began_ns: at_ns,
                        landed_ns: event.at_ns,
                        began: (event.x, event.y),
                        last: (event.x, event.y),
                        last_ns: at_ns,
                        frames: 1,
                        ended_ns: 0,
                        ended: (event.x, event.y),
                        misread: false,
                    });
                }
                ContactPhase::Moved => {
                    let contact = &mut replay.contacts[*open
                        .get(&event.id)
                        .unwrap_or_else(|| panic!("contact {} moved before it began at {at_ns} ns", event.id))];
                    contact.last = (event.x, event.y);
                    contact.last_ns = at_ns;
                    contact.frames += 1;
                }
                ContactPhase::Ended => {
                    let index = open
                        .remove(&event.id)
                        .unwrap_or_else(|| panic!("contact {} ended without beginning at {at_ns} ns", event.id));
                    let contact = &mut replay.contacts[index];
                    contact.ended_ns = at_ns;
                    contact.ended = (event.x, event.y);
                    contact.misread = event.misread;
                }
            }
        }
    }

    let first_ns = recording.frames[0].0;
    for &(ns, ref bytes) in &recording.frames {
        let at_ns = ns - first_ns;
        while let Some(deadline) = stream.next_deadline_ns()
            && deadline <= at_ns
        {
            let due = stream.expire(deadline);
            take(&due, deadline, &mut recognizer, &mut replay, &mut open);
        }
        let frame = ReportParser::parse(bytes).expect("the recordings hold only decodable reports");
        let events = stream.report(frame, at_ns);
        take(&events, at_ns, &mut recognizer, &mut replay, &mut open);
    }
    // What the recording left open runs out after it.
    let end = recording.frames.last().expect("empty recording").0 - first_ns + 1_000_000_000;
    while let Some(deadline) = stream.next_deadline_ns()
        && deadline <= end
    {
        let due = stream.expire(deadline);
        take(&due, deadline, &mut recognizer, &mut replay, &mut open);
    }
    take(&[], end, &mut recognizer, &mut replay, &mut open);
    assert!(open.is_empty(), "{} contact(s) still open after the recording", open.len());
    replay
}

/// How a gesture opened and closed: its name, and whether this action is the one or the other.
fn opens_closes(action: &GestureAction) -> (&'static str, bool, bool) {
    match action {
        GestureAction::Tap { .. } => ("tap", false, false),
        GestureAction::RightClick { .. } => ("rightClick", false, false),
        GestureAction::DragBegan { .. } => ("drag", true, false),
        GestureAction::DragMoved { .. } => ("drag", false, false),
        GestureAction::DragEnded { .. } => ("drag", false, true),
        GestureAction::Scroll { phase, .. } => ("scroll", *phase == GesturePhase::Began, false),
        GestureAction::ScrollEnded { .. } => ("scroll", false, true),
        GestureAction::Magnify { phase, .. } => ("magnify", *phase == GesturePhase::Began, *phase == GesturePhase::Ended),
        GestureAction::Rotate { phase, .. } => ("rotate", *phase == GesturePhase::Began, *phase == GesturePhase::Ended),
        GestureAction::DockSwipe { phase, .. } => ("dockSwipe", *phase == GesturePhase::Began, *phase == GesturePhase::Ended),
        GestureAction::DockSwipeCancelled { .. } => ("dockSwipe", false, true),
        GestureAction::NotificationCenter => ("notificationCenter", false, false),
    }
}

/// Every gesture begins once, is updated inside itself and ends inside itself; taps and clicks
/// fall outside any gesture. Returns how often each began.
fn balanced<'a>(actions: impl IntoIterator<Item = &'a GestureAction>) -> BTreeMap<&'static str, usize> {
    let mut open: Option<&'static str> = None;
    let mut histogram: BTreeMap<&'static str, usize> = BTreeMap::new();
    for action in actions {
        let (name, begins, ends) = opens_closes(action);
        if begins {
            assert!(open.is_none(), "{name} began while {} was open", open.unwrap_or("nothing"));
            open = Some(name);
            *histogram.entry(name).or_insert(0) += 1;
        } else if ends {
            assert!(open == Some(name), "{name} ended while {} was open", open.unwrap_or("nothing"));
            open = None;
        } else if matches!(name, "tap" | "rightClick" | "notificationCenter") {
            assert!(open.is_none(), "{name} outside a gesture");
            *histogram.entry(name).or_insert(0) += 1;
        } else {
            assert!(open == Some(name), "{name} update outside its gesture");
        }
    }
    assert!(open.is_none(), "{} still open at the end", open.unwrap_or("nothing"));
    histogram
}

/// A time to a tenth of a millisecond: what the replays are stated in.
fn tenths(ns: u64) -> i64 {
    ((ns + 50_000) / 100_000) as i64
}

const M1_PROBE: &str = include_str!("fixtures/probe-session-1.jsonl");

#[test]
fn every_recorded_frame_parses() {
    let recording = Recording::new(M1_PROBE);
    assert_eq!(recording.len(), 3941);
    assert!(recording.frames.iter().all(|(_, bytes)| ReportParser::parse(bytes).is_some()));
}

#[test]
fn the_stream_keeps_a_consistent_lifecycle_on_real_data() {
    // Every contact's lifecycle is checked as it goes; this one counts them. The session had 40+
    // separate touches.
    let replay = replay(M1_PROBE, (221.9, 147.9), GestureConfig::default());
    assert!(replay.contacts.len() > 40, "{} contacts", replay.contacts.len());
}

#[test]
fn recognizer_balances_every_gesture_on_real_data() {
    let replay = replay(M1_PROBE, (221.9, 147.9), GestureConfig::default());
    let histogram = balanced(replay.actions.iter().map(|(_, action)| action));
    println!("recognized in probe-session-1: {histogram:?}");
}

// probe-multifinger-2026-11-24 holds 24 s of two- and three-finger gestures and two one-finger
// drags on the JN-MD-IPST105FP: 842 mouse reports. Five of its lift reports carry the
// coordinates (0, 0).

const MULTIFINGER_PROBE: &str =
    include_str!("fixtures/probe-multifinger-2026-11-24.jsonl");

#[test]
fn every_ended_event_sits_where_its_finger_last_was() {
    let replay = replay(MULTIFINGER_PROBE, (221.9, 147.9), GestureConfig::default());
    assert!(!replay.contacts.is_empty());
    for contact in &replay.contacts {
        assert_eq!(contact.last, contact.ended, "contact begun at {} ns ended away from its last frame", contact.began_ns);
    }
}

#[test]
fn no_tap_lands_at_the_panels_origin() {
    let replay = replay(MULTIFINGER_PROBE, (221.9, 147.9), GestureConfig::default());
    let at_origin = replay.actions.iter().any(|(_, action)| matches!(
        action,
        GestureAction::Tap { at, .. } if at.x == 0.0 && at.y == 0.0
    ));
    assert!(!at_origin, "the panel's origin is the Apple menu on a Mac");
}

// title-bar-2026-11-25 holds 31 s on the JN-MD-IPST105FP as its owner dragged a window by its
// title bar and tapped another window's: 658 frames with the tip down, 148 tip-off reports, and
// 87 frames more than 6 mm from the one before. Owner, 2026-11-25: "i dragged around this console
// window, when trying to tap the titlebar of the telegram app, it just goes to the background".
// Taken one frame at a time, as the build before the repair did, it made 44 contacts.

const TITLE_BAR: &str = include_str!("fixtures/title-bar-2026-11-25.jsonl");
const SECOND: u64 = 1_000_000_000;

/// A recording with the settings the owner's Mac had: one finger scrolls, no three-finger drag.
fn scrolling(source: &str) -> Replay {
    replay(
        source,
        (221.9, 147.9),
        GestureConfig {
            one_finger_drag: OneFingerDragMode::Scroll,
            three_finger_drag: false,
            panel_width_mm: 221.9,
            panel_height_mm: 147.9,
            ..GestureConfig::default()
        },
    )
}

/// The contacts that began from `from_ns` to `to_ns` into the recording.
fn touches(replay: &Replay, from_ns: u64, to_ns: u64) -> Vec<&Contact> {
    replay.contacts.iter().filter(|contact| contact.began_ns >= from_ns && contact.began_ns < to_ns).collect()
}

/// The recording as the stream saw it, one line a contact, for a failed assertion to show.
fn describe(replay: &Replay) -> String {
    replay
        .contacts
        .iter()
        .map(|contact| {
            format!(
                "{:7.3} s .. {:7.3} s  {:3} frames  from ({:5.1},{:5.1}) mm",
                contact.began_ns as f64 / 1e9,
                contact.ended_ns as f64 / 1e9,
                contact.frames,
                contact.began.0 * 221.9,
                contact.began.1 * 147.9
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_title_bar_taps_land_where_the_finger_was_and_nowhere_else() {
    // 11.8 s to 12.9 s: three taps on a title bar at (147, 42) mm. After the first lift the panel
    // reported three frames of a touch 107 mm to the left and one frame 9 mm on from there, both
    // over the desktop: taken as they came, each was a click of its own.
    let replay = title_bar();
    let taps = touches(&replay, 11 * SECOND + 700_000_000, 13 * SECOND);
    assert_eq!(taps.len(), 3, "{}", describe(&replay));
    for contact in &taps {
        let (x, y) = (contact.began.0 * 221.9, contact.began.1 * 147.9);
        assert!((x - 147.5).abs() < 6.0 && (y - 41.5).abs() < 6.0, "a touch began at ({x:.1}, {y:.1}) mm");
    }
    // Three clicks there, the second and third counted with the first.
    let mut clicks = Vec::new();
    for (_, action) in &replay.actions {
        if let GestureAction::Tap { at, count } = action
            && (at.x * 221.9 - 147.5).abs() < 6.0
            && (at.y * 147.9 - 41.5).abs() < 6.0
        {
            clicks.push(*count);
        }
    }
    assert_eq!(clicks, [1, 2, 3], "{}", describe(&replay));
    // Nothing else on that row to the left of the window was clicked.
    assert!(
        !replay.actions.iter().any(|(_, action)| matches!(
            action,
            GestureAction::Tap { at, .. } if at.x < 0.5 && (0.24..0.29).contains(&at.y)
        )),
        "{}",
        describe(&replay)
    );
}

#[test]
fn a_window_drag_is_one_contact_through_the_panels_flaps_and_misreads() {
    // 8.4 s to 11.6 s: a drag of three seconds along a title bar and down the window. In it the
    // panel reported the tip off seven times and the finger in six places 6 mm or more from the
    // last. The build before the repair passed those six on as the finger's, and the drag went on
    // inside the scroll of the touch before it.
    let replay = title_bar();
    let drag = touches(&replay, 8 * SECOND + 300_000_000, 11 * SECOND + 600_000_000);
    assert_eq!(drag.len(), 1, "{}", describe(&replay));
    assert!(drag[0].ended_ns > 11 * SECOND + 500_000_000, "{}", describe(&replay));
    assert!(drag[0].frames > 120, "{}", describe(&replay));
    // So one scroll, opened once and closed once while the drag ran.
    let during = replay
        .actions
        .iter()
        .filter(|(at_ns, _)| *at_ns >= 8 * SECOND + 450_000_000 && *at_ns <= 11 * SECOND + 700_000_000)
        .map(|(_, action)| action);
    assert_eq!(balanced(during)["scroll"], 1, "{}", describe(&replay));
}

fn title_bar() -> Replay {
    scrolling(TITLE_BAR)
}

// origin-reports-2026-11-26 holds 44 s on the JN-MD-IPST105FP as its owner tapped, scrolled and
// dragged a window by its title bar: 521 frames with the tip down and 129 tip-off reports, 35 of
// them from the panel's origin. Owner, 2026-11-26, on the build that ended a touch at the first
// report from the origin: "it is still hard to touch/grab, now the telegram window just goes full
// screen". That build let go of both drags at such a report (5.540 s and 43.445 s), and held a
// tap's lift report for as long as the panel went on reporting after it.

const ORIGIN_REPORTS: &str = include_str!("fixtures/origin-reports-2026-11-26.jsonl");

#[test]
fn a_slow_drag_survives_ten_lift_reports_from_the_panels_origin() {
    // 3.7 s to 7.1 s: a window dragged along its title bar and down. At 5.487 s the panel
    // reported the tip off, took the finger back 37 ms later, and at 5.540 s reported the tip
    // off from its origin ten times over 159 ms. The finger was down all along: it was back at
    // 5.751 s, 12 mm on, and went on dragging until 6.9 s.
    let replay = scrolling(ORIGIN_REPORTS);
    let drag = touches(&replay, 3 * SECOND + 700_000_000, 7 * SECOND + 300_000_000);
    assert_eq!(drag.len(), 1, "{}", describe(&replay));
    assert!(drag[0].ended_ns > 7 * SECOND, "{}", describe(&replay));
    assert!(drag[0].frames > 120, "{}", describe(&replay));
}

#[test]
fn a_fast_drag_is_let_go_where_its_finger_stopped_and_leaves_no_taps() {
    // 43.3 s to the end: a swipe to the left along a title bar, 38 mm in 125 ms. The panel then
    // reported its origin three times and eight other places, and the finger three times more,
    // the last 11 mm from the panel's left edge. Let go at the first report from the origin, the
    // drag lasted 54 ms and the finger's last two places were delivered as taps.
    let replay = scrolling(ORIGIN_REPORTS);
    let drag = touches(&replay, 43 * SECOND, u64::MAX);
    assert_eq!(drag.len(), 1, "{}", describe(&replay));
    assert!((drag[0].last.0 * 221.9 - 11.1).abs() < 3.0 && (drag[0].last.1 * 147.9 - 11.5).abs() < 3.0, "{}", describe(&replay));
    assert!(!replay.actions.iter().any(|(at_ns, action)| *at_ns >= 43 * SECOND && matches!(action, GestureAction::Tap { .. })), "{}", describe(&replay));
}

#[test]
fn a_tap_the_panel_keeps_talking_after_is_let_go_soon_and_is_no_long_press() {
    // 10.917 s: a tap of 233 ms at (123, 85) mm, after which the panel reported other places on
    // the finger's row for half a second and then the tip off. Held from its lift report to the
    // end of that, the touch had lasted 750 ms, and a long press opened a context menu. It is
    // let go within 300 ms of its last frame, and is no long press.
    //
    // Not a tap either. The panel's first sweep after it runs from 12 mm right of the finger
    // back to where it lifted, and ends in a lift report from there: the stream takes it for the
    // finger, and the touch is last seen 529 ms after it landed, 4.7 mm from where. A finger that
    // was that far from where it landed before the long-press time is a nudge (gestures.rs), so
    // the lift scrolls by those 4.7 mm and clicks nothing. The build of the recording tapped: it
    // counted the touch from its third frame, which made it 493 ms.
    let replay = scrolling(ORIGIN_REPORTS);
    let taps = touches(&replay, 10 * SECOND + 900_000_000, 11 * SECOND);
    assert_eq!(taps.len(), 1, "{}", describe(&replay));
    assert!(taps[0].ended_ns - taps[0].last_ns < 300_000_000, "{}", describe(&replay));
    let during: Vec<&GestureAction> = replay.actions.iter().filter(|(at_ns, _)| (10 * SECOND + 900_000_000..12 * SECOND).contains(at_ns)).map(|(_, action)| action).collect();
    assert!(
        during.iter().all(|action| !matches!(action, GestureAction::Tap { .. } | GestureAction::RightClick { .. })),
        "{}",
        describe(&replay)
    );
    // Its lift is one scroll, of 4.7 mm, opened and closed at once.
    let lift: Vec<&GestureAction> = replay.actions.iter().filter(|(at_ns, _)| (10 * SECOND + 900_000_000..11 * SECOND + 800_000_000).contains(at_ns)).map(|(_, action)| action).collect();
    assert_eq!(balanced(lift)["scroll"], 1, "{}", describe(&replay));
}

#[test]
fn the_taps_the_panel_chatters_after_are_held_no_longer_than_a_still_finger_comes_back() {
    // Two taps the panel went on reporting after, with places of its own: 8.791 s and 29.940 s.
    // The second is the one the owner meant for a title bar (it landed on a window's zoom button).
    // Both are let go within 200 ms of their last frame; held for the whole debounce and by the
    // panel's own places near the finger, the first took 460 ms.
    let replay = scrolling(ORIGIN_REPORTS);
    for (from_ms, x_mm) in [(8_700, 118.9), (29_900, 152.7)] {
        let taps = touches(&replay, from_ms * 1_000_000, from_ms * 1_000_000 + 200_000_000);
        assert_eq!(taps.len(), 1, "{from_ms} ms: {}", describe(&replay));
        assert!((taps[0].began.0 * 221.9 - x_mm).abs() < 1.5, "{from_ms} ms: {}", describe(&replay));
        assert!(taps[0].ended_ns - taps[0].last_ns < 200_000_000, "{from_ms} ms: {}", describe(&replay));
    }
}

#[test]
fn no_long_press_is_recognised_where_the_owner_made_none() {
    for (name, source) in [
        ("origin-reports-2026-11-26", ORIGIN_REPORTS),
        ("title-bar-2026-11-25", TITLE_BAR),
        ("nudges-2026-11-26", NUDGES),
        ("short-nudge-2026-11-26", SHORT_NUDGE),
    ] {
        let replay = scrolling(source);
        assert!(
            !replay.actions.iter().any(|(_, action)| matches!(action, GestureAction::RightClick { .. })),
            "{name}: {}",
            describe(&replay)
        );
    }
}

// nudges-2026-11-26 holds 34 s of the owner scrolling a terminal with small, slow nudges, on the
// build of the recording above: 492 mouse reports, 484 with the tip down, in seven touches. The
// finger landed, rested 70 to 350 ms, moved, rested again and lifted; three nudges began with 5.5
// to 9 mm in 70 to 90 ms. In that build the slop in which a finger counted as stationary grew
// with the time it was down, to 16.75 mm by half a second. The nudge at 25.720 s, 11.7 mm in all,
// never left it: nothing scrolled, and the lift at 913 ms was a right-click. The one at 31.557 s
// scrolled at 679 ms, once the finger had gone past 16 mm. Replayed, the slow drag at 24.211 s of
// title-bar-2026-11-25 ended the same way (a replay has no window drag areas).

const NUDGES: &str = include_str!("fixtures/nudges-2026-11-26.jsonl");

/// When the first scroll from `from_ns` on began.
fn scroll_begun(replay: &Replay, from_ns: u64) -> Option<u64> {
    replay
        .actions
        .iter()
        .find(|(at_ns, action)| *at_ns >= from_ns && matches!(action, GestureAction::Scroll { phase: GesturePhase::Began, .. }))
        .map(|(at_ns, _)| *at_ns)
}

#[test]
fn a_slow_nudge_scrolls_and_is_no_right_click() {
    // 25.720 s to 26.631 s: landed, 177 ms of rest, 5.5 mm in 70 ms, 6 mm more at 25 mm/s, 300 ms
    // of rest, lifted at 913 ms. It is a scroll from the moment it is 8 mm on, at 423 ms.
    let replay = scrolling(NUDGES);
    let nudge = touches(&replay, 25 * SECOND + 600_000_000, 26 * SECOND + 900_000_000);
    assert_eq!(nudge.len(), 1, "{}", describe(&replay));
    let begun = scroll_begun(&replay, nudge[0].began_ns).unwrap_or_else(|| panic!("the nudge did not scroll: {}", describe(&replay)));
    assert!(
        begun - nudge[0].began_ns < 500_000_000,
        "the scroll began {} ms into the nudge: {}",
        (begun - nudge[0].began_ns) / 1_000_000,
        describe(&replay)
    );
    assert!(!replay.actions.iter().any(|(_, action)| matches!(action, GestureAction::RightClick { .. })), "{}", describe(&replay));
}

#[test]
fn a_nudge_that_begins_small_scrolls_soon() {
    // 31.557 s to 32.467 s: landed, 175 ms of rest, 6.7 mm in 70 ms, 0.3 mm a frame for 350 ms,
    // then 10 mm at 50 mm/s. It is 8 mm on at 298 ms; it scrolled at 679 ms.
    let replay = scrolling(NUDGES);
    let nudge = touches(&replay, 31 * SECOND + 400_000_000, 32 * SECOND + 600_000_000);
    assert_eq!(nudge.len(), 1, "{}", describe(&replay));
    let begun = scroll_begun(&replay, nudge[0].began_ns).unwrap_or_else(|| panic!("the nudge did not scroll: {}", describe(&replay)));
    assert!(
        begun - nudge[0].began_ns < 350_000_000,
        "the scroll began {} ms into the nudge: {}",
        (begun - nudge[0].began_ns) / 1_000_000,
        describe(&replay)
    );
}

#[test]
fn the_nudges_are_six_scrolls_and_a_tap() {
    let replay = scrolling(NUDGES);
    assert_eq!(replay.contacts.len(), 7, "{}", describe(&replay));
    let begun = balanced(replay.actions.iter().map(|(_, action)| action));
    let of = |name: &str| begun.get(name).copied().unwrap_or(0);
    assert_eq!((of("scroll"), of("tap"), of("rightClick"), of("drag")), (6, 1, 0, 0), "{begun:?}");
}

// long-press-2026-11-26 holds 4.3 s of the owner's use at 20:54, after the panel had been away
// from 19:17 to 20:52: a touch the panel reported at its bottom-right corner for 405 ms, a tap at
// the left edge, and a finger held at the left edge for 3.6 s to right-click. In its last half
// second the finger wandered along the edge to 5.9 mm from where it had landed. The tap at the
// left edge (0.422 s to 0.507 s, six frames, 17 ms after the corner touch's lift report) is let
// go by the stream: that soon after a lift, a touch that far off waits for ten frames.

const LONG_PRESS: &str = include_str!("fixtures/long-press-2026-11-26.jsonl");

// short-nudge-2026-11-26 holds 7 s of the owner's use at 21:19, on the build of 21:11: a scroll up
// a terminal from its left edge, a tap, the nudge below, a longer drag, a tap at the left edge and
// a double-click at its bottom-left corner. The nudge's finger rested 354 ms, crept 0.7 mm, was
// 5 mm up at 526 ms and lifted 87 ms later: under the tap slop and past the long-press time, and
// on that build a right-click. After it the panel reported three frames of a touch 180 mm to the
// right, closed by a lift report (a click at (2311, 691) on that build), and after the drag six
// still frames 90 mm to the right of its last (a click at (1913, 724)).

const SHORT_NUDGE: &str = include_str!("fixtures/short-nudge-2026-11-26.jsonl");

#[test]
fn a_short_nudge_scrolls_on_lift() {
    let replay = scrolling(SHORT_NUDGE);
    let nudge = touches(&replay, SECOND + 400_000_000, SECOND + 500_000_000);
    assert_eq!(nudge.len(), 1, "{}", describe(&replay));
    assert!(nudge[0].ended_ns - nudge[0].began_ns >= 500_000_000, "{}", describe(&replay));
    let begun = scroll_begun(&replay, nudge[0].began_ns).unwrap_or_else(|| panic!("the nudge did not scroll: {}", describe(&replay)));
    assert!(
        begun.abs_diff(nudge[0].ended_ns) < 1_000_000,
        "the scroll began at {} s and the nudge ended at {} s: {}",
        begun as f64 / 1e9,
        nudge[0].ended_ns as f64 / 1e9,
        describe(&replay)
    );
    assert!(
        !replay.actions.iter().any(|(_, action)| matches!(action, GestureAction::RightClick { .. })),
        "{}",
        describe(&replay)
    );
    assert_eq!(replay.contacts.len(), 9, "{}", describe(&replay));
    let of = |name: &str| balanced(replay.actions.iter().map(|(_, action)| action)).get(name).copied().unwrap_or(0);
    assert_eq!((of("scroll"), of("tap"), of("rightClick"), of("drag")), (3, 6, 0, 0));
}

#[test]
fn a_finger_held_to_right_click_right_clicks_however_it_wanders_inside_the_slop() {
    let replay = scrolling(LONG_PRESS);
    let clicks: Vec<&GestureAction> =
        replay.actions.iter().filter(|(_, action)| matches!(action, GestureAction::RightClick { .. })).map(|(_, action)| action).collect();
    let [GestureAction::RightClick { at }] = clicks.as_slice() else {
        panic!("one right-click, got {clicks:?}: {}", describe(&replay));
    };
    // Where the finger was held: the panel's left edge, 119 mm down.
    assert!(
        at.x * 221.9 < 8.0 && (at.y * 147.9 - 119.4).abs() < 2.0,
        "right-click at ({:.1}, {:.1}) mm: {}",
        at.x * 221.9,
        at.y * 147.9,
        describe(&replay)
    );
}

// storm-2026-11-26 holds 29 s of the owner's use at 22:03 on 2026-11-26: taps, scrolls, a drag by
// a title bar and a double-click, and after nearly every lift the panel's storm. 1 056 reports,
// 887 with the tip down and 64 of the 169 tip-off ones from the panel's origin. On the build of
// the recording, which believed a still place on its fifth frame and a pair on its lift report,
// it made 37 contacts, 18 taps, 18 scrolls and the drag: ten of the taps and two of the scrolls
// were the storm's, and the drag it cut in two.
// Owner, on that build: "it is still hard to touch/grab, now the telegram window just goes full
// screen".

const STORM: &str = include_str!("fixtures/storm-2026-11-26.jsonl");

#[test]
fn nothing_the_panel_reports_in_the_storm_after_a_lift_is_a_touch() {
    // Where that build began contacts of the storm's: 3.861 s (eight still frames 118 mm from the
    // lift, a click at (1563, 688)), 7.030 s and 7.466 s (a double click at (1597, 795)),
    // 9.071 s (a click), 9.331 s (a scroll of 774 ms), 11.729 s and 11.974 s (clicks), 14.697 s
    // (a click), 18.359 s (a scroll), 19.133 s (a click) and 22.276 s (a click).
    let replay = scrolling(STORM);
    for (from_ms, to_ms) in [
        (3_600, 3_900),
        (6_970, 7_600),
        (9_000, 10_300),
        (11_780, 12_200),
        (14_600, 14_900),
        (18_260, 18_680),
        (19_210, 19_270),
        (22_390, 23_000),
    ] {
        let begun = touches(&replay, from_ms * 1_000_000, to_ms * 1_000_000);
        assert!(begun.is_empty(), "{from_ms} to {to_ms} ms: {}", describe(&replay));
    }
    assert_eq!(replay.contacts.len(), 25, "{}", describe(&replay));
}

#[test]
fn a_touch_the_panel_misread_before_a_landing_is_no_click_and_no_cut_in_the_landing() {
    // 2.007 s: three frames of a touch at (43, 21) mm and the tip off, then 15 ms after that
    // report the finger landing 60 mm away and scrolling to (124, 26) mm. The three frames were
    // a click at (1700, 404) just before the scroll.
    let replay = scrolling(STORM);
    let around = touches(&replay, 2 * SECOND, 3 * SECOND);
    let [misread, scroll, drag] = around.as_slice() else {
        panic!("three contacts from 2 s to 3 s: {}", describe(&replay));
    };
    assert!(misread.misread, "{}", describe(&replay));
    assert_eq!((tenths(misread.began_ns), tenths(misread.ended_ns)), (20_440, 24_270), "{}", describe(&replay));
    assert_eq!(tenths(scroll.began_ns), 24_270, "{}", describe(&replay));
    assert!(!scroll.misread && !drag.misread, "{}", describe(&replay));
    assert!(
        !replay.actions.iter().any(|(at_ns, action)| (2 * SECOND..3 * SECOND).contains(at_ns) && matches!(action, GestureAction::Tap { .. })),
        "{}",
        describe(&replay)
    );
    // The scroll that follows is one, from where the finger landed to where it lifted.
    let scroll_of = balanced(replay.actions.iter().filter(|(at_ns, _)| (24 * SECOND / 10..26 * SECOND / 10).contains(at_ns)).map(|(_, action)| action));
    assert_eq!(scroll_of["scroll"], 1, "{}", describe(&replay));

    // 2.759 s: a drag down the panel, landing 17 ms after a frame of the panel's and its lift
    // report. The panel's frames inside its first 125 ms cut it in two on that build, at 3.042 s.
    assert_eq!((tenths(drag.began_ns), tenths(drag.landed_ns), tenths(drag.ended_ns), drag.frames), (29_170, 27_590, 34_820, 27), "{}", describe(&replay));
    let drags = replay.actions.iter().filter(|(at_ns, _)| (29 * SECOND / 10..35 * SECOND / 10).contains(at_ns)).map(|(_, action)| action);
    assert_eq!(balanced(drags)["scroll"], 1, "{}", describe(&replay));
}

#[test]
fn a_flick_that_lands_amid_the_storm_is_one_scroll_from_its_first_frame() {
    // 3.889 s: a flick upward, landing 20 ms after a lift report of the panel's own. It is
    // believed on its third frame, once it has crossed its row, with all of them and dated from
    // the first. Before it, that build made a click of the still burst at (1563, 688).
    let replay = scrolling(STORM);
    let flick = touches(&replay, 39 * SECOND / 10, 40 * SECOND / 10);
    assert_eq!(flick.len(), 1, "{}", describe(&replay));
    assert_eq!((tenths(flick[0].began_ns), tenths(flick[0].landed_ns), tenths(flick[0].ended_ns), flick[0].frames), (39_220, 38_890, 41_690, 5), "{}", describe(&replay));
    let during = replay.actions.iter().filter(|(at_ns, _)| (39 * SECOND / 10..42 * SECOND / 10).contains(at_ns)).map(|(_, action)| action);
    let begun = balanced(during);
    assert_eq!((begun.get("scroll").copied().unwrap_or(0), begun.get("tap").copied().unwrap_or(0)), (1, 0), "{}", describe(&replay));
}

#[test]
fn the_storms_taps_are_gone_and_the_owners_are_whole() {
    // Seven taps and a double-click, where that build made eighteen taps: at (2030, 652),
    // (1933, 581), (2076, 572), (1837, 572), (1884, 616), (2150, 520) and twice at (1893, 491).
    let replay = scrolling(STORM);
    let taps: Vec<(i64, u32)> = replay
        .actions
        .iter()
        .filter_map(|(at_ns, action)| match action {
            GestureAction::Tap { count, .. } => Some((tenths(*at_ns), *count)),
            _ => None,
        })
        .collect();
    let expected = [
        (105_030, 1),
        (117_710, 1),
        (123_870, 1),
        (145_700, 1),
        (158_170, 1),
        (253_890, 1),
        (287_550, 1),
        (289_480, 2),
    ];
    assert_eq!(taps.len(), expected.len(), "{taps:?}: {}", describe(&replay));
    for ((at, count), (want_at, want_count)) in taps.iter().zip(&expected) {
        assert_eq!(*count, *want_count, "a tap at {at}: {}", describe(&replay));
        assert!((at - want_at).abs() <= 1, "a tap at {at}, wanted {want_at}: {}", describe(&replay));
    }
    let of = |name: &str| balanced(replay.actions.iter().map(|(_, action)| action)).get(name).copied().unwrap_or(0);
    assert_eq!((of("scroll"), of("tap"), of("rightClick"), of("drag")), (15, 8, 0, 1));
}

// garbled-drags-2026-11-27 holds 22 s of the owner's use at 00:21 on 2026-11-27, on the build of
// 21:11: eight drags down a terminal, nine taps and a double-click. 620 reports, 554 with the tip
// down. Around the drags the panel reported places of its own: after the first, four frames at
// (17.8, 86.3) mm closed by a lift report and a sweep of eight; before the second, two at the
// panel's left edge and one 12 mm from where the finger landed; after the fourth, eight still
// frames at another place. On that build they were 21 contacts, 12 taps and 9 scrolls.

const GARBLED: &str = include_str!("fixtures/garbled-drags-2026-11-27.jsonl");

#[test]
fn the_places_the_panel_reports_around_the_drags_are_no_touches() {
    // After the first drag (0.352 s to 2.934 s, its last 0.8 s garbled: places of the panel's and
    // lift reports between the finger's frames): on that build a click at (1568, 693) and a scroll
    // of 180 ms, and until 2026-11-27 the drag cut at 1.520 s and taken up again at 1.863 s.
    // After the fourth (7.481 s to 9.816 s): a click at (2128, 366).
    let replay = scrolling(GARBLED);
    for (from_ms, to_ms) in [(1_530, 2_020), (9_820, 10_170)] {
        let begun = touches(&replay, from_ms * 1_000_000, to_ms * 1_000_000);
        assert!(begun.is_empty(), "{from_ms} to {to_ms} ms: {}", describe(&replay));
    }
    assert_eq!(replay.contacts.len(), 17, "{}", describe(&replay));
}

#[test]
fn each_drag_is_one_contact_whole() {
    // How many frames each of the eight went out with.
    let replay = scrolling(GARBLED);
    let frames: Vec<usize> = replay.contacts.iter().filter(|contact| contact.frames >= 19).map(|contact| contact.frames).collect();
    assert_eq!(frames, [75, 45, 94, 46, 28, 31, 19], "{}", describe(&replay));
    assert_eq!(balanced(replay.actions.iter().map(|(_, action)| action))["scroll"], 7, "{}", describe(&replay));
}

#[test]
fn the_taps_of_the_garbled_drags_are_the_owners_and_the_double_click_is_counted() {
    let replay = scrolling(GARBLED);
    let taps: Vec<(i64, u32)> = replay
        .actions
        .iter()
        .filter_map(|(at_ns, action)| match action {
            GestureAction::Tap { count, .. } => Some((tenths(*at_ns), *count)),
            _ => None,
        })
        .collect();
    let expected = [
        (2_630, 1),
        (40_630, 1),
        (126_700, 1),
        (146_750, 1),
        (174_000, 1),
        (175_950, 2),
        (192_840, 1),
        (198_269, 1),
        (210_409, 1),
        (218_169, 1),
    ];
    assert_eq!(taps.len(), expected.len(), "{taps:?}: {}", describe(&replay));
    for ((at, count), (want_at, want_count)) in taps.iter().zip(&expected) {
        assert_eq!(*count, *want_count, "a tap at {at}: {}", describe(&replay));
        assert!((at - want_at).abs() <= 1, "a tap at {at}, wanted {want_at}: {}", describe(&replay));
    }
    let of = |name: &str| balanced(replay.actions.iter().map(|(_, action)| action)).get(name).copied().unwrap_or(0);
    assert_eq!((of("scroll"), of("tap"), of("rightClick"), of("drag")), (7, 10, 0, 0));
}

// drag-2026-11-27 holds 27 s of the owner's use at 16:25 on 2026-11-27, on the build of 11:24: a
// tap on the desktop, a tap on a window, a window dragged by its title bar for 7.4 s, two taps and
// thirteen scrolls. Owner: "tried, it is okay and the window moves as the finger, its just not
// under my finger when moving". In the drag and the scrolls the panel reported the tip off from
// under the finger eight times and went quiet for a report or two. Seven times the finger came
// back 13 to 22 mm on and to rest: held for five frames each time, the contact stood for 70 ms
// and jumped. The eighth (18.335 s) was a finger going 289 mm/s whose frames were beyond what
// the tracker would still take; they are dropped under either rule. Counted as this harness
// counts them, 32 frames of the recording went out with a later frame of their own; 19 do now.

const DRAG: &str = include_str!("fixtures/drag-2026-11-27.jsonl");

#[test]
fn the_window_drag_and_the_scrolls_go_on_through_the_panels_flaps() {
    let replay = scrolling(DRAG);
    let drag = touches(&replay, 5 * SECOND / 2, 13 * SECOND / 5);
    assert_eq!(drag.len(), 1, "{}", describe(&replay));
    assert_eq!((tenths(drag[0].began_ns), tenths(drag[0].landed_ns), tenths(drag[0].ended_ns), drag[0].frames), (25_150, 24_790, 99_360, 382), "{}", describe(&replay));
    assert_eq!(replay.contacts.len(), 18, "{}", describe(&replay));
    // What is left: at 7.208 s four frames, one of the panel's having come between the flap and
    // the finger's first frame 69 ms after its report, so that the chain waited for five; and
    // elsewhere the one or two frames a chain waits to be believed.
    assert_eq!(replay.held_frames, 19, "{}", describe(&replay));
    let taps: Vec<(i64, u32)> = replay
        .actions
        .iter()
        .filter_map(|(at_ns, action)| match action {
            GestureAction::Tap { count, .. } => Some((tenths(*at_ns), *count)),
            _ => None,
        })
        .collect();
    assert_eq!(taps, [(3_070, 1), (16_300, 1), (114_360, 1), (125_800, 1)], "{}", describe(&replay));
    let of = |name: &str| balanced(replay.actions.iter().map(|(_, action)| action)).get(name).copied().unwrap_or(0);
    assert_eq!((of("scroll"), of("tap"), of("rightClick"), of("drag")), (13, 4, 0, 0));
}
