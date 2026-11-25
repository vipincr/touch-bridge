//! Builds the synthetic CGEvents the driver posts.
//!
//! Field numbers are from CGEventTypes.h unless marked private; the private ones (the gesture
//! fields, the scroll phase) are the numbers hardware and Mac Mouse Fix's TouchSimulator.m use.
//! Every function returns `Option<CFRetained<CGEvent>>`: Core Graphics refuses to build an event
//! without a live source, and the caller posts nothing then, as in the Swift original.

use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventType,
    CGMouseButton, CGScrollEventUnit,
};

/// Written to kCGEventSourceUserData (field 42) so the driver's own events can be recognised ("TBGR").
pub const TAG: i64 = 0x5442_4752;

// `MouseEventWindowUnderMousePointer` and `MouseEventWindowUnderMousePointerThatCanHandleThisEvent`
// (fields 91 and 92) exist as named constants in the bindings and are used through those.
/// The scroll phase bitmask: 1 began, 2 changed, 4 ended.
const SCROLL_PHASE: CGEventField = CGEventField(99);
/// The scroll momentum phase: 0 none, 1 begin, 2 continue, 3 end.
const MOMENTUM_PHASE: CGEventField = CGEventField(123);
// Private gesture fields (as used by Mac Mouse Fix's TouchSimulator.m).
const EVENT_TYPE: CGEventField = CGEventField(55);
const GESTURE_HID_TYPE: CGEventField = CGEventField(110);
const GESTURE_ZOOM_VALUE: CGEventField = CGEventField(113);
const GESTURE_ROTATION_VALUE: CGEventField = CGEventField(114);
const GESTURE_PHASE: CGEventField = CGEventField(132);
const NS_EVENT_TYPE_GESTURE: i64 = 29;
const HID_TYPE_ZOOM: i64 = 8;
const HID_TYPE_ROTATION: i64 = 5;

/// A scroll, pinch or rotate gesture's finger phase, as the phase fields carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Began,
    Changed,
    Ended,
}

/// The scroll momentum phase a real trackpad plays after the finger lifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MomentumPhase {
    None = 0,
    Begin = 1,
    Continue = 2,
    End = 3,
}

/// The gesture stream a real trackpad sends alongside a two-finger scroll (Event Sink tap,
/// 2026-10-11). AppKit's swipe tracking (Safari back/forward) measures progress from it, not from
/// the scroll events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollGesture {
    /// Subtype 61 after the scroll's `began` event.
    Begin,
    /// Subtype 6 with the finger movement (natural direction, as the scroll's pixel delta), per
    /// scroll event.
    Move { dx: f64, dy: f64, phase: Phase },
    /// Subtype 62 after the scroll's `ended` event (preceded by a [`ScrollGesture::Move`] with
    /// phase [`Phase::Ended`]).
    End,
}

/// Builds a mouse click, drag or move. `click_state` is the click count (1 for a plain press),
/// `window` the target window of a Direct delivery.
pub fn mouse(
    event_type: CGEventType,
    at: CGPoint,
    button: CGMouseButton,
    click_state: i64,
    event_number: i64,
    window: Option<i64>,
    source: Option<&CGEventSource>,
) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new_mouse_event(source, event_type, at, button)?;
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventClickState, click_state);
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventNumber, event_number);
    finish(&event, window);
    Some(event)
}

/// Builds a two-wheel scroll. `dx`/`dy` in points; positive dy scrolls content down (as when a
/// finger drags the page down).
pub fn scroll(
    dx: f64,
    dy: f64,
    at: CGPoint,
    phase: Option<Phase>,
    momentum: MomentumPhase,
    window: Option<i64>,
    source: Option<&CGEventSource>,
) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new_scroll_wheel_event2(
        source,
        CGScrollEventUnit::Pixel,
        2,
        dy.round() as i32,
        dx.round() as i32,
        0,
    )?;
    CGEvent::set_location(Some(&event), at);
    CGEvent::set_integer_value_field(Some(&event), CGEventField::ScrollWheelEventIsContinuous, 1);
    CGEvent::set_double_value_field(Some(&event), CGEventField::ScrollWheelEventPointDeltaAxis1, dy);
    CGEvent::set_double_value_field(Some(&event), CGEventField::ScrollWheelEventPointDeltaAxis2, dx);
    CGEvent::set_double_value_field(Some(&event), CGEventField::ScrollWheelEventFixedPtDeltaAxis1, dy);
    CGEvent::set_double_value_field(Some(&event), CGEventField::ScrollWheelEventFixedPtDeltaAxis2, dx);
    CGEvent::set_integer_value_field(Some(&event), SCROLL_PHASE, phase.map(phase_mask).unwrap_or(0));
    CGEvent::set_integer_value_field(Some(&event), MOMENTUM_PHASE, momentum as i64);
    finish(&event, window);
    Some(event)
}

/// Builds a pinch (zoom) gesture event. `delta` is the magnification.
pub fn magnify(
    delta: f64,
    at: CGPoint,
    phase: Phase,
    window: Option<i64>,
    source: Option<&CGEventSource>,
) -> Option<CFRetained<CGEvent>> {
    gesture(HID_TYPE_ZOOM, GESTURE_ZOOM_VALUE, delta, at, phase, window, source)
}

/// Builds a rotation gesture event. `degrees`, positive = counterclockwise.
pub fn rotate(
    degrees: f64,
    at: CGPoint,
    phase: Phase,
    window: Option<i64>,
    source: Option<&CGEventSource>,
) -> Option<CFRetained<CGEvent>> {
    gesture(HID_TYPE_ROTATION, GESTURE_ROTATION_VALUE, degrees, at, phase, window, source)
}

/// Builds one private gesture event (type 29) carrying `value` in `value_field`.
fn gesture(
    hid_type: i64,
    value_field: CGEventField,
    value: f64,
    at: CGPoint,
    phase: Phase,
    window: Option<i64>,
    source: Option<&CGEventSource>,
) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new(source)?;
    CGEvent::set_integer_value_field(Some(&event), EVENT_TYPE, NS_EVENT_TYPE_GESTURE);
    CGEvent::set_location(Some(&event), at);
    CGEvent::set_integer_value_field(Some(&event), GESTURE_HID_TYPE, hid_type);
    CGEvent::set_double_value_field(Some(&event), value_field, value);
    CGEvent::set_integer_value_field(Some(&event), GESTURE_PHASE, phase_mask(phase));
    finish(&event, window);
    Some(event)
}

/// Builds one event of the scroll gesture stream. Fields 113/114/116/118 carry the movement as
/// doubles, 115/117/164 the same as float bit patterns, and the subtypes 61 (begin), 6 (move) and
/// 62 (end) go in field 110; a move sets the phase in field 132.
pub fn scroll_gesture(
    part: ScrollGesture,
    at: CGPoint,
    window: Option<i64>,
    source: Option<&CGEventSource>,
) -> Option<CFRetained<CGEvent>> {
    let event = CGEvent::new(source)?;
    CGEvent::set_integer_value_field(Some(&event), EVENT_TYPE, NS_EVENT_TYPE_GESTURE);
    CGEvent::set_location(Some(&event), at);
    match part {
        ScrollGesture::Begin | ScrollGesture::End => {
            let subtype = if part == ScrollGesture::Begin { 61 } else { 62 };
            CGEvent::set_integer_value_field(Some(&event), GESTURE_HID_TYPE, subtype);
            for field in [113, 114, 116, 118] {
                CGEvent::set_double_value_field(Some(&event), CGEventField(field), f32::from_bits(6) as f64);
            }
            for field in [115, 117, 164] {
                CGEvent::set_integer_value_field(Some(&event), CGEventField(field), 6); // the gesture is a scroll (6)
            }
        }
        ScrollGesture::Move { dx, dy, phase } => {
            CGEvent::set_integer_value_field(Some(&event), GESTURE_HID_TYPE, 6);
            for field in [113, 114, 116, 118] {
                CGEvent::set_double_value_field(Some(&event), CGEventField(field), dx);
            }
            for field in [115, 117, 164] {
                CGEvent::set_integer_value_field(Some(&event), CGEventField(field), float_bits(dx));
            }
            for field in [119, 139] {
                CGEvent::set_double_value_field(Some(&event), CGEventField(field), dy);
            }
            for field in [123, 165] {
                CGEvent::set_integer_value_field(Some(&event), CGEventField(field), float_bits(dy));
            }
            CGEvent::set_double_value_field(Some(&event), CGEventField(124), f32::from_bits(1) as f64);
            CGEvent::set_integer_value_field(Some(&event), CGEventField(135), 1);
            CGEvent::set_integer_value_field(Some(&event), GESTURE_PHASE, phase_mask(phase));
        }
    }
    finish(&event, window);
    Some(event)
}

/// A phase as the bitmask the scroll and gesture phase fields carry.
fn phase_mask(phase: Phase) -> i64 {
    match phase {
        Phase::Began => 1,
        Phase::Changed => 2,
        Phase::Ended => 4,
    }
}

/// `Float(v)`'s raw 32 bits, as the private gesture fields carry the movements.
fn float_bits(value: f64) -> i64 {
    (value as f32).to_bits() as i64
}

/// Stamps the event as the driver's (the tag), copies the current modifier keys onto it, and, for
/// Direct delivery, names the target window in the two fields the window server fills when it
/// routes an event.
fn finish(event: &CGEvent, window: Option<i64>) {
    CGEvent::set_integer_value_field(Some(event), CGEventField::EventSourceUserData, TAG);
    let pass_through = CGEventFlags::MaskShift
        | CGEventFlags::MaskControl
        | CGEventFlags::MaskAlternate
        | CGEventFlags::MaskCommand;
    CGEvent::set_flags(
        Some(event),
        CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState) & pass_through,
    );
    if let Some(window) = window {
        CGEvent::set_integer_value_field(
            Some(event),
            CGEventField::MouseEventWindowUnderMousePointer,
            window,
        );
        CGEvent::set_integer_value_field(
            Some(event),
            CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            window,
        );
    }
}
