//! The deliverer: posts one gesture at a time, either straight to the target app (Direct) or
//! through the window server with the cursor briefly moved there (Warp).
//!
//! The Swift deliverer kept its state on a dispatch queue and scheduled its delays (the cursor's
//! return after a Warp gesture, the focus hand-back after a click) with `dispatch_after`, and its
//! scroll momentum with a 120 Hz `DispatchSourceTimer`. Here it is a single-threaded state machine:
//! the methods that start a delay take `now_ns`, the deadlines live in the struct,
//! [`Deliverer::tick`] performs whatever is due, in deadline order, and
//! [`Deliverer::next_deadline_ns`] tells the caller exactly when to wake next.

use objc2_core_foundation::{CGPoint, CFRetained};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGEvent, CGEventField, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton, CGWarpMouseCursorPosition,
};
use touchcore::gestures::GesturePhase;
use touchcore::momentum::Momentum;
use touchcore::policy::FocusMode;

use crate::LogSink;
use crate::apps;
use crate::cursor::CursorVisibility;
use crate::direct::DirectTarget;
use crate::event::{self, MomentumPhase, Phase, ScrollGesture};
use crate::journal::{PressJournal, PressRecord};
use crate::swift_text::swift_point;

/// How long the cursor stays at the finger after a Warp gesture before returning (Swift's
/// `warpReturnDelay` of 0.05 s).
pub const WARP_RETURN_DELAY_NS: u64 = 50_000_000;

/// How long after a gesture ends the previously active app is re-activated. Longer than the
/// double-click interval, so a double-tap doesn't bounce focus back and forth (Swift's
/// `focusReturnDelay` of 0.6 s).
pub const FOCUS_RETURN_DELAY_NS: u64 = 600_000_000;

/// Swift's momentum timer period: one scroll frame every 1/120 s.
const MOMENTUM_FRAME_NS: u64 = 1_000_000_000 / 120;

/// Everything the deliverer does to the outside world, so tests can watch it.
pub trait DeliveryEffects: Send {
    fn post(&mut self, event: &CGEvent, target: Target);
    fn cursor_location(&self) -> Option<CGPoint>;
    fn warp_cursor(&mut self, point: CGPoint);
    fn reassociate_cursor(&mut self);
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);
    fn frontmost_app(&self) -> Option<i32>;
    fn activate(&mut self, pid: i32);
    fn record_press(&mut self, record: &PressRecord);
    fn clear_press(&mut self);
    fn pending_press(&self) -> Option<PressRecord>;
    /// The bundle identifier of a running process; `None` when none runs with that pid.
    fn running_app(&self, pid: i32) -> Option<String>;
}

/// Where an event goes: through the window server at the cursor (`Warp`), or straight into an
/// app's process (`Direct`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Warp,
    Direct(DirectTarget),
}

// CGPoint only derives `PartialEq` (a float cannot honestly promise `Eq`), but Swift's
// `Target: Equatable` compares the same way, so equality here means the same thing.
impl Eq for Target {}

/// The real effects: window server, cursor, app activation, press journal on disk.
pub struct SystemDeliveryEffects {
    journal: PressJournal,
}

impl SystemDeliveryEffects {
    /// Asks the window server for background cursor hiding first, as the Swift `init` does.
    pub fn new() -> Self {
        CursorVisibility::allow_hiding_in_background();
        Self { journal: PressJournal::new() }
    }
}

impl Default for SystemDeliveryEffects {
    fn default() -> Self {
        Self::new()
    }
}

impl DeliveryEffects for SystemDeliveryEffects {
    fn post(&mut self, event: &CGEvent, target: Target) {
        match target {
            Target::Direct(direct) => direct.post(event),
            Target::Warp => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event)),
        }
    }

    /// The cursor's position, read off a fresh event with no source (what `CGEvent(source: nil)`
    /// reports its location as).
    fn cursor_location(&self) -> Option<CGPoint> {
        CGEvent::new(None).map(|event| CGEvent::location(Some(&event)))
    }

    fn warp_cursor(&mut self, point: CGPoint) {
        CGWarpMouseCursorPosition(point);
    }

    fn reassociate_cursor(&mut self) {
        CGAssociateMouseAndMouseCursorPosition(true);
    }

    fn hide_cursor(&mut self) {
        CursorVisibility::hide();
    }

    fn show_cursor(&mut self) {
        CursorVisibility::show();
    }

    fn frontmost_app(&self) -> Option<i32> {
        apps::frontmost().map(|app| app.processIdentifier())
    }

    fn activate(&mut self, pid: i32) {
        let _ = apps::with_pid(pid).is_some_and(|app| apps::activate(&app));
    }

    fn record_press(&mut self, record: &PressRecord) {
        self.journal.record(record);
    }

    fn clear_press(&mut self) {
        self.journal.clear();
    }

    fn pending_press(&self) -> Option<PressRecord> {
        self.journal.pending()
    }

    fn running_app(&self, pid: i32) -> Option<String> {
        apps::with_pid(pid).and_then(|app| app.bundleIdentifier()).map(|id| id.to_string())
    }
}

/// What is left of a started scroll momentum: the frames still to play, and when the next one is
/// due (Swift's repeating timer became this deadline and [`Deliverer::tick`]).
struct MomentumRun {
    momentum: Momentum,
    next_frame_ns: u64,
}

/// The focus hand-back Swift scheduled with `dispatch_after`: whose app to re-activate, and when.
struct FocusReturn {
    deadline_ns: u64,
    pid: i32,
}

/// The scroll/pinch/rotate phase an app has been told began and not yet told ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenPhase {
    Scroll,
    Magnify,
    Rotate,
}

/// Posts one gesture at a time, either straight to the target app (Direct) or through the window
/// server with the cursor briefly moved there (Warp).
pub struct Deliverer<E: DeliveryEffects> {
    /// Post nowhere and log every event instead.
    pub dry_run: bool,
    /// Who owns focus after a touch.
    pub focus_mode: FocusMode,
    /// Trackpad setting "momentum scrolling".
    pub momentum_enabled: bool,
    /// Scrolls carry the trackpad's gesture stream (type 29: subtype 61, 6 per move, 62), from
    /// which macOS's swipe tracking measures progress: Safari's back/forward page swipe follows
    /// the fingers. Set per gesture.
    pub scroll_gesture_stream: bool,
    /// Where the log lines go (a gesture's begin, and every event a dry run would have posted).
    /// `None` unless one is set, and a line is built only when it is.
    pub log: Option<LogSink>,

    effects: E,
    /// Events for Direct delivery are built on a private source, Warp events on the HID source,
    /// as the Swift originals are.
    direct_source: Option<CFRetained<CGEventSource>>,
    warp_source: Option<CFRetained<CGEventSource>>,
    target: Target,
    location: CGPoint,
    event_number: i64,
    /// The button currently held down, if any.
    button_down: Option<CGMouseButton>,
    open_phase: Option<OpenPhase>,
    /// Warp has the cursor hidden and parked at a finger; `saved_cursor` is where it came from.
    parked: bool,
    saved_cursor: Option<CGPoint>,
    /// When the parked cursor is due back at its saved position (Swift's `restore` work item).
    restore_deadline_ns: Option<u64>,
    momentum: Option<MomentumRun>,
    /// The app that was frontmost before touches activated another one (for "return focus").
    previous_frontmost: Option<i32>,
    focus_return: Option<FocusReturn>,
}

impl<E: DeliveryEffects> Deliverer<E> {
    /// A deliverer that owns `effects` and its two event sources.
    pub fn new(effects: E) -> Self {
        Self {
            dry_run: false,
            focus_mode: FocusMode::FollowTouch,
            momentum_enabled: true,
            scroll_gesture_stream: false,
            log: None,
            effects,
            direct_source: CGEventSource::new(CGEventSourceStateID::Private),
            warp_source: CGEventSource::new(CGEventSourceStateID::HIDSystemState),
            target: Target::Warp,
            location: CGPoint::new(0.0, 0.0),
            event_number: 0,
            button_down: None,
            open_phase: None,
            parked: false,
            saved_cursor: None,
            restore_deadline_ns: None,
            momentum: None,
            previous_frontmost: None,
            focus_return: None,
        }
    }

    /// Performs everything that is due by `now_ns`, in deadline order: the parked cursor's return,
    /// the focus hand-back, and momentum frames, one per frame interval.
    pub fn tick(&mut self, now_ns: u64) {
        loop {
            let Some(due) = self.next_deadline_ns() else { return };
            if due > now_ns {
                return;
            }
            if self.restore_deadline_ns == Some(due) {
                self.return_cursor();
                continue;
            }
            if let Some(focus) = self.focus_return.take_if(|focus| focus.deadline_ns == due) {
                self.previous_frontmost = None;
                self.effects.activate(focus.pid);
                continue;
            }
            if self.momentum.as_ref().is_some_and(|run| run.next_frame_ns == due) {
                // The next frame keeps its place on the 120 Hz grid the run started on. A tick that
                // arrives late (a stalled process) plays one frame and skips the fires it missed,
                // so the missed scroll is not dumped at once.
                let missed = now_ns.saturating_sub(due) / MOMENTUM_FRAME_NS;
                let step = self.momentum.as_mut().map(|run| {
                    run.next_frame_ns = due.saturating_add((missed + 1) * MOMENTUM_FRAME_NS);
                    run.momentum.next_frame()
                });
                match step {
                    Some(Some((dx, dy))) => {
                        self.post(event::scroll(
                            dx,
                            dy,
                            self.location,
                            None,
                            MomentumPhase::Continue,
                            self.window(),
                            self.source(),
                        ));
                    }
                    _ => {
                        // The momentum died out: close it as Swift's timer handler does.
                        self.stop_momentum();
                        self.end(now_ns, WARP_RETURN_DELAY_NS);
                    }
                }
                continue;
            }
            // `due` is the earliest pending deadline, so it matched a branch above.
            return;
        }
    }

    /// The earliest pending deadline, for the caller to sleep exactly until then.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        [
            self.restore_deadline_ns,
            self.focus_return.as_ref().map(|focus| focus.deadline_ns),
            self.momentum.as_ref().map(|run| run.next_frame_ns),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    // MARK: gesture lifecycle

    /// Starts a gesture at `at`. `app_pid` is the app owning the touched window; `activate` asks
    /// for it to be made active first (macOS swallows clicks and drops pinches aimed at a
    /// background app's window).
    pub fn begin(&mut self, target: Target, at: CGPoint, app_pid: Option<i32>, activate: bool) {
        self.stop_momentum();
        self.target = target;
        self.location = at;
        self.event_number += 1;
        self.log_line(|| format!("begin {} at {},{}", target_label(&target), at.x as i64, at.y as i64));
        self.focus_return = None;
        if activate
            && let Some(app_pid) = app_pid
                && !self.dry_run {
                    let frontmost = self.effects.frontmost_app();
                    if frontmost != Some(app_pid) {
                        if self.previous_frontmost.is_none() {
                            self.previous_frontmost = frontmost;
                        }
                        // A Warp click is routed by the window server, which activates the app by
                        // itself.
                        if matches!(target, Target::Direct(_)) {
                            self.effects.activate(app_pid);
                        }
                    }
                }
        self.restore_deadline_ns = None;
        if target != Target::Warp || self.dry_run {
            // A Direct gesture right after a Warp one (or its momentum) gives the cursor back now.
            self.return_cursor();
            return;
        }
        if !self.parked {
            self.saved_cursor = self.effects.cursor_location();
            self.effects.hide_cursor();
            self.parked = true;
        }
        self.effects.warp_cursor(at);
    }

    /// Ends a gesture. `return_delay_ns` is how long a parked cursor stays at the finger (longer
    /// for system gestures, which pick their display from the cursor while their animation
    /// starts); `WARP_RETURN_DELAY_NS` is the usual value.
    pub fn end(&mut self, now_ns: u64, return_delay_ns: u64) {
        self.schedule_focus_return(now_ns);
        if !self.parked {
            return;
        }
        self.restore_deadline_ns = Some(now_ns.saturating_add(return_delay_ns));
    }

    /// Arms the focus hand-back, when the mode asks for it and another app was frontmost before
    /// the touch.
    fn schedule_focus_return(&mut self, now_ns: u64) {
        let previous = match (self.focus_mode, self.previous_frontmost) {
            (FocusMode::ReturnToPrevious, Some(previous)) => previous,
            _ => {
                self.previous_frontmost = None;
                return;
            }
        };
        self.focus_return = Some(FocusReturn {
            deadline_ns: now_ns.saturating_add(FOCUS_RETURN_DELAY_NS),
            pid: previous,
        });
    }

    // MARK: clicks and drags

    /// A click: the press and its release back to back. `count` is the click state (a double-tap
    /// is 2), which macOS matches clicks by.
    pub fn click(&mut self, at: CGPoint, count: i64, button: CGMouseButton) {
        let (down, up) = if button == CGMouseButton::Right {
            (CGEventType::RightMouseDown, CGEventType::RightMouseUp)
        } else {
            (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp)
        };
        self.press(down, at, button, count);
        self.release(up, at, button, count);
    }

    /// A drag starts: the left button goes down at `at`.
    pub fn drag_down(&mut self, at: CGPoint) {
        self.press(CGEventType::LeftMouseDown, at, CGMouseButton::Left, 1);
    }

    /// The finger moved during a drag; the position is posted as reported.
    pub fn drag_move(&mut self, at: CGPoint) {
        self.post_drag_move(at);
    }

    /// The drag ends with the finger's lift.
    pub fn drag_up(&mut self, at: CGPoint) {
        self.release(CGEventType::LeftMouseUp, at, CGMouseButton::Left, 1);
    }

    /// A mouse-moved event with a push direction, through the window server. It moves the parked
    /// cursor, and pushes against a display edge (the Dock moves to a display when the cursor
    /// pushes against its bottom edge).
    pub fn nudge(&mut self, at: CGPoint, dx: i64, dy: i64) {
        let Some(event) = CGEvent::new_mouse_event(
            self.warp_source.as_deref(),
            CGEventType::MouseMoved,
            at,
            CGMouseButton::Left,
        ) else {
            return;
        };
        CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaX, dx);
        CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaY, dy);
        if self.dry_run {
            return;
        }
        self.effects.post(&event, Target::Warp);
    }

    /// Sends the missing mouse-up if a press is still open (quit, disable, crash recovery).
    pub fn release_stuck_press(&mut self, now_ns: u64) {
        if let Some(button) = self.button_down {
            let up = if button == CGMouseButton::Right {
                CGEventType::RightMouseUp
            } else {
                CGEventType::LeftMouseUp
            };
            self.release(up, self.location, button, 1);
        }
        let Some(pending) = self.effects.pending_press() else {
            self.return_cursor();
            return;
        };
        self.effects.clear_press();
        let cursor = self.effects.cursor_location().unwrap_or(CGPoint::new(0.0, 0.0));
        let at = CGPoint::new(pending.x.unwrap_or(cursor.x), pending.y.unwrap_or(cursor.y));
        let (button, up) = if pending.right {
            (CGMouseButton::Right, CGEventType::RightMouseUp)
        } else {
            (CGMouseButton::Left, CGEventType::LeftMouseUp)
        };
        if pending.pid <= 0 {
            // Warp: the window server still holds the button. Release it where it was pressed, so
            // a drag that was under way drops back where it started; the hidden cursor visits
            // that point briefly.
            self.begin(Target::Warp, at, None, false);
            self.release(up, at, button, 1);
            self.end(now_ns, WARP_RETURN_DELAY_NS);
            self.log_line(|| format!(
                "released a press left open by a previous run at {},{}",
                at.x as i64, at.y as i64
            ));
            return;
        }
        // Direct: on the window it was pressed in, and only if the same app still runs.
        match (pending.window, pending.origin_x, pending.origin_y, pending.bundle_id) {
            (Some(window), Some(origin_x), Some(origin_y), Some(app))
                if self.effects.running_app(pending.pid).as_deref() == Some(app.as_str()) =>
            {
                let target = Target::Direct(DirectTarget {
                    pid: pending.pid,
                    window,
                    origin: CGPoint::new(origin_x, origin_y),
                });
                self.begin(target, at, None, false);
                self.release(up, at, button, 1);
                self.log_line(|| format!(
                    "released a press left open by a previous run in {app} at {},{}",
                    at.x as i64, at.y as i64
                ));
            }
            _ => {
                self.log_line(|| {
                    "a press left open by a previous run belonged to an app that is gone; nothing to release".to_owned()
                });
                self.return_cursor();
            }
        }
    }

    /// Closes whatever is open (momentum, scroll/pinch/rotate phase, pressed button) and gives the
    /// cursor back. For Disable, Quit, and the touch display disappearing mid-gesture.
    pub fn abort_gesture(&mut self) {
        self.stop_momentum();
        if let Some(phase) = self.open_phase.take() {
            match phase {
                OpenPhase::Scroll => {
                    self.post(event::scroll(
                        0.0,
                        0.0,
                        self.location,
                        Some(Phase::Ended),
                        MomentumPhase::None,
                        self.window(),
                        self.source(),
                    ));
                    self.end_scroll_gesture();
                }
                OpenPhase::Magnify => {
                    self.post(event::magnify(0.0, self.location, Phase::Ended, self.window(), self.source()));
                }
                OpenPhase::Rotate => {
                    self.post(event::rotate(0.0, self.location, Phase::Ended, self.window(), self.source()));
                }
            }
        }
        if let Some(button) = self.button_down {
            let up = if button == CGMouseButton::Right {
                CGEventType::RightMouseUp
            } else {
                CGEventType::LeftMouseUp
            };
            self.release(up, self.location, button, 1);
        }
        self.focus_return = None;
        self.previous_frontmost = None;
        self.restore_deadline_ns = None;
        self.return_cursor();
    }

    // MARK: scroll and gestures

    /// One scroll frame. A finger scroll is continuous and carries no momentum.
    pub fn scroll(&mut self, dx: f64, dy: f64, phase: GesturePhase) {
        self.post(event::scroll(
            dx,
            dy,
            self.location,
            Some(synth_phase(phase)),
            MomentumPhase::None,
            self.window(),
            self.source(),
        ));
        self.open_phase = (phase != GesturePhase::Ended).then_some(OpenPhase::Scroll);
        if !self.scroll_gesture_stream {
            return;
        }
        match phase {
            GesturePhase::Began => self.post_scroll_gesture(ScrollGesture::Begin),
            GesturePhase::Changed => {
                self.post_scroll_gesture(ScrollGesture::Move { dx, dy, phase: Phase::Changed })
            }
            GesturePhase::Ended => self.end_scroll_gesture(),
        }
    }

    fn post_scroll_gesture(&mut self, part: ScrollGesture) {
        self.post(event::scroll_gesture(part, self.location, self.window(), self.source()));
    }

    /// Closes the gesture stream after the scroll's `ended` event, as the trackpad does.
    fn end_scroll_gesture(&mut self) {
        if !self.scroll_gesture_stream {
            return;
        }
        self.post_scroll_gesture(ScrollGesture::Move { dx: 0.0, dy: 0.0, phase: Phase::Ended });
        self.post_scroll_gesture(ScrollGesture::End);
    }

    /// Ends the finger phase, then plays momentum (points per second) and finishes the gesture.
    pub fn scroll_ended(&mut self, velocity_x: f64, velocity_y: f64, now_ns: u64) {
        self.post(event::scroll(
            0.0,
            0.0,
            self.location,
            Some(Phase::Ended),
            MomentumPhase::None,
            self.window(),
            self.source(),
        ));
        self.open_phase = None;
        self.end_scroll_gesture();
        let mut momentum = Momentum::new(velocity_x, velocity_y);
        if !self.momentum_enabled {
            self.end(now_ns, WARP_RETURN_DELAY_NS);
            return;
        }
        let Some(first) = momentum.next_frame() else {
            self.end(now_ns, WARP_RETURN_DELAY_NS);
            return;
        };
        self.post(event::scroll(
            first.0,
            first.1,
            self.location,
            None,
            MomentumPhase::Begin,
            self.window(),
            self.source(),
        ));
        self.momentum = Some(MomentumRun {
            momentum,
            next_frame_ns: now_ns.saturating_add(MOMENTUM_FRAME_NS),
        });
    }

    /// One pinch frame.
    pub fn magnify(&mut self, delta: f64, phase: GesturePhase, now_ns: u64) {
        self.post(event::magnify(delta, self.location, synth_phase(phase), self.window(), self.source()));
        self.open_phase = (phase != GesturePhase::Ended).then_some(OpenPhase::Magnify);
        if phase == GesturePhase::Ended {
            self.end(now_ns, WARP_RETURN_DELAY_NS);
        }
    }

    /// One rotate frame. `degrees`, positive = counterclockwise.
    pub fn rotate(&mut self, degrees: f64, phase: GesturePhase, now_ns: u64) {
        self.post(event::rotate(degrees, self.location, synth_phase(phase), self.window(), self.source()));
        self.open_phase = (phase != GesturePhase::Ended).then_some(OpenPhase::Rotate);
        if phase == GesturePhase::Ended {
            self.end(now_ns, WARP_RETURN_DELAY_NS);
        }
    }

    // MARK: internals

    /// The target window of a Direct delivery, for the event fields that name it.
    fn window(&self) -> Option<i64> {
        match self.target {
            Target::Warp => None,
            Target::Direct(direct) => Some(direct.window),
        }
    }

    /// Direct events are built on a private source, Warp events on the HID source.
    fn source(&self) -> Option<&CGEventSource> {
        if self.target == Target::Warp {
            self.warp_source.as_deref()
        } else {
            self.direct_source.as_deref()
        }
    }

    /// Records the open press in the journal before posting the press event, so a crash in
    /// between leaves the journal holding it.
    fn press(&mut self, event_type: CGEventType, at: CGPoint, button: CGMouseButton, click_state: i64) {
        self.button_down = Some(button);
        let record = self.press_record(at, button == CGMouseButton::Right);
        self.effects.record_press(&record);
        self.post(event::mouse(
            event_type,
            at,
            button,
            click_state,
            self.event_number,
            self.window(),
            self.source(),
        ));
    }

    /// What the journal needs to release the press where it was made.
    fn press_record(&self, at: CGPoint, right: bool) -> PressRecord {
        match self.target {
            Target::Warp => {
                PressRecord { pid: 0, right, x: Some(at.x), y: Some(at.y), ..Default::default() }
            }
            Target::Direct(direct) => PressRecord {
                pid: direct.pid,
                right,
                x: Some(at.x),
                y: Some(at.y),
                window: Some(direct.window),
                origin_x: Some(direct.origin.x),
                origin_y: Some(direct.origin.y),
                bundle_id: self.effects.running_app(direct.pid),
            },
        }
    }

    fn release(&mut self, event_type: CGEventType, at: CGPoint, button: CGMouseButton, click_state: i64) {
        self.post(event::mouse(
            event_type,
            at,
            button,
            click_state,
            self.event_number,
            self.window(),
            self.source(),
        ));
        self.button_down = None;
        self.effects.clear_press();
    }

    fn post_drag_move(&mut self, at: CGPoint) {
        self.location = at;
        self.post(event::mouse(
            CGEventType::LeftMouseDragged,
            at,
            CGMouseButton::Left,
            1,
            self.event_number,
            self.window(),
            self.source(),
        ));
    }

    /// Posts the event, or logs it in a dry run.
    fn post(&mut self, event: Option<CFRetained<CGEvent>>) {
        let Some(event) = event else { return };
        if self.dry_run {
            let location = CGEvent::location(Some(&event));
            self.log_line(|| format!(
                "dry-run {} at {},{} → {}",
                CGEvent::r#type(Some(&event)).0,
                location.x as i64,
                location.y as i64,
                target_label(&self.target),
            ));
            return;
        }
        // A debug build's log also names each press and drag it posts for real. Scrolls are left
        // out: there are dozens a second.
        #[cfg(feature = "debug")]
        {
            let kind = CGEvent::r#type(Some(&event));
            let press = [
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseDragged,
                CGEventType::LeftMouseUp,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
            ]
            .contains(&kind);
            if press {
                let location = CGEvent::location(Some(&event));
                let target = target_label(&self.target);
                self.log_line(|| format!("posted {} at {},{} ({target})", kind.0, location.x as i64, location.y as i64));
            }
        }
        self.effects.post(&event, self.target);
    }

    /// Hands `line` to the log when one is set; a line nobody reads is never built.
    fn log_line(&self, line: impl FnOnce() -> String) {
        if let Some(log) = &self.log {
            log(&line());
        }
    }

    /// The same deliverer, handing its log lines to `log`.
    pub fn logging_to(mut self, log: impl Fn(&str) + Send + 'static) -> Self {
        self.log = Some(Box::new(log));
        self
    }

    /// Stops the momentum and closes it with the momentum-end event the trackpad sends.
    fn stop_momentum(&mut self) {
        if self.momentum.take().is_none() {
            return;
        }
        self.post(event::scroll(
            0.0,
            0.0,
            self.location,
            None,
            MomentumPhase::End,
            self.window(),
            self.source(),
        ));
    }

    /// Gives the parked cursor back: to where it was saved, and visible again even if the
    /// original position was unknown.
    fn return_cursor(&mut self) {
        self.restore_deadline_ns = None;
        if !self.parked {
            return;
        }
        if let Some(saved) = self.saved_cursor {
            self.effects.warp_cursor(saved);
            self.effects.reassociate_cursor();
        }
        self.effects.show_cursor();
        self.saved_cursor = None;
        self.parked = false;
    }
}

/// The core's gesture phase as the event phase.
fn synth_phase(phase: GesturePhase) -> Phase {
    match phase {
        GesturePhase::Began => Phase::Began,
        GesturePhase::Changed => Phase::Changed,
        GesturePhase::Ended => Phase::Ended,
    }
}

/// What Swift's `\(target)` interpolation printed, for the log lines the owner's scripts read:
/// the case's name, and the target with the module Swift named it in.
fn target_label(target: &Target) -> String {
    match target {
        Target::Warp => "warp".to_string(),
        Target::Direct(direct) => format!(
            "direct(TouchPlatform.DirectTarget(pid: {}, window: {}, origin: {}))",
            direct.pid,
            direct.window,
            swift_point(direct.origin),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, MutexGuard};

    use objc2_core_graphics::{CGEvent, CGEventField, CGEventType, CGMouseButton};
    use touchcore::gestures::GesturePhase;

    use super::*;

    /// A point on the touch display, right of the main one.
    const TOUCH_DISPLAY_POINT: CGPoint = CGPoint::new(2500.0, 500.0);
    const DIRECT: Target =
        Target::Direct(DirectTarget { pid: 42, window: 7, origin: CGPoint::new(2000.0, 100.0) });
    /// The test's own clock, started a second in so every deadline arithmetic is plainly positive.
    const START_NS: u64 = 1_000_000_000;
    /// One `Thread.sleep` step of 0.1 s.
    const SLEEP_100_MS_NS: u64 = 100_000_000;
    /// One momentum frame, 1/120 s, written out so the test does not follow a changed interval.
    const FRAME_NS: u64 = 1_000_000_000 / 120;

    /// One posted event, as the Swift fake records it: type, location, the scroll and momentum
    /// phase fields (99 and 123), the target, and field 110 (the gesture subtype).
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct PostedEvent {
        event_type: CGEventType,
        location: CGPoint,
        scroll_phase: i64,
        momentum_phase: i64,
        target: Target,
        subtype: i64,
    }

    /// What the fake records, shared between the deliverer and the test through `Arc`.
    struct FakeEffectsState {
        posted: Vec<PostedEvent>,
        cursor: CGPoint, // starts on the MacBook screen
        hidden: bool,
        activated: Vec<i32>,
        frontmost: Option<i32>,
        press: Option<PressRecord>,
        running: HashMap<i32, String>,
    }

    impl Default for FakeEffectsState {
        fn default() -> Self {
            Self {
                posted: Vec::new(),
                cursor: CGPoint::new(100.0, 100.0),
                hidden: false,
                activated: Vec::new(),
                frontmost: Some(1),
                press: None,
                running: HashMap::from([(42, "com.example.app".to_string())]),
            }
        }
    }

    /// Records what the deliverer does instead of touching the window server.
    #[derive(Clone)]
    struct FakeEffects {
        state: Arc<Mutex<FakeEffectsState>>,
    }

    impl FakeEffects {
        fn new() -> Self {
            Self { state: Arc::new(Mutex::new(FakeEffectsState::default())) }
        }

        fn lock(&self) -> MutexGuard<'_, FakeEffectsState> {
            self.state.lock().unwrap()
        }
    }

    impl DeliveryEffects for FakeEffects {
        fn post(&mut self, event: &CGEvent, target: Target) {
            self.state.lock().unwrap().posted.push(PostedEvent {
                event_type: CGEvent::r#type(Some(event)),
                location: CGEvent::location(Some(event)),
                scroll_phase: CGEvent::integer_value_field(Some(event), CGEventField(99)),
                momentum_phase: CGEvent::integer_value_field(Some(event), CGEventField(123)),
                target,
                subtype: CGEvent::integer_value_field(Some(event), CGEventField(110)),
            });
        }

        fn cursor_location(&self) -> Option<CGPoint> {
            Some(self.lock().cursor)
        }

        fn warp_cursor(&mut self, point: CGPoint) {
            self.lock().cursor = point;
        }

        fn reassociate_cursor(&mut self) {}

        fn hide_cursor(&mut self) {
            self.lock().hidden = true;
        }

        fn show_cursor(&mut self) {
            self.lock().hidden = false;
        }

        fn frontmost_app(&self) -> Option<i32> {
            self.lock().frontmost
        }

        fn activate(&mut self, pid: i32) {
            let mut state = self.lock();
            state.activated.push(pid);
            state.frontmost = Some(pid);
        }

        fn record_press(&mut self, record: &PressRecord) {
            self.lock().press = Some(record.clone());
        }

        fn clear_press(&mut self) {
            self.lock().press = None;
        }

        fn pending_press(&self) -> Option<PressRecord> {
            self.lock().press.clone()
        }

        fn running_app(&self, pid: i32) -> Option<String> {
            self.lock().running.get(&pid).cloned()
        }
    }

    /// A deliverer over the fake, and the fake's own handle for asserting.
    fn make() -> (Deliverer<FakeEffects>, FakeEffects) {
        let effects = FakeEffects::new();
        let deliverer = Deliverer::new(effects.clone());
        (deliverer, effects)
    }

    /// The journal entry a Direct press on the test window is expected to leave.
    fn journaled_direct() -> PressRecord {
        PressRecord {
            pid: 42,
            right: false,
            x: Some(2500.0),
            y: Some(500.0),
            window: Some(7),
            origin_x: Some(2000.0),
            origin_y: Some(100.0),
            bundle_id: Some("com.example.app".to_string()),
        }
    }

    // Review finding 1: a Direct gesture that interrupts a Warp scroll's momentum left the cursor
    // hidden.
    #[test]
    fn a_direct_gesture_after_warp_momentum_gives_the_cursor_back() {
        let (mut d, fx) = make();
        let now = START_NS;
        d.begin(Target::Warp, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(0.0, 10.0, GesturePhase::Began);
        d.scroll_ended(0.0, 3000.0, now); // momentum running, cursor parked at the finger
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), true);
        assert!(!fx.lock().hidden);
        assert_eq!(fx.lock().cursor, CGPoint::new(100.0, 100.0), "back where it was on the MacBook screen");
    }

    #[test]
    fn a_warp_tap_returns_the_cursor_shortly_after() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.begin(Target::Warp, TOUCH_DISPLAY_POINT, Some(42), true);
        d.click(TOUCH_DISPLAY_POINT, 1, CGMouseButton::Left);
        d.end(now, WARP_RETURN_DELAY_NS);
        assert!(fx.lock().hidden, "stays at the finger for a moment");
        // 0.05 s, written out so the test does not follow a changed constant.
        now += 49_000_000;
        d.tick(now);
        assert!(fx.lock().hidden, "still at the finger just before the delay is up");
        now = START_NS + 51_000_000;
        d.tick(now);
        assert!(!fx.lock().hidden);
        assert_eq!(fx.lock().cursor, CGPoint::new(100.0, 100.0));
    }

    // Launchpad and Space switching open where the cursor is: it waits on the touch display until
    // the animation is under way.
    #[test]
    fn a_system_gesture_keeps_the_cursor_parked_for_the_requested_time() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.begin(Target::Warp, TOUCH_DISPLAY_POINT, None, false);
        d.end(now, 400_000_000);
        now += 150_000_000;
        d.tick(now);
        assert!(fx.lock().hidden);
        assert_eq!(fx.lock().cursor, TOUCH_DISPLAY_POINT, "still on the touch display");
        now += 400_000_000;
        d.tick(now);
        assert!(!fx.lock().hidden);
        assert_eq!(fx.lock().cursor, CGPoint::new(100.0, 100.0));
        assert!(fx.lock().posted.is_empty(), "parking posts nothing");
    }

    // Launchpad opens where the Dock is; the Dock moves to a display when the cursor pushes
    // against its bottom edge.
    #[test]
    fn a_nudge_pushes_the_parked_cursor_through_the_window_server() {
        let (mut d, fx) = make();
        let edge = CGPoint::new(2688.0, 1079.0);
        d.begin(Target::Warp, edge, None, false);
        d.nudge(edge, 0, 4);
        let state = fx.lock();
        assert_eq!(state.posted.len(), 1);
        assert_eq!(state.posted[0].event_type, CGEventType::MouseMoved);
        assert_eq!(state.posted[0].location, edge);
        assert_eq!(state.posted[0].target, Target::Warp);
        assert!(state.hidden, "the cursor stays hidden while it pushes");
    }

    #[test]
    fn a_nudge_in_dry_run_posts_nothing() {
        let (mut d, fx) = make();
        d.dry_run = true;
        d.nudge(CGPoint::new(2688.0, 1079.0), 0, 4);
        assert!(fx.lock().posted.is_empty());
    }

    // Safari's page swipe measures progress from the trackpad's gesture stream, not the scroll
    // events (Event Sink, S17).
    #[test]
    fn a_two_finger_scroll_carries_the_trackpads_gesture_stream() {
        let (mut d, fx) = make();
        let now = START_NS;
        d.scroll_gesture_stream = true;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(2.0, 0.0, GesturePhase::Began);
        d.scroll(18.0, 0.0, GesturePhase::Changed);
        d.scroll_ended(0.0, 0.0, now);
        let posted = fx.lock().posted.clone();
        // scroll began, gesture begin (61); scroll changed, gesture move (6); scroll ended,
        // gesture move ended (6), gesture end (62)
        assert_eq!(
            posted.iter().map(|event| event.event_type.0).collect::<Vec<_>>(),
            [22, 29, 22, 29, 22, 29, 29]
        );
        assert_eq!(
            posted.iter().filter(|event| event.event_type.0 == 29).map(|event| event.subtype).collect::<Vec<_>>(),
            [61, 6, 6, 62]
        );
    }

    #[test]
    fn without_the_stream_a_scroll_is_plain() {
        let (mut d, fx) = make();
        let now = START_NS;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(2.0, 0.0, GesturePhase::Began);
        d.scroll(18.0, 0.0, GesturePhase::Changed);
        d.scroll_ended(0.0, 0.0, now);
        assert!(fx.lock().posted.iter().all(|event| event.event_type.0 == 22));
    }

    #[test]
    fn aborting_a_streamed_scroll_closes_the_gesture() {
        let (mut d, fx) = make();
        d.scroll_gesture_stream = true;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(2.0, 0.0, GesturePhase::Began);
        d.abort_gesture();
        assert_eq!(fx.lock().posted.last().map(|event| event.subtype), Some(62), "the gesture is closed too");
    }

    // Crash safety (review finding 5): the journal holds enough to release a press where it was
    // made.
    #[test]
    fn a_direct_press_is_journaled_with_its_window_point_and_app() {
        let (mut d, fx) = make();
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.drag_down(TOUCH_DISPLAY_POINT);
        assert_eq!(fx.lock().press, Some(journaled_direct()));
    }

    #[test]
    fn a_crashed_direct_press_is_released_on_its_window_where_it_was_pressed() {
        let (mut d, fx) = make();
        fx.lock().press = Some(journaled_direct());
        d.release_stuck_press(START_NS);
        let state = fx.lock();
        assert_eq!(state.posted.len(), 1);
        let last = state.posted[0];
        assert_eq!(last.event_type, CGEventType::LeftMouseUp);
        assert_eq!(last.location, TOUCH_DISPLAY_POINT, "where it was pressed, not at the cursor");
        assert_eq!(last.target, DIRECT, "on the window it was pressed in");
        assert_eq!(state.press, None);
        assert_eq!(state.cursor, CGPoint::new(100.0, 100.0), "the cursor never moves");
    }

    #[test]
    fn a_crashed_press_is_dropped_when_its_app_is_gone_or_the_pid_was_reused() {
        let now = START_NS;
        for running in [
            HashMap::<i32, String>::new(),
            HashMap::from([(42, "com.other.app".to_string())]),
        ] {
            let (mut d, fx) = make();
            fx.lock().press = Some(journaled_direct());
            fx.lock().running = running;
            d.release_stuck_press(now);
            let state = fx.lock();
            assert!(state.posted.is_empty(), "{:?}", state.running);
            assert_eq!(state.press, None);
        }
    }

    #[test]
    fn a_crashed_warp_press_is_released_where_it_was_pressed() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        fx.lock().press = Some(PressRecord {
            pid: 0,
            right: false,
            x: Some(2500.0),
            y: Some(500.0),
            ..Default::default()
        });
        d.release_stuck_press(now);
        let last = fx.lock().posted[0];
        assert_eq!(last.event_type, CGEventType::LeftMouseUp);
        assert_eq!(last.location, TOUCH_DISPLAY_POINT, "a drag under way drops back where it started");
        assert_eq!(last.target, Target::Warp);
        now += 100_000_000;
        d.tick(now);
        assert_eq!(fx.lock().cursor, CGPoint::new(100.0, 100.0));
        assert!(!fx.lock().hidden);
    }

    #[test]
    fn an_old_journal_without_a_point_is_released_at_the_cursor_as_before() {
        let (mut d, fx) = make();
        fx.lock().press = Some(PressRecord { pid: 0, right: true, ..Default::default() });
        d.release_stuck_press(START_NS);
        let last = fx.lock().posted[0];
        assert_eq!(last.event_type, CGEventType::RightMouseUp);
        assert_eq!(last.location, CGPoint::new(100.0, 100.0));
    }

    // Review findings 3 and 4: stopping or losing the display mid-gesture must close everything.
    #[test]
    fn abort_during_a_warp_drag_releases_the_button_and_the_cursor() {
        let (mut d, fx) = make();
        d.begin(Target::Warp, TOUCH_DISPLAY_POINT, Some(42), true);
        d.drag_down(TOUCH_DISPLAY_POINT);
        d.drag_move(CGPoint::new(2550.0, 520.0));
        d.abort_gesture();
        let state = fx.lock();
        assert_eq!(state.posted.last().map(|event| event.event_type), Some(CGEventType::LeftMouseUp));
        assert_eq!(state.press, None);
        assert!(!state.hidden);
        assert_eq!(state.cursor, CGPoint::new(100.0, 100.0));
    }

    #[test]
    fn abort_during_a_direct_scroll_ends_the_scroll_phase() {
        let (mut d, fx) = make();
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(0.0, 5.0, GesturePhase::Began);
        d.abort_gesture();
        let last = fx.lock().posted.last().copied();
        assert_eq!(last.map(|event| event.event_type), Some(CGEventType::ScrollWheel));
        assert_eq!(last.map(|event| event.scroll_phase), Some(4), "kCGScrollPhaseEnded");
    }

    #[test]
    fn abort_during_momentum_stops_it() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(0.0, 10.0, GesturePhase::Began);
        d.scroll_ended(0.0, 3000.0, now);
        d.abort_gesture();
        let count = fx.lock().posted.len();
        assert_eq!(
            fx.lock().posted.last().map(|event| event.momentum_phase),
            Some(3),
            "momentum end"
        );
        now += SLEEP_100_MS_NS;
        d.tick(now);
        assert_eq!(fx.lock().posted.len(), count, "no momentum events after the abort");
    }

    #[test]
    fn abort_during_a_pinch_ends_it() {
        let (mut d, fx) = make();
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), true);
        d.magnify(0.1, GesturePhase::Began, START_NS);
        d.abort_gesture();
        assert_eq!(fx.lock().posted.len(), 2, "began + ended");
    }

    #[test]
    fn abort_with_nothing_open_does_nothing() {
        let (mut d, fx) = make();
        d.abort_gesture();
        assert!(fx.lock().posted.is_empty());
        assert!(!fx.lock().hidden);
    }

    // Momentum plays one frame per 1/120 s while the cursor stays parked, and closes with the
    // momentum-end event and the cursor back once it has died out. The Swift timer drove this; a
    // tick drives it now.
    #[test]
    fn momentum_plays_one_frame_per_tick_interval_until_it_dies_out() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.begin(Target::Warp, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(0.0, 10.0, GesturePhase::Began);
        d.scroll_ended(0.0, 3000.0, now);
        assert_eq!(fx.lock().posted.last().unwrap().momentum_phase, 1, "the first frame goes out at once");
        assert!(fx.lock().hidden, "momentum keeps the cursor parked");
        now += FRAME_NS - 1_000_000;
        d.tick(now);
        assert_eq!(d.next_deadline_ns(), Some(START_NS + FRAME_NS));
        now = START_NS + FRAME_NS;
        let before = fx.lock().posted.len();
        d.tick(now);
        {
            let state = fx.lock();
            assert_eq!(state.posted.len(), before + 1);
            assert_eq!(state.posted.last().unwrap().momentum_phase, 2, "kCGMomentumPhaseContinue");
            assert!(state.hidden);
        }
        // A tick that arrives several frames late plays one frame, not all it missed.
        let before = fx.lock().posted.len();
        now += 5 * FRAME_NS;
        d.tick(now);
        assert_eq!(fx.lock().posted.len(), before + 1);
        assert!(d.next_deadline_ns().unwrap() > now);
        // Let it die out; the cursor goes back 50 ms after the last frame.
        for _ in 0..600 {
            now += FRAME_NS;
            d.tick(now);
        }
        let state = fx.lock();
        assert_eq!(state.posted.last().unwrap().momentum_phase, 3, "the momentum-end event closes it");
        assert!(!state.hidden, "the cursor is back");
        assert_eq!(state.cursor, CGPoint::new(100.0, 100.0));
        assert!(d.next_deadline_ns().is_none(), "nothing is left pending");
    }

    // "Return focus to previous app": the app that was frontmost before a Direct click activated
    // another one gets focus back 0.6 s after the gesture ends, later than a double-click.
    #[test]
    fn a_direct_click_hands_focus_back_to_the_previous_app_after_the_delay() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.focus_mode = FocusMode::ReturnToPrevious;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), true);
        d.click(TOUCH_DISPLAY_POINT, 1, CGMouseButton::Left);
        d.end(now, WARP_RETURN_DELAY_NS);
        assert_eq!(fx.lock().activated, [42], "the touched app is activated first");
        // 0.6 s, longer than a double-click: written out so the test does not follow a changed constant.
        now += 500_000_000;
        d.tick(now);
        assert_eq!(fx.lock().activated, [42], "not before the delay");
        now = START_NS + 700_000_000;
        d.tick(now);
        assert_eq!(fx.lock().activated, [42, 1], "the previous app is frontmost again");
    }

    // Focus follows the touch (the default): nothing is activated behind the user's back.
    #[test]
    fn with_focus_following_the_touch_nothing_is_handed_back() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), true);
        d.click(TOUCH_DISPLAY_POINT, 1, CGMouseButton::Left);
        d.end(now, WARP_RETURN_DELAY_NS);
        now += 2_000_000_000;
        d.tick(now);
        assert_eq!(fx.lock().activated, [42]);
        assert!(d.next_deadline_ns().is_none());
    }

    // Trackpad setting "momentum scrolling" off: a flick ends without momentum.
    #[test]
    fn with_momentum_off_a_flick_ends_without_momentum() {
        let (mut d, fx) = make();
        let mut now = START_NS;
        d.momentum_enabled = false;
        d.begin(DIRECT, TOUCH_DISPLAY_POINT, Some(42), false);
        d.scroll(0.0, 10.0, GesturePhase::Began);
        d.scroll_ended(0.0, 3000.0, now);
        now += SLEEP_100_MS_NS / 2;
        d.tick(now);
        assert!(fx.lock().posted.iter().all(|event| event.momentum_phase == 0));
        assert_eq!(fx.lock().posted.last().map(|event| event.scroll_phase), Some(4));
    }

    /// The label as the Swift app's log has it on this machine, so the lines that name a target
    /// stay comparable across the two.
    #[test]
    fn a_target_is_labelled_the_way_the_swift_log_printed_it() {
        assert_eq!(target_label(&Target::Warp), "warp");
        let direct = DirectTarget { pid: 42, window: 7, origin: CGPoint::new(2000.0, 100.0) };
        assert_eq!(
            target_label(&Target::Direct(direct)),
            "direct(TouchPlatform.DirectTarget(pid: 42, window: 7, origin: (2000.0, 100.0)))"
        );
    }
}
