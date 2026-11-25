//! Diagnostic for Direct delivery: posts events straight to the Event Sink (another TouchNext
//! instance) via DirectTarget, first while the sink is inactive (Finder frontmost), then while it
//! is active. Only ever targets our own Event Sink.

use std::time::Duration;

use objc2_core_foundation::{CGPoint, CFRetained};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGEvent, CGEventField, CGEventSource, CGEventSourceStateID,
    CGEventTapLocation, CGEventType, CGMouseButton, CGWarpMouseCursorPosition,
};

use touchcore::policy::WindowInfo;

use crate::apps;
use crate::cursor::CursorVisibility;
use crate::direct::DirectTarget;
use crate::event::{self, MomentumPhase, Phase, ScrollGesture};
use crate::hid_bridge::{HIDEventBridge, Phase as HIDPhase};
use crate::swift_text::swift_f64;
use crate::window::WindowLocator;

/// Runs the Direct self-test.
pub struct DirectSelfTest;

impl DirectSelfTest {
    /// Runs the self-test the command line asks for: the swipe experiments (`--swipe`, `--from N`),
    /// the button taps (`--button`), the activation timing (`--activation-timing`), or the default
    /// inactive/active rounds.
    pub fn run(log: &mut dyn FnMut(&str)) {
        // SAFETY: the call has no preconditions and never fails.
        let me = unsafe { libc::getpid() };
        let Some(sink) = WindowLocator::on_screen_windows().into_iter().find(|w| {
            w.owner_name == "TouchNext" && w.pid != me && w.layer == 0 && w.bounds.width >= 400.0
        }) else {
            log("no Event Sink window found; open it first (menu → Open Event Sink)");
            return;
        };
        log(&format!(
            "sink pid {} window {} bounds {}; CGEventSetWindowLocation available: {}",
            sink.pid,
            sink.number,
            swift_rect_struct(&sink.bounds),
            DirectTarget::is_available(),
        ));
        let target = DirectTarget {
            pid: sink.pid,
            window: sink.number,
            origin: CGPoint::new(sink.bounds.x, sink.bounds.y),
        };
        let source = CGEventSource::new(CGEventSourceStateID::Private)
            .expect("CGEventSource for the private state");

        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|arg| arg == "--swipe") {
            swipe_variants(&sink, &target, &source, log);
            return;
        }
        if args.iter().any(|arg| arg == "--button") {
            button_variants(&sink, &target, &source, log);
            return;
        }
        if args.iter().any(|arg| arg == "--activation-timing") {
            activation_timing(&sink, &target, &source, log);
            return;
        }

        if let Some(finder) = apps::with_bundle_id("com.apple.finder") {
            apps::activate(&finder);
        }
        std::thread::sleep(Duration::from_millis(800));
        log(&format!("--- sink inactive (frontmost: {}) ---", frontmost_name()));
        round(&sink, &target, &source, log, "inactive", 400.0);

        if let Some(sink_app) = apps::with_pid(sink.pid) {
            apps::activate(&sink_app);
        }
        std::thread::sleep(Duration::from_millis(800));
        log(&format!("--- sink active (frontmost: {}) ---", frontmost_name()));
        round(&sink, &target, &source, log, "active", 500.0);
        log("done");
    }
}

/// The frontmost app's name, "?" when the workspace cannot say.
fn frontmost_name() -> String {
    apps::frontmost()
        .and_then(|app| app.localizedName())
        .map(|name| name.to_string())
        .unwrap_or_else(|| "?".to_string())
}

/// Posts each event to the sink, logs the frontmost app, and rests half a second.
fn send(
    target: &DirectTarget,
    log: &mut dyn FnMut(&str),
    label: &str,
    events: Vec<Option<CFRetained<CGEvent>>>,
) {
    for event in events.into_iter().flatten() {
        target.post(&event);
    }
    log(&format!("posted {label}; frontmost: {}", frontmost_name()));
    std::thread::sleep(Duration::from_millis(500));
}

/// Click, right-click, scroll and pinch at four points across the sink's window.
fn round(
    sink: &WindowInfo,
    target: &DirectTarget,
    source: &CGEventSource,
    log: &mut dyn FnMut(&str),
    tag: &str,
    y: f64,
) {
    let point = |dx: f64| CGPoint::new(sink.bounds.x + dx, sink.bounds.y + y);
    let p1 = point(150.0);
    let p2 = point(300.0);
    let p3 = point(450.0);
    let p4 = point(600.0);
    send(
        target,
        log,
        &format!("{tag} click"),
        vec![
            event::mouse(CGEventType::LeftMouseDown, p1, CGMouseButton::Left, 1, 1, Some(sink.number), Some(source)),
            event::mouse(CGEventType::LeftMouseUp, p1, CGMouseButton::Left, 1, 1, Some(sink.number), Some(source)),
        ],
    );
    send(
        target,
        log,
        &format!("{tag} right-click"),
        vec![
            event::mouse(CGEventType::RightMouseDown, p2, CGMouseButton::Right, 1, 2, Some(sink.number), Some(source)),
            event::mouse(CGEventType::RightMouseUp, p2, CGMouseButton::Right, 1, 2, Some(sink.number), Some(source)),
        ],
    );
    send(
        target,
        log,
        &format!("{tag} scroll"),
        vec![
            event::scroll(0.0, 12.0, p3, Some(Phase::Began), MomentumPhase::None, Some(sink.number), Some(source)),
            event::scroll(0.0, 0.0, p3, Some(Phase::Ended), MomentumPhase::None, Some(sink.number), Some(source)),
        ],
    );
    send(
        target,
        log,
        &format!("{tag} pinch"),
        vec![
            event::magnify(0.1, p4, Phase::Began, Some(sink.number), Some(source)),
            event::magnify(0.0, p4, Phase::Ended, Some(sink.number), Some(source)),
        ],
    );
}

/// From a background process: make Finder frontmost, activate the sink, wait N ms, click.
fn activation_timing(
    sink: &WindowInfo,
    target: &DirectTarget,
    source: &CGEventSource,
    log: &mut dyn FnMut(&str),
) {
    let finder = apps::with_bundle_id("com.apple.finder");
    let sink_app = apps::with_pid(sink.pid);
    for (i, delay) in [0.0f64, 0.02, 0.05, 0.15].into_iter().enumerate() {
        if let Some(finder) = &finder {
            apps::activate(finder);
        }
        std::thread::sleep(Duration::from_millis(800));
        let accepted = sink_app.as_ref().is_some_and(|app| apps::activate(app));
        std::thread::sleep(Duration::from_secs_f64(delay));
        let p = CGPoint::new(sink.bounds.x + 120.0 + i as f64 * 150.0, sink.bounds.y + 450.0);
        for event_type in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
            if let Some(e) =
                event::mouse(event_type, p, CGMouseButton::Left, 1, 10 + i as i64, Some(sink.number), Some(source))
            {
                target.post(&e);
            }
        }
        log(&format!(
            "T{}: activate() returned {}, waited {} ms, clicked at x+{}",
            i + 1,
            accepted,
            (delay * 1000.0) as i64,
            120 + i * 150,
        ));
        std::thread::sleep(Duration::from_millis(500));
    }
    log("done");
}

/// Taps the sink's NSButton (window-local centre 76,60) with one change per variant; the sink logs
/// "BUTTON action fired" when AppKit's control tracking accepts the press.
fn button_variants(
    sink: &WindowInfo,
    target: &DirectTarget,
    source: &CGEventSource,
    log: &mut dyn FnMut(&str),
) {
    if let Some(app) = apps::with_pid(sink.pid) {
        apps::activate(&app);
    }
    std::thread::sleep(Duration::from_millis(600));
    let p = CGPoint::new(sink.bounds.x + 76.0, sink.bounds.y + 60.0);
    tap(sink, target, source, log, p, "B1 Direct, up immediately", 0, &[], false);
    tap(sink, target, source, log, p, "B2 Direct, up after 80 ms", 80, &[], false);
    tap(
        sink,
        target,
        source,
        log,
        p,
        "B3 Direct + field 106 (window height)",
        0,
        &[(106, sink.bounds.height as i64)],
        false,
    );
    // The guard: only move the cursor if the sink really is topmost at the button.
    let top = WindowLocator::on_screen_windows().into_iter().find(|w| {
        w.alpha > 0.0
            && p.x >= w.bounds.x
            && p.x < w.bounds.x + w.bounds.width
            && p.y >= w.bounds.y
            && p.y < w.bounds.y + w.bounds.height
    });
    if top.as_ref().map(|w| w.number) == Some(sink.number) {
        tap(
            sink,
            target,
            source,
            log,
            p,
            "B4 Direct with the real cursor over the button",
            0,
            &[],
            true,
        );
    } else {
        log(&format!(
            "B4 skipped: topmost window at the button is {}",
            top.map(|w| w.owner_name)
                .unwrap_or_else(|| "none".to_string()),
        ));
    }
    log("done");
}

/// One button tap: down, optional hold, up. `cursor_over` moves the real cursor there and back.
#[allow(clippy::too_many_arguments)]
fn tap(
    sink: &WindowInfo,
    target: &DirectTarget,
    source: &CGEventSource,
    log: &mut dyn FnMut(&str),
    p: CGPoint,
    label: &str,
    up_delay_ms: u64,
    extra: &[(u32, i64)],
    cursor_over: bool,
) {
    let saved = CGEvent::new(None)
        .map(|event| CGEvent::location(Some(&event)))
        .unwrap_or(CGPoint::new(0.0, 0.0));
    if cursor_over {
        CGWarpMouseCursorPosition(p);
        std::thread::sleep(Duration::from_millis(50));
    }
    for (i, event_type) in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp]
        .into_iter()
        .enumerate()
    {
        if i == 1 && up_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(up_delay_ms));
        }
        let Some(e) = event::mouse(event_type, p, CGMouseButton::Left, 1, 40, Some(sink.number), Some(source))
        else {
            continue;
        };
        for (field, value) in extra {
            CGEvent::set_integer_value_field(Some(&e), CGEventField(*field), *value);
        }
        target.post(&e);
    }
    if cursor_over {
        std::thread::sleep(Duration::from_millis(100));
        CGWarpMouseCursorPosition(saved);
        CGAssociateMouseAndMouseCursorPosition(true);
    }
    log(&format!("posted {label}"));
    std::thread::sleep(Duration::from_millis(700));
}

/// What one swipe variant adds to every scroll event: the event, and the event's `dx`.
type Step = fn(&CGEvent, f64);

/// Writes an integer field by its private number.
fn set_field(e: &CGEvent, number: u32, value: i64) {
    CGEvent::set_integer_value_field(Some(e), CGEventField(number), value);
}

/// Writes a double field by its private number.
fn set_field_double(e: &CGEvent, number: u32, value: f64) {
    CGEvent::set_double_value_field(Some(e), CGEventField(number), value);
}

/// A field's value as `Double(Float(bitPattern:))`, the raw-bits form the HID fields carry.
fn float_bits(bits: u32) -> f64 {
    f32::from_bits(bits) as f64
}

/// The sender id of the built-in trackpad, which the HID attachments name.
const TRACKPAD_SENDER_ID: u64 = 4_294_970_419;

/// The accelerated and raw horizontal deltas of the trackpad's changed events, scaled down.
fn scaled_deltas(e: &CGEvent, dx: f64) {
    set_field_double(e, 175, -dx * 0.0222);
    set_field(e, 177, -dx.round() as i64);
}

/// Field 137, the direction flag.
fn direction_flag(e: &CGEvent, _dx: f64) {
    set_field(e, 137, 1);
}

/// Realistic deltas from the trackpad's changed events: 175 (accelerated) ≈ -dx/10, 177 (raw) ≈
/// -1.4·dx, as doubles (an integer write to 177 was silently dropped).
fn realistic_deltas(e: &CGEvent, dx: f64) {
    set_field_double(e, 175, -dx / 10.0);
    set_field_double(e, 177, -(dx * 1.4).round());
}

/// The sender id of the built-in trackpad, in field 87.
fn trackpad_sender(e: &CGEvent, _dx: f64) {
    set_field(e, 87, TRACKPAD_SENDER_ID as i64);
}

/// Field 126 holds the underlying HID event type as raw bits: 6 is kIOHIDEventTypeScroll on the
/// trackpad.
fn hid_scroll_type(e: &CGEvent, _dx: f64) {
    set_field_double(e, 126, float_bits(6));
}

/// Field 169, the HID timestamp.
fn hid_timestamp(e: &CGEvent, _dx: f64) {
    set_field(e, 169, touchhid::clock::now_ticks() as i64);
}

/// Field 101, the event source state the hardware's events carry (60).
fn source_state(e: &CGEvent, _dx: f64) {
    set_field(e, 101, 60);
}

/// Fields 41 to 44 (the poster's own fields) zeroed, and 45 set: as the hardware sends them.
fn no_poster_fields(e: &CGEvent, _dx: f64) {
    for number in [41, 42, 43, 44] {
        set_field(e, number, 0);
    }
    set_field(e, 45, 1);
}

/// What a variant attaches to each scroll event beyond its fields.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hid {
    /// No attached HID event.
    None,
    /// A plain HID scroll event, as real trackpad events carry.
    Scroll,
    /// What the built-in trackpad attaches: a built-in scroll with a child carrying the finger's
    /// travel, plus the multitouch driver's vendor event when `vendor`.
    Trackpad { vendor: bool },
}

/// One swipe variant: what it writes into each event, what it attaches, and how it is delivered.
#[derive(Clone, Copy)]
struct Variant {
    label: &'static str,
    steps: &'static [Step],
    /// The gesture phase goes into fields 119/139 as raw bits. `CGEventCreateScrollWheelEvent2`
    /// leaves them at 1 for every event; trackpad events carry the phase there (1 began, 2 changed,
    /// 4 ended).
    gesture_phase: bool,
    /// The trackpad's scroll-gesture stream (type 29: subtype 61, 6 per move, 62) goes out next to
    /// the scrolls.
    gesture_stream: bool,
    hid: Hid,
    /// Routed through the window server, to the window under the hidden cursor, instead of posted
    /// to the app.
    routed: bool,
}

/// The fields of S11: what a trackpad swipe's scroll event carries.
const S11: &[Step] = &[direction_flag, realistic_deltas, hid_scroll_type, trackpad_sender];

/// Every variant, in the order the test posts them; each adds one change to the one before.
const VARIANTS: &[Variant] = &[
    Variant { label: "S1 baseline", steps: &[], gesture_phase: false, gesture_stream: false, hid: Hid::None, routed: false },
    Variant { label: "S2 + accelerated/raw horizontal deltas (175, 177)", steps: &[scaled_deltas], gesture_phase: false, gesture_stream: false, hid: Hid::None, routed: false },
    Variant { label: "S3 + field 137 = 1", steps: &[scaled_deltas, direction_flag], gesture_phase: false, gesture_stream: false, hid: Hid::None, routed: false },
    Variant {
        label: "S4 + sender id 87 (the trackpad's)",
        steps: &[scaled_deltas, direction_flag, trackpad_sender],
        gesture_phase: false,
        gesture_stream: false,
        hid: Hid::None,
        routed: false,
    },
    Variant {
        label: "S5 + 126 = HID type scroll (6)",
        steps: &[scaled_deltas, direction_flag, trackpad_sender, hid_scroll_type],
        gesture_phase: false,
        gesture_stream: false,
        hid: Hid::None,
        routed: false,
    },
    Variant {
        label: "S6 + 169 = HID timestamp",
        steps: &[scaled_deltas, direction_flag, trackpad_sender, hid_scroll_type, hid_timestamp],
        gesture_phase: false,
        gesture_stream: false,
        hid: Hid::None,
        routed: false,
    },
    Variant {
        label: "S7 + 101 = 60",
        steps: &[scaled_deltas, direction_flag, trackpad_sender, hid_scroll_type, hid_timestamp, source_state],
        gesture_phase: false,
        gesture_stream: false,
        hid: Hid::None,
        routed: false,
    },
    Variant { label: "S8 baseline + gesture phase in 119/139", steps: &[], gesture_phase: true, gesture_stream: false, hid: Hid::None, routed: false },
    Variant { label: "S9 S8 + direction flag 137", steps: &[direction_flag], gesture_phase: true, gesture_stream: false, hid: Hid::None, routed: false },
    Variant {
        label: "S10 S9 + realistic 175/177 as doubles",
        steps: &[direction_flag, realistic_deltas],
        gesture_phase: true,
        gesture_stream: false,
        hid: Hid::None,
        routed: false,
    },
    Variant { label: "S11 S10 + HID type 6 + sender id", steps: S11, gesture_phase: true, gesture_stream: false, hid: Hid::None, routed: false },
    Variant {
        label: "S12 S11 + attached HID scroll event",
        steps: S11,
        gesture_phase: true,
        gesture_stream: false,
        hid: Hid::Scroll,
        routed: false,
    },
    Variant {
        label: "S13 S11 + trackpad-shaped HID scroll with child",
        steps: S11,
        gesture_phase: true,
        gesture_stream: false,
        hid: Hid::Trackpad { vendor: false },
        routed: false,
    },
    Variant {
        label: "S14 S13 + multitouch vendor event",
        steps: S11,
        gesture_phase: true,
        gesture_stream: false,
        hid: Hid::Trackpad { vendor: true },
        routed: false,
    },
    Variant {
        label: "S15 S14 + HID-system source, no poster fields",
        steps: &[direction_flag, realistic_deltas, hid_scroll_type, trackpad_sender, source_state, hid_timestamp, no_poster_fields],
        gesture_phase: true,
        gesture_stream: false,
        hid: Hid::Trackpad { vendor: true },
        routed: false,
    },
    Variant {
        label: "S16 S15 routed through the window server",
        steps: &[direction_flag, realistic_deltas, hid_scroll_type, trackpad_sender, source_state, hid_timestamp, no_poster_fields],
        gesture_phase: true,
        gesture_stream: false,
        hid: Hid::Trackpad { vendor: true },
        routed: true,
    },
    Variant { label: "S17 S8 + the trackpad's scroll-gesture stream", steps: &[], gesture_phase: true, gesture_stream: true, hid: Hid::None, routed: false },
];

/// The gesture-phase bits fields 119/139 carry, 0 for a momentum event.
fn phase_bits(phase: Option<Phase>) -> u32 {
    match phase {
        Some(Phase::Began) => 1,
        Some(Phase::Changed) => 2,
        Some(Phase::Ended) => 4,
        None => 0,
    }
}

/// The scroll phase as the HID bridge's phase type.
fn hid_phase(phase: Phase) -> HIDPhase {
    match phase {
        Phase::Began => HIDPhase::Began,
        Phase::Changed => HIDPhase::Changed,
        Phase::Ended => HIDPhase::Ended,
    }
}

/// The scroll-gesture stream's events for one scroll event.
fn stream_parts(phase: Phase, dx: f64) -> Vec<ScrollGesture> {
    match phase {
        Phase::Began => vec![ScrollGesture::Begin],
        Phase::Changed => vec![ScrollGesture::Move { dx, dy: 0.0, phase: Phase::Changed }],
        Phase::Ended => vec![
            ScrollGesture::Move { dx: 0.0, dy: 0.0, phase: Phase::Ended },
            ScrollGesture::End,
        ],
    }
}

/// Horizontal two-finger swipes posted to the sink with progressively more of the fields a real
/// trackpad swipe carries, to find what macOS's swipe tracking (Safari back/forward) needs. The
/// sink logs "SWIPE tracking … complete=y amount=1.00" when a swipe completes.
fn swipe_variants(sink: &WindowInfo, target: &DirectTarget, source: &CGEventSource, log: &mut dyn FnMut(&str)) {
    if let Some(app) = apps::with_pid(sink.pid) {
        apps::activate(&app);
    }
    std::thread::sleep(Duration::from_millis(600));
    let p = CGPoint::new(sink.bounds.x + 450.0, sink.bounds.y + 300.0);
    let args: Vec<String> = std::env::args().collect();
    let from = args
        .iter()
        .position(|arg| arg == "--from")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1);
    for Variant { label, steps, gesture_phase, gesture_stream, hid, routed } in VARIANTS.iter().copied().skip(from.saturating_sub(1)) {
        let saved = CGEvent::new(None).map(|event| CGEvent::location(Some(&event)));
        if routed {
            CursorVisibility::allow_hiding_in_background();
            CursorVisibility::hide();
            CGWarpMouseCursorPosition(p);
        }
        let mut changed_count = 0usize;
        if hid != Hid::None {
            log(&format!("HID bridge available: {}", HIDEventBridge::is_available()));
        }
        log(&format!("posting {label}"));
        let mut post = |dx: f64, phase: Option<Phase>, momentum: MomentumPhase| {
            let Some(e) = event::scroll(dx, 0.0, p, phase, momentum, Some(sink.number), Some(source)) else {
                return;
            };
            for step in steps {
                step(&e, dx);
            }
            if gesture_phase {
                let value = float_bits(phase_bits(phase));
                set_field_double(&e, 119, value);
                set_field_double(&e, 139, value);
            }
            if let Some(hid_phase) = phase.map(hid_phase) {
                match hid {
                    Hid::None => {}
                    Hid::Scroll => {
                        HIDEventBridge::attach_scroll(&e, -(dx * 1.4).round(), 0.0, hid_phase, Some(TRACKPAD_SENDER_ID));
                    }
                    Hid::Trackpad { vendor } => {
                        if phase == Some(Phase::Changed) {
                            changed_count += 1;
                        }
                        HIDEventBridge::attach_trackpad_scroll(&e, dx, 0.0, hid_phase, TRACKPAD_SENDER_ID, vendor, changed_count > 8);
                    }
                }
            }
            if routed {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            } else {
                target.post(&e);
            }
            if gesture_stream && let Some(phase) = phase {
                for part in stream_parts(phase, dx) {
                    if let Some(gesture) = event::scroll_gesture(part, p, Some(sink.number), Some(source)) {
                        target.post(&gesture);
                    }
                }
            }
        };
        post(2.0, Some(Phase::Began), MomentumPhase::None);
        for _ in 0..14 {
            std::thread::sleep(Duration::from_millis(16));
            post(18.0, Some(Phase::Changed), MomentumPhase::None);
        }
        std::thread::sleep(Duration::from_millis(16));
        post(0.0, Some(Phase::Ended), MomentumPhase::None);
        for i in 0..10 {
            std::thread::sleep(Duration::from_millis(16));
            post(40.0 - i as f64 * 4.0, None, if i == 0 { MomentumPhase::Begin } else { MomentumPhase::Continue });
        }
        post(0.0, None, MomentumPhase::End);
        // A routed variant put the hidden cursor over the sink; put it back.
        if routed && let Some(saved) = saved {
            CGWarpMouseCursorPosition(saved);
            CGAssociateMouseAndMouseCursorPosition(true);
            CursorVisibility::show();
        }
        std::thread::sleep(Duration::from_millis(1_200));
    }
    log("done");
}

/// A rect the way Swift prints a struct of this shape: `ScreenRect(x: x, y: y, width: w, height: h)`.
fn swift_rect_struct(rect: &touchcore::mapper::ScreenRect) -> String {
    format!(
        "ScreenRect(x: {}, y: {}, width: {}, height: {})",
        swift_f64(rect.x),
        swift_f64(rect.y),
        swift_f64(rect.width),
        swift_f64(rect.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swift_rect_struct_prints_every_field() {
        let rect = touchcore::mapper::ScreenRect {
            x: 0.0,
            y: 25.0,
            width: 900.0,
            height: 600.0,
        };
        assert_eq!(
            swift_rect_struct(&rect),
            "ScreenRect(x: 0.0, y: 25.0, width: 900.0, height: 600.0)"
        );
    }
}
