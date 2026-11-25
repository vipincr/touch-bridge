//! The trackpad's animated "dock swipes" (Mission Control, App Exposé, Spaces, Launchpad, Show
//! Desktop), synthesized as the private gesture events macOS 15 expects. The event format was
//! learned from Mac Mouse Fix's TouchSimulator (pre-macOS 27 path): per step, a type-30 event
//! carrying a kIOHIDEventTypeDockSwipe subtype with the phase, swipe type and accumulated progress,
//! then a type-29 companion, both to the session tap.
//!
//! The end pair is re-sent after 0.2 s and 0.5 s because the system sometimes misses it under load.
//! Swift scheduled those re-sends with `DispatchWorkItem`s; here the swipe is a single-threaded
//! state machine that never reads the clock: [`DockSwipe::post`] records when each re-sent copy
//! falls due, [`DockSwipe::next_deadline_ns`] tells the caller when that is, and
//! [`DockSwipe::tick`] posts the copies that are due.

use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGEvent, CGEventField, CGEventTapLocation};
use touchhid::clock;

use crate::LogSink;

/// The delays the end pair is re-sent at, in nanoseconds.
const RESEND_DELAYS_NS: [u64; 2] = [200_000_000, 500_000_000];

/// The swipe's direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Kind {
    /// Spaces / full-screen apps (four fingers left / right).
    Horizontal = 1,
    /// Mission Control (up) / App Exposé (down).
    Vertical = 2,
    /// Launchpad (pinch in) / Show Desktop (spread out).
    Pinch = 3,
}

impl Kind {
    /// The case's name, as the Swift source writes it into the dry-run line.
    fn name(self) -> &'static str {
        match self {
            Kind::Horizontal => "horizontal",
            Kind::Vertical => "vertical",
            Kind::Pinch => "pinch",
        }
    }
}

/// One step's place in the swipe, as the private dock-swipe subtype encodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Phase {
    Began = 1,
    Changed = 2,
    Ended = 4,
    Cancelled = 8,
}

impl Phase {
    /// The case's name, as the Swift source writes it into the dry-run line.
    fn name(self) -> &'static str {
        match self {
            Phase::Began => "began",
            Phase::Changed => "changed",
            Phase::Ended => "ended",
            Phase::Cancelled => "cancelled",
        }
    }
}

/// A re-send of the end pair: when it falls due, and the copied events to post then.
struct Resend {
    due_ns: u64,
    e30: CFRetained<CGEvent>,
    e29: CFRetained<CGEvent>,
}

/// Posts the dock-swipe gesture events, and re-sends the end pair when the caller's time says to.
pub struct DockSwipe {
    /// The accumulated progress the system completes or cancels the swipe from.
    origin_offset: f64,
    last_delta: f64,
    /// The pending re-sends of the end pair.
    resends: Vec<Resend>,
    /// Set by `post_raw` for experiments with type values beyond the known three.
    raw_kind: Option<i64>,
    /// When true, nothing is posted; every step is logged instead (the experiment switch).
    pub dry_run: bool,
    /// The dry-run record of a swipe. `None` unless a log is set, and a line is built only when
    /// one is.
    pub log: Option<LogSink>,
}

impl Default for DockSwipe {
    fn default() -> Self {
        Self::new()
    }
}

impl DockSwipe {
    /// A swipe that posts nothing until a step comes in.
    pub fn new() -> Self {
        Self {
            origin_offset: 0.0,
            last_delta: 0.0,
            resends: Vec::new(),
            raw_kind: None,
            dry_run: false,
            log: None,
        }
    }

    /// `delta` is this step's progress; the system completes or cancels the swipe from the
    /// accumulated progress and the last step's direction when it ends.
    /// For experiments with type values beyond the known three.
    ///
    /// The re-sends an end arms need a time to fall due at, and this door carries none, so the
    /// driver's clock is read once here.
    pub fn post_raw(&mut self, delta: f64, raw_kind: i64, phase: Phase) {
        self.raw_kind = Some(raw_kind);
        self.post(delta, Kind::Vertical, phase, false, clock::now_ns());
        self.raw_kind = None;
    }

    /// Posts one step of the swipe to the session tap. `delta` is this step's progress; the system
    /// completes or cancels the swipe from the accumulated progress and the last step's direction
    /// when it ends. `inverted` is field 136, the flip the trackpad's events carry.
    ///
    /// A `began` cancels the pending re-sends and resets the origin, a `changed` with zero delta is
    /// dropped, and an `ended` against the swipe's direction becomes a `cancelled`. After an `ended`
    /// or a `cancelled`, the same two events are re-sent 0.2 s and 0.5 s later, by [`tick`].
    ///
    /// [`tick`]: DockSwipe::tick
    pub fn post(&mut self, delta: f64, kind: Kind, phase: Phase, inverted: bool, now_ns: u64) {
        let mut phase = phase;
        match phase {
            Phase::Began => {
                self.cancel_resends();
                self.origin_offset = delta;
            }
            Phase::Changed => {
                if delta == 0.0 {
                    return;
                }
                self.origin_offset += delta;
            }
            Phase::Ended | Phase::Cancelled => {
                // Ending against the swipe's direction cancels it.
                if phase == Phase::Ended && (self.last_delta < 0.0) != (self.origin_offset < 0.0) {
                    phase = Phase::Cancelled;
                }
            }
        }
        let exit_speed = self.last_delta * 100.0;
        self.last_delta = delta;
        let Some((e30, e29)) = self.make_events(kind, phase, exit_speed, inverted) else {
            return;
        };
        if self.dry_run {
            self.log_line(|| format!(
                "dry-run dock swipe {} {} offset {:.3}",
                kind.name(),
                phase.name(),
                self.origin_offset
            ));
            return;
        }
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&e30));
        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&e29));
        if matches!(phase, Phase::Ended | Phase::Cancelled) {
            self.schedule_resends(&e30, &e29, now_ns);
        }
    }

    /// Hands `line` to the log when one is set; a line nobody reads is never built.
    fn log_line(&self, line: impl FnOnce() -> String) {
        if let Some(log) = &self.log {
            log(&line());
        }
    }

    /// The same swipe, handing its dry-run lines to `log`.
    pub fn logging_to(mut self, log: impl Fn(&str) + Send + 'static) -> Self {
        self.log = Some(Box::new(log));
        self
    }

    /// Posts the re-sends of the end pair that have fallen due by `now_ns`, and drops them.
    pub fn tick(&mut self, now_ns: u64) {
        for resend in &self.resends {
            if resend.due_ns <= now_ns {
                CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&resend.e30));
                CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&resend.e29));
            }
        }
        self.resends.retain(|resend| resend.due_ns > now_ns);
    }

    /// When the next pending re-send falls due; `None` when none is pending.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        self.resends.iter().map(|resend| resend.due_ns).min()
    }

    /// The two events of one step: the type-30 carrier with the dock-swipe subtype, then the
    /// type-29 companion. Every field is laid down as the Swift source lays it down.
    fn make_events(
        &self,
        kind: Kind,
        phase: Phase,
        exit_speed: f64,
        inverted: bool,
    ) -> Option<(CFRetained<CGEvent>, CFRetained<CGEvent>)> {
        let e30 = CGEvent::new(None)?;
        let e29 = CGEvent::new(None)?;
        let field = CGEventField;
        let raw_type = self.raw_kind.unwrap_or(kind as i64);
        // The type's bits reinterpreted as a float, like the trackpad's events carry it.
        let type_bits = f32::from_bits(raw_type as u32) as f64;

        CGEvent::set_double_value_field(Some(&e29), field(55), 29.0); // NSEventTypeGesture
        CGEvent::set_double_value_field(Some(&e29), field(41), 33231.0);

        CGEvent::set_double_value_field(Some(&e30), field(55), 30.0);
        CGEvent::set_double_value_field(Some(&e30), field(110), 23.0); // kIOHIDEventTypeDockSwipe
        CGEvent::set_double_value_field(Some(&e30), field(132), phase as i64 as f64);
        CGEvent::set_double_value_field(Some(&e30), field(134), phase as i64 as f64);
        CGEvent::set_double_value_field(Some(&e30), field(124), self.origin_offset);
        CGEvent::set_integer_value_field(
            Some(&e30),
            field(135),
            i64::from((self.origin_offset as f32).to_bits()),
        );
        CGEvent::set_double_value_field(Some(&e30), field(41), 33231.0);
        CGEvent::set_double_value_field(Some(&e30), field(119), type_bits);
        CGEvent::set_double_value_field(Some(&e30), field(139), type_bits);
        CGEvent::set_double_value_field(Some(&e30), field(123), raw_type as f64);
        CGEvent::set_double_value_field(Some(&e30), field(165), raw_type as f64);
        CGEvent::set_integer_value_field(Some(&e30), field(136), i64::from(inverted));
        if matches!(phase, Phase::Ended | Phase::Cancelled) {
            CGEvent::set_double_value_field(Some(&e30), field(129), exit_speed);
            CGEvent::set_double_value_field(Some(&e30), field(130), exit_speed);
        }
        Some((e30, e29))
    }

    /// Arms the two re-sends of the end pair: copies of the events, due 0.2 s and 0.5 s out.
    fn schedule_resends(&mut self, e30: &CFRetained<CGEvent>, e29: &CFRetained<CGEvent>, now_ns: u64) {
        self.cancel_resends();
        self.resends = RESEND_DELAYS_NS
            .iter()
            .map(|&delay| Resend {
                due_ns: now_ns + delay,
                // A copy failed only means memory ran out; the retained original posts the same.
                e30: CGEvent::new_copy(Some(e30)).unwrap_or_else(|| e30.clone()),
                e29: CGEvent::new_copy(Some(e29)).unwrap_or_else(|| e29.clone()),
            })
            .collect();
    }

    /// Drops the pending re-sends; the owned events go with them.
    fn cancel_resends(&mut self) {
        self.resends.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A dry-run swipe that records its status lines.
    fn recording_swipe() -> (DockSwipe, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let mut swipe = DockSwipe::new();
        swipe.dry_run = true;
        swipe.log = Some(Box::new(move |line: &str| sink.lock().unwrap().push(line.to_owned())));
        (swipe, lines)
    }

    #[test]
    fn a_step_is_logged_with_its_kind_phase_and_accumulated_offset() {
        let (mut swipe, lines) = recording_swipe();
        swipe.post(0.2, Kind::Vertical, Phase::Began, false, 0);
        swipe.post(0.1, Kind::Vertical, Phase::Changed, false, 0);
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            [
                "dry-run dock swipe vertical began offset 0.200",
                "dry-run dock swipe vertical changed offset 0.300",
            ]
        );
    }

    #[test]
    fn a_changed_step_with_zero_delta_is_dropped() {
        let (mut swipe, lines) = recording_swipe();
        swipe.post(0.3, Kind::Horizontal, Phase::Began, false, 0);
        swipe.post(0.0, Kind::Horizontal, Phase::Changed, false, 0);
        swipe.post(0.1, Kind::Horizontal, Phase::Changed, false, 0);
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            [
                "dry-run dock swipe horizontal began offset 0.300",
                "dry-run dock swipe horizontal changed offset 0.400",
            ]
        );
    }

    #[test]
    fn a_began_resets_the_origin() {
        let (mut swipe, lines) = recording_swipe();
        swipe.post(0.5, Kind::Pinch, Phase::Began, false, 0);
        // A new swipe starts here: its origin is this step alone, not 0.5 plus 0.9.
        swipe.post(0.9, Kind::Pinch, Phase::Began, false, 0);
        swipe.post(0.1, Kind::Pinch, Phase::Changed, false, 0);
        assert_eq!(
            lines.lock().unwrap().last().unwrap(),
            "dry-run dock swipe pinch changed offset 1.000"
        );
    }

    #[test]
    fn an_end_in_the_swipe_s_direction_stays_an_end() {
        let (mut swipe, lines) = recording_swipe();
        swipe.post(0.6, Kind::Vertical, Phase::Began, false, 0);
        swipe.post(0.1, Kind::Vertical, Phase::Changed, false, 0);
        swipe.post(0.0, Kind::Vertical, Phase::Ended, false, 0);
        assert_eq!(
            lines.lock().unwrap().last().unwrap(),
            "dry-run dock swipe vertical ended offset 0.700"
        );
    }

    #[test]
    fn ending_against_the_swipe_s_direction_becomes_a_cancel() {
        let (mut swipe, lines) = recording_swipe();
        swipe.post(-0.6, Kind::Vertical, Phase::Began, false, 0);
        swipe.post(0.1, Kind::Vertical, Phase::Changed, false, 0);
        swipe.post(0.0, Kind::Vertical, Phase::Ended, false, 0);
        assert_eq!(
            lines.lock().unwrap().last().unwrap(),
            "dry-run dock swipe vertical cancelled offset -0.500"
        );
    }

    #[test]
    fn a_cancelled_step_passes_through_as_a_cancel() {
        let (mut swipe, lines) = recording_swipe();
        swipe.post(0.6, Kind::Vertical, Phase::Began, false, 0);
        swipe.post(0.0, Kind::Vertical, Phase::Cancelled, false, 0);
        assert_eq!(
            lines.lock().unwrap().last().unwrap(),
            "dry-run dock swipe vertical cancelled offset 0.600"
        );
    }
}
