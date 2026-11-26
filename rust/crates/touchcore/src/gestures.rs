//! Gesture recognition: turns contact events into taps, right clicks, drags, scrolls,
//! magnification, rotation, the system dock swipes, the Notification Center edge swipe and
//! drawing strokes. Ported from `TouchCore/GestureRecognizer.swift`, which stays the
//! specification for every state, threshold and transition.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use smallvec::{SmallVec, smallvec};

use crate::tracker::{ContactEvent, ContactPhase};

/// A position on the panel in its native orientation, normalized to 0...1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelPoint {
    pub x: f64,
    pub y: f64,
}

impl PanelPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// The phase of a gesture stream that spans several reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GesturePhase {
    Began,
    Changed,
    Ended,
}

/// The trackpad's system "dock swipes".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockSwipeKind {
    /// Four fingers left/right: Spaces and full-screen apps.
    Horizontal,
    /// Four fingers up/down: Mission Control / App Exposé.
    Vertical,
    /// Thumb and three or four fingers pinching in/out: Launchpad / Show Desktop.
    Pinch,
}

/// What the user did, in panel coordinates. Delivery maps points and deltas onto the display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureAction {
    Tap { at: PanelPoint, count: u32 },
    RightClick { at: PanelPoint },
    /// `hold` is true when the drag must go through the window server (Warp delivery), with the
    /// mouse button pressed system-wide: drag-and-drop after a hold, and drawing strokes.
    DragBegan { at: PanelPoint, hold: bool },
    DragMoved { to: PanelPoint },
    DragEnded { at: PanelPoint },
    /// Finger movement in normalized panel units; `Began` carries the movement since touch-down.
    Scroll { dx: f64, dy: f64, phase: GesturePhase },
    /// Lift velocity in normalized panel units per second, for momentum.
    ScrollEnded { velocity_x: f64, velocity_y: f64 },
    /// Incremental magnification (0.1 = 10 % larger); `Began` carries the change since touch-down.
    Magnify { delta: f64, phase: GesturePhase },
    /// Incremental rotation in degrees, positive = counterclockwise as seen on screen.
    Rotate { degrees: f64, phase: GesturePhase },
    /// System gesture progress (1.0 is about a full swipe). Positive = Mission Control / Space on
    /// the right / Launchpad; negative = App Exposé / Space on the left / Show Desktop. `Began`
    /// carries the progress since touch-down; `Ended` lets the system complete or snap back.
    DockSwipe { kind: DockSwipeKind, delta: f64, phase: GesturePhase },
    /// A palm landed mid-swipe: snap the system gesture back.
    DockSwipeCancelled { kind: DockSwipeKind },
    /// Two fingers swiped in from the right edge.
    NotificationCenter,
}

/// What a one-finger drag does (menu switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OneFingerDragMode {
    /// Touch style: drag scrolls; press-and-hold then drag is click-and-drag.
    Scroll,
    /// Mouse style: drag is click-and-drag; two fingers scroll.
    #[default]
    ClickAndDrag,
}

impl OneFingerDragMode {
    /// The Swift raw value, which is how the setting is stored.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Scroll => "scroll",
            Self::ClickAndDrag => "clickAndDrag",
        }
    }
}

impl FromStr for OneFingerDragMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scroll" => Ok(Self::Scroll),
            "clickAndDrag" => Ok(Self::ClickAndDrag),
            other => Err(other.to_string()),
        }
    }
}

/// What touches do (menu "Touch Mode", per session).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    /// Taps, scrolling, pinch, system gestures and the rest (the default).
    #[default]
    Gestures,
    /// One finger is a pen: mouse down at touch-down, drag, up at lift. Other fingers and gestures
    /// are ignored.
    Drawing,
    /// Every finger goes out as TUIO for multitouch apps; nothing is delivered to macOS.
    Multitouch,
}

impl InputMode {
    /// The Swift raw value, which is how the setting is stored.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Gestures => "gestures",
            Self::Drawing => "drawing",
            Self::Multitouch => "multitouch",
        }
    }
}

impl FromStr for InputMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "gestures" => Ok(Self::Gestures),
            "drawing" => Ok(Self::Drawing),
            "multitouch" => Ok(Self::Multitouch),
            other => Err(other.to_string()),
        }
    }
}

/// Where a three-finger drag grabs (menu setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreeFingerAnchor {
    /// The middle of the three fingers.
    #[default]
    Centroid,
    /// Under the first finger that landed.
    FirstFinger,
}

impl ThreeFingerAnchor {
    /// The Swift raw value, which is how the setting is stored.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Centroid => "centroid",
            Self::FirstFinger => "firstFinger",
        }
    }
}

impl FromStr for ThreeFingerAnchor {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "centroid" => Ok(Self::Centroid),
            "firstFinger" => Ok(Self::FirstFinger),
            other => Err(other.to_string()),
        }
    }
}

/// Thresholds for gesture classification. Distances are millimetres on the panel.
#[derive(Debug, Clone, PartialEq)]
pub struct GestureConfig {
    pub one_finger_drag: OneFingerDragMode,
    /// Three fingers moving together = click-and-drag, like the trackpad's "Three-finger drag".
    pub three_finger_drag: bool,
    pub three_finger_anchor: ThreeFingerAnchor,
    /// Drawing mode (see `InputMode::Drawing`): the first finger draws, nothing else is recognized.
    pub drawing_mode: bool,
    /// The third finger must land within this time of the first.
    pub three_finger_window_ns: u64,
    /// "Rest a finger, add two": two fingers joining a still first finger within this time start a
    /// three-finger drag that grabs under the resting finger.
    pub resting_drag_window_ns: u64,
    /// Two-finger gestures that are switched on (trackpad settings).
    pub scroll_enabled: bool,
    pub pinch_enabled: bool,
    pub rotate_enabled: bool,
    /// System gestures that are switched on (trackpad settings).
    pub four_finger_vertical_swipe: bool,
    pub four_finger_horizontal_swipe: bool,
    pub four_finger_pinch: bool,
    pub notification_center_swipe: bool,
    /// Four or five fingers must all land within this time of the first.
    pub multi_finger_window_ns: u64,
    pub max_multi_contacts: usize,
    /// Travel before a four-finger gesture is classified, and how far a full swipe (progress 1.0) is.
    pub dock_swipe_decision_mm: f64,
    pub dock_swipe_mm_per_unit: f64,
    pub dock_pinch_mm_per_unit: f64,
    /// Two fingers starting this close to the panel's right edge and moving left this far open
    /// Notification Center.
    pub edge_zone_mm: f64,
    pub edge_swipe_mm: f64,
    /// The panel's active area, set by the pipeline from the panel profile; thresholds are in mm.
    pub panel_width_mm: f64,
    pub panel_height_mm: f64,
    /// Travel that still counts as a stationary finger: taps and long-presses are decided inside
    /// this radius (touch slop), so settling wobble on glass doesn't turn a hold into a drag.
    /// Classification of a scroll or drag needs travel beyond it.
    pub hold_slop_mm: f64,
    /// From 150 ms until the long-press time, the stationary radius grows with hold time at this
    /// rate: a finger planted to right-click has room to settle, and is given 16.75 mm by then.
    /// It stops there. Growing any longer, a drag at or below this rate never classified.
    pub hold_drift_mm_per_s: f64,
    pub double_tap_radius_mm: f64,
    pub double_tap_interval_ns: u64,
    pub long_press_ns: u64,
    pub two_finger_window_ns: u64,
    /// Finger travel before a two-finger gesture is classified.
    pub two_finger_decision_mm: f64,
    /// Fingers count as moving together when both moved at least this far, in directions this
    /// close (cosine).
    pub parallel_min_mm: f64,
    pub parallel_min_cosine: f64,
    /// Differential motion (fingers moving apart/together, or around each other) needed for
    /// pinch/rotate.
    pub pinch_min_mm: f64,
    pub rotate_min_mm: f64,
    pub max_contacts: usize,
    pub velocity_window_ns: u64,
    pub velocity_stale_ns: u64,
    /// When a drag's contact lifts, the press is held this long before it is committed, and a
    /// finger returning within a velocity-scaled radius resumes the drag seamlessly. Off by
    /// default (the Swift build held the press for 1.2 s): the tracker now carries a contact
    /// through the panel's tip flaps, so what reaches the recognizer as a lift is one, and the
    /// next touch is a new gesture. With the grace on, that next touch is lost instead: measured
    /// on 2026-11-25, a tap 1.2 s after a drag was taken for the drag resuming, and the window
    /// moved 68 pt.
    pub drag_grace_ns: u64,
    pub drag_resume_radius_mm: f64,
    /// How far the resume radius grows per second of finger speed at the lift.
    pub drag_resume_speed_gain: f64,
}

impl Default for GestureConfig {
    /// The Swift struct's default values, but for the drag grace, which is off.
    fn default() -> Self {
        Self {
            one_finger_drag: OneFingerDragMode::Scroll,
            three_finger_drag: true,
            three_finger_anchor: ThreeFingerAnchor::Centroid,
            drawing_mode: false,
            three_finger_window_ns: 250_000_000,
            resting_drag_window_ns: 1_500_000_000,
            scroll_enabled: true,
            pinch_enabled: true,
            rotate_enabled: true,
            four_finger_vertical_swipe: true,
            four_finger_horizontal_swipe: true,
            four_finger_pinch: true,
            notification_center_swipe: true,
            multi_finger_window_ns: 350_000_000,
            max_multi_contacts: 5,
            dock_swipe_decision_mm: 8.0,
            dock_swipe_mm_per_unit: 80.0,
            dock_pinch_mm_per_unit: 40.0,
            edge_zone_mm: 15.0,
            edge_swipe_mm: 10.0,
            panel_width_mm: 309.3,
            panel_height_mm: 173.9,
            hold_slop_mm: 8.0,
            hold_drift_mm_per_s: 25.0,
            double_tap_radius_mm: 8.0,
            double_tap_interval_ns: 500_000_000,
            long_press_ns: 500_000_000,
            two_finger_window_ns: 150_000_000,
            two_finger_decision_mm: 4.0,
            parallel_min_mm: 1.5,
            parallel_min_cosine: 0.5,
            pinch_min_mm: 2.0,
            rotate_min_mm: 2.0,
            max_contacts: 3,
            velocity_window_ns: 100_000_000,
            velocity_stale_ns: 50_000_000,
            drag_grace_ns: 0,
            drag_resume_radius_mm: 20.0,
            drag_resume_speed_gain: 0.75,
        }
    }
}

/// How long a touch is judged by the tap slop alone, before the hold's drift tolerance grows.
const TAP_SLOP_FLAT_NS: u64 = 150_000_000;

/// What a held press resumes as, after a flapped contact returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Drag,
    Pan,
}

/// Which two-finger gesture a two-finger stream is locked into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lock {
    Scroll,
    Pinch,
    Rotate,
    Edge,
}

/// Recent samples of a moving point, newest last.
type Samples = SmallVec<[(u64, PanelPoint); 8]>;

/// A movement or offset in millimetres on the panel.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MmVector {
    x: f64,
    y: f64,
}

#[derive(Debug)]
struct TwoFinger {
    ids: (u8, u8),
    /// When the first finger landed (a third finger must follow within `three_finger_window_ns`).
    first_down_ns: u64,
    start: BTreeMap<u8, PanelPoint>,
    positions: BTreeMap<u8, PanelPoint>,
    start_centroid: PanelPoint,
    start_distance: f64,
    start_angle: f64,
    lock: Option<Lock>,
    last_centroid: PanelPoint,
    last_distance: f64,
    last_angle: f64,
    samples: Samples,
}

#[derive(Debug)]
struct ThreeFinger {
    /// The first finger to land comes first.
    ids: SmallVec<[u8; 4]>,
    first_down_ns: u64,
    start: BTreeMap<u8, PanelPoint>,
    positions: BTreeMap<u8, PanelPoint>,
    dragging: bool,
    last_anchor: PanelPoint,
    /// Started by resting the first finger, then adding two: the drag grabs where that finger
    /// first touched, then follows its movement.
    resting_grab: Option<PanelPoint>,
}

#[derive(Debug)]
struct MultiFinger {
    ids: SmallVec<[u8; 5]>,
    first_down_ns: u64,
    start: BTreeMap<u8, PanelPoint>,
    positions: BTreeMap<u8, PanelPoint>,
    lock: Option<DockSwipeKind>,
    last_centroid_mm: MmVector,
    last_spread_mm: f64,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Idle,
    OneFinger {
        id: u8,
        down_ns: u64,
        origin: PanelPoint,
        current: PanelPoint,
        first_move_ns: Option<u64>,
    },
    Dragging {
        id: u8,
        last: PanelPoint,
        samples: Samples,
    },
    /// One-finger scroll (scroll mode).
    Panning {
        id: u8,
        last: PanelPoint,
        samples: Samples,
    },
    /// A drag whose contact lifted: the press is held until `until_ns`, resuming if a finger
    /// returns within a radius grown by the press's speed at the lift (a WCH dropout or
    /// contact flap), committed as a real end otherwise.
    DragGrace {
        kind: DragKind,
        last: PanelPoint,
        until_ns: u64,
        speed_mm_s: f64,
        samples: Samples,
    },
    TwoFinger(TwoFinger),
    ThreeFinger(ThreeFinger),
    MultiFinger(MultiFinger),
    /// Waits until every finger has lifted (after a two-finger gesture or a palm).
    Ignoring,
}

/// Classifies contact events into gestures. Nothing is emitted until a gesture is classified,
/// so delivery can pick Direct or Warp once per gesture.
#[derive(Default)]
pub struct GestureRecognizer {
    /// Thresholds and switches. In Swift these were reached through computed properties on the
    /// recognizer; callers set them here directly.
    pub config: GestureConfig,
    /// Whether a one-finger touch at a point is on a window's drag area (the pipeline asks the
    /// window list and Accessibility). Past the tap slop, such a touch drags the window.
    pub drag_zone: Option<Box<dyn Fn(PanelPoint) -> bool + Send>>,
    state: State,
    active: BTreeMap<u8, PanelPoint>,
    /// Fingers that joined a resting first finger after the two-finger window (see
    /// `resting_drag_window_ns`).
    resting_extras: BTreeMap<u8, PanelPoint>,
    last_tap: Option<(u64, PanelPoint, u32)>,
    /// Drawing mode: the finger that draws, and where it last was.
    pen: Option<(u8, PanelPoint)>,
    two_finger_moved: bool,
    three_finger_moved: bool,
    multi_finger_moved: bool,
}

impl fmt::Debug for GestureRecognizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GestureRecognizer")
            .field("config", &self.config)
            .field("state", &self.state)
            .field("active", &self.active)
            .field("resting_extras", &self.resting_extras)
            .field("last_tap", &self.last_tap)
            .field("pen", &self.pen)
            .finish_non_exhaustive()
    }
}

impl GestureRecognizer {
    pub fn new(config: GestureConfig) -> Self {
        Self {
            config,
            drag_zone: None,
            state: State::Idle,
            active: BTreeMap::new(),
            resting_extras: BTreeMap::new(),
            last_tap: None,
            pen: None,
            two_finger_moved: false,
            three_finger_moved: false,
            multi_finger_moved: false,
        }
    }

    /// Feed one frame's contact events in at time `at_ns` and get the actions they complete.
    /// `at_ns` drives every deadline (long press, tap interval, drag grace), so an empty
    /// `events` slice still advances the state machine.
    pub fn handle(&mut self, events: &[ContactEvent], at_ns: u64) -> SmallVec<[GestureAction; 8]> {
        if self.config.drawing_mode {
            return self.draw(events);
        }
        let mut actions: SmallVec<[GestureAction; 8]> = SmallVec::new();
        // A drag grace past its deadline commits the held press, even with no contact events to
        // carry it (the pipeline pumps this with an empty event list from its timer).
        if let State::DragGrace { kind, last, until_ns, samples, .. } = &self.state
            && at_ns >= *until_ns {
                let action = grace_end(&self.config, *kind, *last, samples, at_ns);
                self.state = State::Idle;
                actions.push(action);
            }
        self.two_finger_moved = false;
        self.three_finger_moved = false;
        self.multi_finger_moved = false;
        for event in events {
            let point = PanelPoint::new(event.x, event.y);
            match event.phase {
                ContactPhase::Began | ContactPhase::Moved => {
                    self.active.insert(event.id, point);
                }
                ContactPhase::Ended => {
                    self.active.remove(&event.id);
                }
            }
            if self.active.len() > self.contact_limit() {
                if event.phase == ContactPhase::Began
                    && let Some(multi) = self.start_multi_finger(event, point, at_ns)
                {
                    self.state = State::MultiFinger(multi);
                    continue;
                }
                actions.extend(self.cancel(at_ns));
                continue;
            }
            actions.extend(self.step(event, point, at_ns));
            if self.active.is_empty() && matches!(self.state, State::Ignoring) {
                self.state = State::Idle;
            }
        }
        // Both fingers of a frame move together; evaluate the two-finger gesture once per frame.
        // (Same for three fingers and for the dock swipe.)
        if self.two_finger_moved
            && let State::TwoFinger(g) = &mut self.state {
                actions.extend(update_two_finger(&self.config, g, at_ns));
            }
        if self.three_finger_moved
            && let State::ThreeFinger(t) = &mut self.state {
                actions.extend(update_three_finger(&self.config, t));
            }
        if self.multi_finger_moved
            && let State::MultiFinger(m) = &mut self.state {
                actions.extend(update_multi_finger(&self.config, m));
            }
        actions
    }

    /// Switch every two-finger and system gesture on (the trackpad's "reset to defaults").
    pub fn enable_all_two_finger_gestures(&mut self) {
        let c = &mut self.config;
        c.scroll_enabled = true;
        c.pinch_enabled = true;
        c.rotate_enabled = true;
        c.four_finger_vertical_swipe = true;
        c.four_finger_horizontal_swipe = true;
        c.four_finger_pinch = true;
        c.notification_center_swipe = true;
    }

    /// The earliest time at which the recognizer changes state with no further input: the end of a
    /// drag grace that is holding a press. `None` when only new events can move it, which lets the
    /// caller sleep until an event arrives or this time passes.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        match &self.state {
            State::DragGrace { until_ns, .. } => Some(*until_ns),
            _ => None,
        }
    }

    /// How far from where it landed a finger may be `held_ns` into the touch and still count as
    /// stationary: the tap slop for the first 150 ms, so a deliberate swipe classifies at once;
    /// growing with hold time until the long-press time, so a finger planted to right-click has
    /// room to settle; and no wider after that, so a slow drag, which keeps travelling, leaves it.
    fn stationary_radius_mm(&self, held_ns: u64) -> f64 {
        let growing_ns = held_ns
            .saturating_sub(TAP_SLOP_FLAT_NS)
            .min(self.config.long_press_ns.saturating_sub(TAP_SLOP_FLAT_NS));
        self.config.hold_slop_mm + growing_ns as f64 / 1e9 * self.config.hold_drift_mm_per_s
    }

    /// More fingers than a one-, two- or three-finger gesture allows; four or five may form a
    /// dock swipe.
    fn contact_limit(&self) -> usize {
        match self.state {
            State::MultiFinger(_) => self.config.max_multi_contacts,
            _ => self.config.max_contacts,
        }
    }

    fn step(&mut self, event: &ContactEvent, point: PanelPoint, now: u64) -> SmallVec<[GestureAction; 8]> {
        // The old state is taken out so arms can replace it freely; every arm either assigns the
        // successor or puts the old state back.
        match std::mem::replace(&mut self.state, State::Ignoring) {
            State::Idle => {
                if event.phase == ContactPhase::Began {
                    self.state = State::OneFinger {
                        id: event.id,
                        down_ns: now,
                        origin: point,
                        current: point,
                        first_move_ns: None,
                    };
                    self.resting_extras.clear();
                } else {
                    self.state = State::Idle;
                }
                smallvec![]
            }

            State::OneFinger { id, down_ns, origin, current, first_move_ns } => {
                if event.id != id {
                    // The first finger is still (it would have left this state after moving past
                    // the tap slop).
                    match event.phase {
                        ContactPhase::Began if now - down_ns <= self.config.two_finger_window_ns => {
                            self.resting_extras.clear();
                            self.state =
                                State::TwoFinger(start_two_finger(&self.config, id, current, event.id, point, now, down_ns));
                            return smallvec![];
                        }
                        ContactPhase::Began
                            if self.config.three_finger_drag
                                && now - down_ns <= self.config.resting_drag_window_ns =>
                        {
                            self.resting_extras.insert(event.id, point);
                            self.state = State::OneFinger { id, down_ns, origin, current, first_move_ns };
                        }
                        ContactPhase::Moved if self.resting_extras.contains_key(&event.id) => {
                            self.resting_extras.insert(event.id, point);
                            self.state = State::OneFinger { id, down_ns, origin, current, first_move_ns };
                        }
                        ContactPhase::Ended => {
                            self.resting_extras.remove(&event.id);
                            self.state = State::OneFinger { id, down_ns, origin, current, first_move_ns };
                            return smallvec![];
                        }
                        _ => {
                            // One late finger alone is ignored, as before.
                            self.state = State::OneFinger { id, down_ns, origin, current, first_move_ns };
                            return smallvec![];
                        }
                    }
                    if self.resting_extras.len() != 2 {
                        return smallvec![];
                    }
                    // Rest a finger, add two: a three-finger drag that grabs under the resting finger.
                    let mut start = self.resting_extras.clone();
                    start.insert(id, current);
                    let mut ids: SmallVec<[u8; 4]> = smallvec![id];
                    ids.extend(self.resting_extras.keys().copied());
                    self.resting_extras.clear();
                    self.state = State::ThreeFinger(ThreeFinger {
                        ids,
                        first_down_ns: down_ns,
                        start: start.clone(),
                        positions: start,
                        dragging: false,
                        last_anchor: origin,
                        resting_grab: Some(origin),
                    });
                    return smallvec![];
                }
                let held_ns = now.saturating_sub(down_ns);
                let travel_mm = distance_mm(&self.config, point, origin);
                // Past the tap slop, a finger that landed on a window's drag area is dragging that
                // window, however slowly: the growing radius is for a finger held to right-click.
                let window_drag = travel_mm > self.config.hold_slop_mm
                    && self.drag_zone.as_ref().is_some_and(|zone| zone(origin));
                match event.phase {
                    ContactPhase::Moved
                        if window_drag || travel_mm > self.stationary_radius_mm(held_ns) =>
                    {
                        // A click-and-drag needs a still finger first: the linger before the first
                        // real movement separates hold-drag from a slow natural swipe, which scrolls.
                        let first_move = first_move_ns.unwrap_or(now);
                        let hold = first_move - down_ns >= self.config.long_press_ns;
                        if !hold
                            && self.config.one_finger_drag == OneFingerDragMode::Scroll
                            && !window_drag
                        {
                            self.state =
                                State::Panning { id, last: point, samples: smallvec![(down_ns, origin), (now, point)] };
                            return smallvec![GestureAction::Scroll {
                                dx: point.x - origin.x,
                                dy: point.y - origin.y,
                                phase: GesturePhase::Began,
                            }];
                        }
                        // A window drag is pressed where the finger is by now, for as long as that
                        // is still a drag area: the drag is recognised only after some travel, and
                        // the window would jump that far to catch up with the finger.
                        let at = if window_drag && self.drag_zone.as_ref().is_some_and(|zone| zone(point)) {
                            point
                        } else {
                            origin
                        };
                        self.state =
                            State::Dragging { id, last: point, samples: smallvec![(down_ns, origin), (now, point)] };
                        let mut actions = smallvec![GestureAction::DragBegan { at, hold }];
                        if point != at {
                            actions.push(GestureAction::DragMoved { to: point });
                        }
                        actions
                    }
                    ContactPhase::Moved | ContactPhase::Began => {
                        let first_move =
                            first_move_ns.or_else(|| if distance_mm(&self.config, point, origin) > 1.0 { Some(now) } else { None });
                        self.state =
                            State::OneFinger { id, down_ns, origin, current: point, first_move_ns: first_move };
                        smallvec![]
                    }
                    ContactPhase::Ended => {
                        // Taps and long-presses land where the finger rested, not where it first
                        // touched: touchdown wobbles on glass, the resting point is what the user
                        // aimed. And they are judged at the moment the finger was last seen: the
                        // tracker holds a lift report for as much as 250 ms while the panel
                        // chatters, and that time is no finger's (owner's recording 2026-11-26,
                        // 10.954 s: a tap of 233 ms, so held, was a long press).
                        self.state = if self.active.is_empty() { State::Idle } else { State::Ignoring };
                        if event.at_ns.saturating_sub(down_ns) >= self.config.long_press_ns {
                            smallvec![GestureAction::RightClick { at: point }]
                        } else {
                            smallvec![self.tap(point, event.at_ns)]
                        }
                    }
                }
            }

            State::Dragging { id, last, samples } => {
                if event.id != id {
                    self.state = State::Dragging { id, last, samples };
                    return smallvec![];
                }
                match event.phase {
                    ContactPhase::Moved => {
                        let kept: Samples = samples
                            .into_iter()
                            .filter(|&(ns, _)| now - ns <= self.config.velocity_window_ns)
                            .collect();
                        let mut kept = kept;
                        kept.push((now, point));
                        self.state = State::Dragging { id, last: point, samples: kept };
                        smallvec![GestureAction::DragMoved { to: point }]
                    }
                    ContactPhase::Ended if self.config.drag_grace_ns > 0 => {
                        self.state = State::DragGrace {
                            kind: DragKind::Drag,
                            last: point,
                            until_ns: now + self.config.drag_grace_ns,
                            speed_mm_s: speed_mm_s(&self.config, &samples, now),
                            samples,
                        };
                        smallvec![]
                    }
                    ContactPhase::Ended => {
                        self.state = if self.active.is_empty() { State::Idle } else { State::Ignoring };
                        smallvec![GestureAction::DragEnded { at: point }]
                    }
                    ContactPhase::Began => {
                        self.state = State::Dragging { id, last, samples };
                        smallvec![GestureAction::DragEnded { at: last }]
                    }
                }
            }

            State::Panning { id, last, samples } => {
                if event.id != id {
                    self.state = State::Panning { id, last, samples };
                    return smallvec![];
                }
                match event.phase {
                    ContactPhase::Moved => {
                        let mut kept: Samples = samples
                            .into_iter()
                            .filter(|&(ns, _)| now - ns <= self.config.velocity_window_ns)
                            .collect();
                        kept.push((now, point));
                        self.state = State::Panning { id, last: point, samples: kept };
                        smallvec![GestureAction::Scroll {
                            dx: point.x - last.x,
                            dy: point.y - last.y,
                            phase: GesturePhase::Changed,
                        }]
                    }
                    ContactPhase::Ended if self.config.drag_grace_ns > 0 => {
                        self.state = State::DragGrace {
                            kind: DragKind::Pan,
                            last: point,
                            until_ns: now + self.config.drag_grace_ns,
                            speed_mm_s: speed_mm_s(&self.config, &samples, now),
                            samples,
                        };
                        smallvec![]
                    }
                    ContactPhase::Ended => {
                        self.state = if self.active.is_empty() { State::Idle } else { State::Ignoring };
                        let (velocity_x, velocity_y) = velocity(&self.config, &samples, now);
                        smallvec![GestureAction::ScrollEnded { velocity_x, velocity_y }]
                    }
                    ContactPhase::Began => {
                        self.state = State::Panning { id, last, samples };
                        smallvec![]
                    }
                }
            }

            State::DragGrace { kind, last, until_ns, speed_mm_s, samples } => {
                // The resume radius grows with how fast the finger was moving and how long it was
                // gone: a dropout during a slow drag resumes tens of millimetres along its way,
                // while a tap near the release point stays outside it.
                let resume_radius_mm = self.config.drag_resume_radius_mm
                    + speed_mm_s * (now - (until_ns - self.config.drag_grace_ns)) as f64 / 1_000_000_000.0
                        * self.config.drag_resume_speed_gain;
                match event.phase {
                    ContactPhase::Began
                        if now < until_ns && distance_mm(&self.config, point, last) <= resume_radius_mm =>
                    {
                        // The finger came back (a dropout or contact flap): the press never
                        // happened to end.
                        match kind {
                            DragKind::Drag => {
                                self.state =
                                    State::Dragging { id: event.id, last: point, samples: smallvec![(now, point)] };
                                if point != last {
                                    smallvec![GestureAction::DragMoved { to: point }]
                                } else {
                                    smallvec![]
                                }
                            }
                            DragKind::Pan => {
                                let mut kept: Samples = samples
                                    .into_iter()
                                    .filter(|&(ns, _)| now - ns <= self.config.velocity_window_ns)
                                    .collect();
                                kept.push((now, point));
                                self.state = State::Panning { id: event.id, last: point, samples: kept };
                                smallvec![GestureAction::Scroll {
                                    dx: point.x - last.x,
                                    dy: point.y - last.y,
                                    phase: GesturePhase::Changed,
                                }]
                            }
                        }
                    }
                    ContactPhase::Began => {
                        // A touch somewhere else: the held press ends, the new touch starts fresh.
                        // (The state is still a drag grace here, so Swift's contact limit resolves
                        // to max_contacts.)
                        let action = grace_end(&self.config, kind, last, &samples, now);
                        self.state = if self.active.len() > self.config.max_contacts {
                            State::Ignoring
                        } else {
                            State::OneFinger {
                                id: event.id,
                                down_ns: now,
                                origin: point,
                                current: point,
                                first_move_ns: None,
                            }
                        };
                        smallvec![action]
                    }
                    ContactPhase::Moved | ContactPhase::Ended => {
                        self.state = State::DragGrace { kind, last, until_ns, speed_mm_s, samples };
                        smallvec![]
                    }
                }
            }

            State::TwoFinger(mut g) => {
                if event.phase == ContactPhase::Began
                    && event.id != g.ids.0
                    && event.id != g.ids.1
                    && g.lock.is_none()
                    && self.config.three_finger_drag
                    && now - g.first_down_ns <= self.config.three_finger_window_ns
                    && let (Some(&a), Some(&b)) = (g.positions.get(&g.ids.0), g.positions.get(&g.ids.1))
                {
                    let start = BTreeMap::from([(g.ids.0, a), (g.ids.1, b), (event.id, point)]);
                    self.state = State::ThreeFinger(ThreeFinger {
                        ids: smallvec![g.ids.0, g.ids.1, event.id],
                        first_down_ns: g.first_down_ns,
                        start: start.clone(),
                        positions: start,
                        dragging: false,
                        last_anchor: point,
                        resting_grab: None,
                    });
                    return smallvec![];
                }
                if event.id != g.ids.0 && event.id != g.ids.1 {
                    self.state = State::TwoFinger(g);
                    return smallvec![];
                }
                if event.phase == ContactPhase::Ended {
                    self.state = if self.active.is_empty() { State::Idle } else { State::Ignoring };
                    return end_two_finger(&self.config, &g, now);
                }
                g.positions.insert(event.id, point);
                self.state = State::TwoFinger(g);
                self.two_finger_moved = true;
                smallvec![]
            }

            State::ThreeFinger(mut t) => {
                if !t.ids.contains(&event.id) {
                    self.state = State::ThreeFinger(t);
                    return smallvec![];
                }
                if event.phase == ContactPhase::Ended {
                    self.state = if self.active.is_empty() { State::Idle } else { State::Ignoring };
                    return if t.dragging {
                        smallvec![GestureAction::DragEnded { at: t.last_anchor }]
                    } else {
                        smallvec![]
                    };
                }
                t.positions.insert(event.id, point);
                self.state = State::ThreeFinger(t);
                self.three_finger_moved = true;
                smallvec![]
            }

            State::MultiFinger(mut m) => {
                if event.phase == ContactPhase::Began {
                    // A fifth finger may still join while nothing has been classified.
                    if m.lock.is_none()
                        && !m.ids.contains(&event.id)
                        && now - m.first_down_ns <= self.config.multi_finger_window_ns
                    {
                        m.ids.push(event.id);
                        m.start.insert(event.id, point);
                        m.positions.insert(event.id, point);
                    }
                    self.state = State::MultiFinger(m);
                    return smallvec![];
                }
                if !m.ids.contains(&event.id) {
                    self.state = State::MultiFinger(m);
                    return smallvec![];
                }
                if event.phase == ContactPhase::Ended {
                    self.state = if self.active.is_empty() { State::Idle } else { State::Ignoring };
                    return match m.lock {
                        Some(kind) => smallvec![GestureAction::DockSwipe {
                            kind,
                            delta: 0.0,
                            phase: GesturePhase::Ended,
                        }],
                        None => smallvec![],
                    };
                }
                m.positions.insert(event.id, point);
                self.state = State::MultiFinger(m);
                self.multi_finger_moved = true;
                smallvec![]
            }

            State::Ignoring => smallvec![],
        }
    }

    // Drawing mode

    /// The first finger to land is a pen: the stroke starts at touch-down (so a tap is a dot) and
    /// ends at lift. Every other finger, such as a palm resting on the screen, is ignored, and so
    /// is every gesture. When the pen lifts, the next finger to land draws.
    fn draw(&mut self, events: &[ContactEvent]) -> SmallVec<[GestureAction; 8]> {
        let mut actions: SmallVec<[GestureAction; 8]> = SmallVec::new();
        for event in events {
            let point = PanelPoint::new(event.x, event.y);
            match event.phase {
                ContactPhase::Began if self.pen.is_none() => {
                    self.pen = Some((event.id, point));
                    // Through the window server, like a real mouse: canvases check that a button
                    // is really down.
                    actions.push(GestureAction::DragBegan { at: point, hold: true });
                }
                ContactPhase::Moved if matches!(self.pen, Some((id, _)) if id == event.id) => {
                    self.pen = Some((event.id, point));
                    actions.push(GestureAction::DragMoved { to: point });
                }
                ContactPhase::Ended if matches!(self.pen, Some((id, _)) if id == event.id) => {
                    self.pen = None;
                    actions.push(GestureAction::DragEnded { at: point });
                }
                _ => {}
            }
        }
        actions
    }

    // Four and five fingers (system dock swipes)

    /// A fourth finger joins three that landed together and haven't started a drag.
    fn start_multi_finger(&self, event: &ContactEvent, point: PanelPoint, now: u64) -> Option<MultiFinger> {
        let State::ThreeFinger(t) = &self.state else { return None };
        if t.dragging
            || self.active.len() != self.config.max_contacts + 1
            || now - t.first_down_ns > self.config.multi_finger_window_ns
        {
            return None;
        }
        let mut start = t.positions.clone();
        start.insert(event.id, point);
        let mut ids: SmallVec<[u8; 5]> = t.ids.iter().copied().collect();
        ids.push(event.id);
        let last_centroid_mm = centroid_mm(&self.config, &start);
        let last_spread_mm = spread_mm(&self.config, &start);
        Some(MultiFinger {
            ids,
            first_down_ns: t.first_down_ns,
            start: start.clone(),
            positions: start,
            lock: None,
            last_centroid_mm,
            last_spread_mm,
        })
    }

    // Taps

    /// A lift near a recent tap within the double-tap interval counts up and snaps to that tap's
    /// position; anything farther or later starts a new sequence.
    fn tap(&mut self, at: PanelPoint, now: u64) -> GestureAction {
        if let Some((last_ns, last_at, last_count)) = self.last_tap
            && now - last_ns <= self.config.double_tap_interval_ns
            && distance_mm(&self.config, at, last_at) <= self.config.double_tap_radius_mm
        {
            self.last_tap = Some((now, last_at, last_count + 1));
            return GestureAction::Tap { at: last_at, count: last_count + 1 };
        }
        self.last_tap = Some((now, at, 1));
        GestureAction::Tap { at, count: 1 }
    }

    // Palm

    /// The action that commits a held press whose grace ran out (or was cut short).
    fn cancel(&mut self, now: u64) -> SmallVec<[GestureAction; 8]> {
        let old = std::mem::replace(&mut self.state, State::Ignoring);
        let actions = match &old {
            State::Dragging { last, .. } => smallvec![GestureAction::DragEnded { at: *last }],
            State::DragGrace { kind, last, samples, .. } => {
                smallvec![grace_end(&self.config, *kind, *last, samples, now)]
            }
            State::ThreeFinger(t) if t.dragging => smallvec![GestureAction::DragEnded { at: t.last_anchor }],
            State::MultiFinger(m) => match m.lock {
                Some(kind) => smallvec![GestureAction::DockSwipeCancelled { kind }],
                None => smallvec![],
            },
            State::Panning { .. } => smallvec![GestureAction::ScrollEnded { velocity_x: 0.0, velocity_y: 0.0 }],
            State::TwoFinger(g) => match g.lock {
                Some(Lock::Scroll) => smallvec![GestureAction::ScrollEnded { velocity_x: 0.0, velocity_y: 0.0 }],
                Some(Lock::Pinch) => smallvec![GestureAction::Magnify { delta: 0.0, phase: GesturePhase::Ended }],
                Some(Lock::Rotate) => smallvec![GestureAction::Rotate { degrees: 0.0, phase: GesturePhase::Ended }],
                _ => smallvec![],
            },
            _ => smallvec![],
        };
        self.resting_extras.clear();
        actions
    }
}

fn start_two_finger(
    config: &GestureConfig,
    a: u8,
    pa: PanelPoint,
    b: u8,
    pb: PanelPoint,
    now: u64,
    first_down_ns: u64,
) -> TwoFinger {
    let centroid = midpoint(pa, pb);
    let distance = distance_mm(config, pa, pb);
    let angle = angle_degrees(config, pa, pb);
    TwoFinger {
        ids: (a, b),
        first_down_ns,
        start: BTreeMap::from([(a, pa), (b, pb)]),
        positions: BTreeMap::from([(a, pa), (b, pb)]),
        start_centroid: centroid,
        start_distance: distance,
        start_angle: angle,
        lock: None,
        last_centroid: centroid,
        last_distance: distance,
        last_angle: angle,
        samples: smallvec![(now, centroid)],
    }
}

fn update_two_finger(config: &GestureConfig, g: &mut TwoFinger, now: u64) -> SmallVec<[GestureAction; 8]> {
    let (Some(&pa), Some(&pb)) = (g.positions.get(&g.ids.0), g.positions.get(&g.ids.1)) else {
        return smallvec![];
    };
    let centroid = midpoint(pa, pb);
    let distance = distance_mm(config, pa, pb);
    let angle = angle_degrees(config, pa, pb);
    g.samples.push((now, centroid));
    g.samples.retain(|(ns, _)| now - *ns <= config.velocity_window_ns);
    let lock = g.lock;
    let classified = if lock.is_none() { classify(config, g, pa, pb) } else { None };

    let actions = match lock {
        None => match classified {
            None => smallvec![],
            Some(lock) => {
                g.lock = Some(lock);
                match lock {
                    Lock::Edge => smallvec![GestureAction::NotificationCenter],
                    Lock::Scroll => smallvec![GestureAction::Scroll {
                        dx: centroid.x - g.start_centroid.x,
                        dy: centroid.y - g.start_centroid.y,
                        phase: GesturePhase::Began,
                    }],
                    Lock::Pinch => {
                        if g.start_distance > 0.0 {
                            smallvec![GestureAction::Magnify {
                                delta: distance / g.start_distance - 1.0,
                                phase: GesturePhase::Began,
                            }]
                        } else {
                            smallvec![]
                        }
                    }
                    Lock::Rotate => smallvec![GestureAction::Rotate {
                        degrees: counterclockwise(g.start_angle, angle),
                        phase: GesturePhase::Began,
                    }],
                }
            }
        },
        Some(Lock::Scroll) => smallvec![GestureAction::Scroll {
            dx: centroid.x - g.last_centroid.x,
            dy: centroid.y - g.last_centroid.y,
            phase: GesturePhase::Changed,
        }],
        Some(Lock::Pinch) => {
            if g.last_distance > 0.0 {
                smallvec![GestureAction::Magnify {
                    delta: distance / g.last_distance - 1.0,
                    phase: GesturePhase::Changed,
                }]
            } else {
                smallvec![]
            }
        }
        Some(Lock::Rotate) => smallvec![GestureAction::Rotate {
            degrees: counterclockwise(g.last_angle, angle),
            phase: GesturePhase::Changed,
        }],
        Some(Lock::Edge) => smallvec![],
    };
    g.last_centroid = centroid;
    g.last_distance = distance;
    g.last_angle = angle;
    actions
}

fn end_two_finger(config: &GestureConfig, g: &TwoFinger, now: u64) -> SmallVec<[GestureAction; 8]> {
    match g.lock {
        None | Some(Lock::Edge) => smallvec![],
        Some(Lock::Pinch) => smallvec![GestureAction::Magnify { delta: 0.0, phase: GesturePhase::Ended }],
        Some(Lock::Rotate) => smallvec![GestureAction::Rotate { degrees: 0.0, phase: GesturePhase::Ended }],
        Some(Lock::Scroll) => {
            let (velocity_x, velocity_y) = velocity(config, &g.samples, now);
            smallvec![GestureAction::ScrollEnded { velocity_x, velocity_y }]
        }
    }
}

/// Classifies by how the fingers move (like the trackpad): both the same way gives scroll,
/// toward/away from each other gives pinch, around each other gives rotate. Nothing until clear.
fn classify(config: &GestureConfig, g: &TwoFinger, pa: PanelPoint, pb: PanelPoint) -> Option<Lock> {
    let (Some(&sa), Some(&sb)) = (g.start.get(&g.ids.0), g.start.get(&g.ids.1)) else {
        return None;
    };
    let da = mm_vector(config, sa, pa);
    let db = mm_vector(config, sb, pb);
    let ma = f64::hypot(da.x, da.y);
    let mb = f64::hypot(db.x, db.y);
    if f64::max(ma, mb) <= config.two_finger_decision_mm {
        return None;
    }

    let axis = mm_vector(config, sb, sa);
    let length = f64::hypot(axis.x, axis.y);
    if length <= 0.0 {
        return Some(Lock::Scroll);
    }
    let ux = axis.x / length;
    let uy = axis.y / length;
    let common = MmVector { x: (da.x + db.x) / 2.0, y: (da.y + db.y) / 2.0 };
    let differential = MmVector { x: (da.x - db.x) / 2.0, y: (da.y - db.y) / 2.0 };
    let radial = f64::abs(differential.x * ux + differential.y * uy);
    let tangential = f64::abs(differential.x * uy - differential.y * ux);

    let together = f64::min(ma, mb) >= config.parallel_min_mm
        && (da.x * db.x + da.y * db.y) / (ma * mb) > config.parallel_min_cosine;
    // Two fingers starting near the right edge and moving mostly left: Notification Center. The
    // fingers may move unevenly (owner report: stacked fingers were taken for rotate); waits for
    // enough travel.
    if config.notification_center_swipe
        && common.x < 0.0
        && f64::abs(common.x) > 2.0 * f64::abs(common.y)
        && (1.0 - (sa.x + sb.x) / 2.0) * config.panel_width_mm <= config.edge_zone_mm
    {
        return if f64::abs(common.x) > config.edge_swipe_mm { Some(Lock::Edge) } else { None };
    }
    if together && f64::hypot(common.x, common.y) > f64::max(radial, tangential) {
        return if config.scroll_enabled { Some(Lock::Scroll) } else { None };
    }
    if radial >= tangential {
        return if radial >= config.pinch_min_mm && config.pinch_enabled {
            Some(Lock::Pinch)
        } else {
            None
        };
    }
    if tangential >= config.rotate_min_mm && config.rotate_enabled {
        Some(Lock::Rotate)
    } else {
        None
    }
}

/// Three fingers moving together drag (like the trackpad setting), from their centre or the
/// first finger.
fn update_three_finger(config: &GestureConfig, t: &mut ThreeFinger) -> SmallVec<[GestureAction; 8]> {
    let anchor = anchor_point(config, &t.positions, t);
    if !t.dragging {
        if distance_mm(config, centroid(&t.positions), centroid(&t.start)) <= config.two_finger_decision_mm {
            return smallvec![];
        }
        t.dragging = true;
        t.last_anchor = anchor;
        return smallvec![
            GestureAction::DragBegan { at: anchor_point(config, &t.start, t), hold: true },
            GestureAction::DragMoved { to: anchor },
        ];
    }
    if anchor == t.last_anchor {
        return smallvec![];
    }
    t.last_anchor = anchor;
    smallvec![GestureAction::DragMoved { to: anchor }]
}

fn anchor_point(config: &GestureConfig, points: &BTreeMap<u8, PanelPoint>, t: &ThreeFinger) -> PanelPoint {
    if let Some(grab) = t.resting_grab
        && let (Some(&p), Some(&p0)) = (points.get(&t.ids[0]), t.start.get(&t.ids[0]))
    {
        // Where the resting finger first touched (it may roll a little as the others land), moved
        // as far as that finger has moved since.
        return PanelPoint::new(grab.x + p.x - p0.x, grab.y + p.y - p0.y);
    }
    if config.three_finger_anchor == ThreeFingerAnchor::FirstFinger {
        return points.get(&t.ids[0]).copied().unwrap_or_else(|| centroid(points));
    }
    centroid(points)
}

fn update_multi_finger(config: &GestureConfig, m: &mut MultiFinger) -> SmallVec<[GestureAction; 8]> {
    let points: BTreeMap<u8, PanelPoint> =
        m.positions.iter().filter(|(id, _)| m.ids.contains(id)).map(|(id, p)| (*id, *p)).collect();
    let centroid = centroid_mm(config, &points);
    let spread = spread_mm(config, &points);

    let actions = match m.lock {
        Some(kind) => {
            let delta = match kind {
                DockSwipeKind::Vertical => -(centroid.y - m.last_centroid_mm.y) / config.dock_swipe_mm_per_unit,
                DockSwipeKind::Horizontal => -(centroid.x - m.last_centroid_mm.x) / config.dock_swipe_mm_per_unit,
                DockSwipeKind::Pinch => -(spread - m.last_spread_mm) / config.dock_pinch_mm_per_unit,
            };
            if delta == 0.0 {
                smallvec![]
            } else {
                smallvec![GestureAction::DockSwipe { kind, delta, phase: GesturePhase::Changed }]
            }
        }
        None => {
            let start: BTreeMap<u8, PanelPoint> =
                m.start.iter().filter(|(id, _)| m.ids.contains(id)).map(|(id, p)| (*id, *p)).collect();
            let c0 = centroid_mm(config, &start);
            let s0 = spread_mm(config, &start);
            let shift = MmVector { x: centroid.x - c0.x, y: centroid.y - c0.y };
            let travel = f64::hypot(shift.x, shift.y);
            let spread_change = spread - s0;
            // Coherence: all fingers but one must take part, so a hand settling doesn't trigger
            // anything.
            let needed = m.ids.len() - 1;
            if f64::abs(spread_change) > config.dock_swipe_decision_mm && f64::abs(spread_change) > travel {
                let radial: Vec<f64> = m
                    .ids
                    .iter()
                    .filter_map(|id| {
                        let (Some(&p), Some(&p0)) = (points.get(id), start.get(id)) else {
                            return None;
                        };
                        let r = f64::hypot(p.x * config.panel_width_mm - centroid.x, p.y * config.panel_height_mm - centroid.y);
                        let r0 = f64::hypot(p0.x * config.panel_width_mm - c0.x, p0.y * config.panel_height_mm - c0.y);
                        Some(r - r0)
                    })
                    .collect();
                let coherent = radial
                    .iter()
                    .filter(|&&r| (r < 0.0) == (spread_change < 0.0) && f64::abs(r) >= 2.0)
                    .count()
                    >= needed;
                if coherent && config.four_finger_pinch {
                    m.lock = Some(DockSwipeKind::Pinch);
                    smallvec![GestureAction::DockSwipe {
                        kind: DockSwipeKind::Pinch,
                        delta: -spread_change / config.dock_pinch_mm_per_unit,
                        phase: GesturePhase::Began,
                    }]
                } else {
                    smallvec![]
                }
            } else if travel > config.dock_swipe_decision_mm {
                let direction = MmVector { x: shift.x / travel, y: shift.y / travel };
                let along = m
                    .ids
                    .iter()
                    .filter(|id| {
                        let (Some(&p), Some(&p0)) = (points.get(id), start.get(id)) else {
                            return false;
                        };
                        let d = MmVector {
                            x: (p.x - p0.x) * config.panel_width_mm,
                            y: (p.y - p0.y) * config.panel_height_mm,
                        };
                        d.x * direction.x + d.y * direction.y >= config.dock_swipe_decision_mm / 2.0
                    })
                    .count();
                if along >= needed {
                    if f64::abs(shift.y) >= f64::abs(shift.x) {
                        if config.four_finger_vertical_swipe {
                            m.lock = Some(DockSwipeKind::Vertical);
                            smallvec![GestureAction::DockSwipe {
                                kind: DockSwipeKind::Vertical,
                                delta: -shift.y / config.dock_swipe_mm_per_unit,
                                phase: GesturePhase::Began,
                            }]
                        } else {
                            smallvec![]
                        }
                    } else if config.four_finger_horizontal_swipe {
                        m.lock = Some(DockSwipeKind::Horizontal);
                        smallvec![GestureAction::DockSwipe {
                            kind: DockSwipeKind::Horizontal,
                            delta: -shift.x / config.dock_swipe_mm_per_unit,
                            phase: GesturePhase::Began,
                        }]
                    } else {
                        smallvec![]
                    }
                } else {
                    smallvec![]
                }
            } else {
                smallvec![]
            }
        }
    };
    m.last_centroid_mm = centroid;
    m.last_spread_mm = spread;
    actions
}

fn centroid_mm(config: &GestureConfig, points: &BTreeMap<u8, PanelPoint>) -> MmVector {
    let c = centroid(points);
    MmVector { x: c.x * config.panel_width_mm, y: c.y * config.panel_height_mm }
}

/// Mean distance of the fingers from their centre, in millimetres.
fn spread_mm(config: &GestureConfig, points: &BTreeMap<u8, PanelPoint>) -> f64 {
    let c = centroid_mm(config, points);
    let mut total = 0.0;
    for p in points.values() {
        total += f64::hypot(p.x * config.panel_width_mm - c.x, p.y * config.panel_height_mm - c.y);
    }
    total / f64::max(points.len() as f64, 1.0)
}

fn centroid(points: &BTreeMap<u8, PanelPoint>) -> PanelPoint {
    let n = f64::max(points.len() as f64, 1.0);
    let (mut x, mut y) = (0.0, 0.0);
    for p in points.values() {
        x += p.x;
        y += p.y;
    }
    PanelPoint::new(x / n, y / n)
}

/// Velocity over the recent samples, in normalized panel units per second; zero if the finger
/// had stopped.
fn velocity(config: &GestureConfig, samples: &[(u64, PanelPoint)], now: u64) -> (f64, f64) {
    let (Some(&(first_ns, first)), Some(&(last_ns, last))) = (samples.first(), samples.last()) else {
        return (0.0, 0.0);
    };
    if last_ns <= first_ns || now - last_ns > config.velocity_stale_ns {
        return (0.0, 0.0);
    }
    let seconds = (last_ns - first_ns) as f64 / 1e9;
    ((last.x - first.x) / seconds, (last.y - first.y) / seconds)
}

/// The press's speed in mm/s over its recent samples: how far a returning finger may have
/// travelled during a dropout.
fn speed_mm_s(config: &GestureConfig, samples: &[(u64, PanelPoint)], now: u64) -> f64 {
    let (vx, vy) = velocity(config, samples, now);
    f64::hypot(vx * config.panel_width_mm, vy * config.panel_height_mm)
}

/// The action that commits a held press whose grace ran out (or was cut short).
fn grace_end(config: &GestureConfig, kind: DragKind, last: PanelPoint, samples: &[(u64, PanelPoint)], now: u64) -> GestureAction {
    match kind {
        DragKind::Drag => GestureAction::DragEnded { at: last },
        DragKind::Pan => {
            let (velocity_x, velocity_y) = velocity(config, samples, now);
            GestureAction::ScrollEnded { velocity_x, velocity_y }
        }
    }
}

// Geometry (millimetres, so the panel's aspect ratio doesn't skew distances or angles)

fn mm_vector(config: &GestureConfig, from: PanelPoint, to: PanelPoint) -> MmVector {
    MmVector { x: (to.x - from.x) * config.panel_width_mm, y: (to.y - from.y) * config.panel_height_mm }
}

fn distance_mm(config: &GestureConfig, a: PanelPoint, b: PanelPoint) -> f64 {
    f64::hypot((a.x - b.x) * config.panel_width_mm, (a.y - b.y) * config.panel_height_mm)
}

fn angle_degrees(config: &GestureConfig, a: PanelPoint, b: PanelPoint) -> f64 {
    f64::atan2((b.y - a.y) * config.panel_height_mm, (b.x - a.x) * config.panel_width_mm) * 180.0
        / std::f64::consts::PI
}

/// Screen y grows downward, so a counterclockwise turn decreases the atan2 angle.
fn counterclockwise(from: f64, to: f64) -> f64 {
    let mut delta = from - to;
    if delta > 180.0 {
        delta -= 360.0;
    }
    if delta < -180.0 {
        delta += 360.0;
    }
    delta
}

fn midpoint(a: PanelPoint, b: PanelPoint) -> PanelPoint {
    PanelPoint::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    const MS: u64 = 1_000_000;

    /// Normalized horizontal/vertical distance for a length in millimetres on this panel.
    fn mm_x(mm: f64) -> f64 {
        mm / GestureConfig::default().panel_width_mm
    }
    fn mm_y(mm: f64) -> f64 {
        mm / GestureConfig::default().panel_height_mm
    }

    // The events carry no time of their own: `Script::at` dates each with the moment it feeds it.
    fn began(id: u8, x: f64, y: f64) -> ContactEvent {
        ContactEvent { id, phase: ContactPhase::Began, x, y, at_ns: 0 }
    }
    fn moved(id: u8, x: f64, y: f64) -> ContactEvent {
        ContactEvent { id, phase: ContactPhase::Moved, x, y, at_ns: 0 }
    }
    fn ended(id: u8, x: f64, y: f64) -> ContactEvent {
        ContactEvent { id, phase: ContactPhase::Ended, x, y, at_ns: 0 }
    }

    /// The Swift `Script`: a recognizer plus every action it has produced.
    struct Script {
        recognizer: GestureRecognizer,
        actions: Vec<GestureAction>,
    }

    impl Script {
        fn build(one_finger_drag: OneFingerDragMode, configure: impl FnOnce(&mut GestureConfig)) -> Self {
            let mut config = GestureConfig { one_finger_drag, ..GestureConfig::default() };
            configure(&mut config);
            Self { recognizer: GestureRecognizer::new(config), actions: Vec::new() }
        }
        fn new() -> Self {
            Self::build(OneFingerDragMode::ClickAndDrag, |_| {})
        }
        fn in_mode(one_finger_drag: OneFingerDragMode) -> Self {
            Self::build(one_finger_drag, |_| {})
        }
        fn configured(configure: impl FnOnce(&mut GestureConfig)) -> Self {
            Self::build(OneFingerDragMode::ClickAndDrag, configure)
        }
        fn at(&mut self, t_ms: u64, events: &[ContactEvent]) {
            let events: Vec<ContactEvent> = events.iter().map(|event| ContactEvent { at_ns: t_ms * MS, ..*event }).collect();
            self.actions.extend(self.recognizer.handle(&events, t_ms * MS));
        }
    }

    // Centroids of three coordinates can differ from the exact value in the last binary digit.
    fn near(a: PanelPoint, b: PanelPoint) -> bool {
        f64::abs(a.x - b.x) < 1e-9 && f64::abs(a.y - b.y) < 1e-9
    }
    fn is_drag_began(action: Option<&GestureAction>, p: PanelPoint, hold: bool) -> bool {
        match action {
            Some(GestureAction::DragBegan { at, hold: h }) => *h == hold && near(*at, p),
            _ => false,
        }
    }
    fn is_drag_moved(action: Option<&GestureAction>, p: PanelPoint) -> bool {
        match action {
            Some(GestureAction::DragMoved { to }) => near(*to, p),
            _ => false,
        }
    }
    fn is_drag_ended(action: Option<&GestureAction>, p: PanelPoint) -> bool {
        match action {
            Some(GestureAction::DragEnded { at }) => near(*at, p),
            _ => false,
        }
    }

    // Single finger

    #[test]
    fn quick_stationary_touch_is_a_tap_on_lift() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.5, 0.5)]);
        assert!(s.actions.is_empty(), "nothing is posted before the gesture is classified");
        s.at(80, &[ended(0, 0.5, 0.5)]);
        assert_eq!(s.actions, [GestureAction::Tap { at: PanelPoint::new(0.5, 0.5), count: 1 }]);
    }

    #[test]
    fn jitter_below_the_slop_is_still_a_tap_at_the_resting_point() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.5, 0.5)]);
        s.at(30, &[moved(0, 0.5 + mm_x(2.0), 0.5)]);
        s.at(60, &[ended(0, 0.5 + mm_x(2.0), 0.5)]);
        assert_eq!(s.actions, [GestureAction::Tap { at: PanelPoint::new(0.5 + mm_x(2.0), 0.5), count: 1 }]);
    }

    #[test]
    fn second_and_third_tap_nearby_count_up_and_snap_to_the_first_tap() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.5, 0.5)]);
        s.at(60, &[ended(0, 0.5, 0.5)]);
        s.at(200, &[began(1, 0.5 + mm_x(5.0), 0.5)]);
        s.at(260, &[ended(1, 0.5 + mm_x(5.0), 0.5)]);
        s.at(400, &[began(2, 0.5, 0.5 + mm_y(5.0))]);
        s.at(460, &[ended(2, 0.5, 0.5 + mm_y(5.0))]);
        let first = PanelPoint::new(0.5, 0.5);
        assert_eq!(
            s.actions,
            [
                GestureAction::Tap { at: first, count: 1 },
                GestureAction::Tap { at: first, count: 2 },
                GestureAction::Tap { at: first, count: 3 },
            ]
        );
    }

    #[test]
    fn a_far_away_or_late_second_tap_starts_a_new_sequence() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.2, 0.2)]);
        s.at(60, &[ended(0, 0.2, 0.2)]);
        s.at(200, &[began(1, 0.2 + mm_x(20.0), 0.2)]);
        s.at(260, &[ended(1, 0.2 + mm_x(20.0), 0.2)]);
        s.at(2000, &[began(2, 0.2 + mm_x(20.0), 0.2)]);
        s.at(2060, &[ended(2, 0.2 + mm_x(20.0), 0.2)]);
        let counts: Vec<i32> = s
            .actions
            .iter()
            .map(|a| match a {
                GestureAction::Tap { count, .. } => *count as i32,
                _ => -1,
            })
            .collect();
        assert_eq!(counts, [1, 1, 1]);
    }

    #[test]
    fn stationary_hold_is_a_right_click_on_lift() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        s.at(700, &[moved(0, 0.4 + mm_x(1.0), 0.4)]);
        assert!(s.actions.is_empty());
        s.at(900, &[ended(0, 0.4 + mm_x(1.0), 0.4)]);
        assert_eq!(s.actions, [GestureAction::RightClick { at: PanelPoint::new(0.4 + mm_x(1.0), 0.4) }]);
    }

    // Owner's recording 2026-11-26, 10.954 s: a tap of 233 ms, whose lift report the tracker held
    // for 250 ms while the panel went on reporting. Counted to the moment the contact ended, it
    // had lasted long enough to be a long press, and a context menu opened.
    #[test]
    fn a_tap_is_judged_by_how_long_the_finger_was_down_not_by_how_long_its_lift_was_held() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        let held = ContactEvent { at_ns: 233 * MS, ..ended(0, 0.402, 0.4) };
        s.actions.extend(s.recognizer.handle(&[held], 713 * MS));
        assert_eq!(s.actions, [GestureAction::Tap { at: PanelPoint::new(0.402, 0.4), count: 1 }]);
    }

    // Two taps 667 ms apart to the finger, the first one's lift report held for 480 ms: delivered,
    // they are 437 ms apart. Counted by the clock they were delivered on, they would be a
    // double-click; the fingers were not.
    #[test]
    fn taps_are_counted_from_when_the_finger_was_there() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        let first = ContactEvent { at_ns: 233 * MS, ..ended(0, 0.402, 0.4) };
        s.actions.extend(s.recognizer.handle(&[first], 713 * MS));
        s.at(800, &[began(0, 0.404, 0.401)]);
        let second = ContactEvent { at_ns: 900 * MS, ..ended(0, 0.404, 0.401) };
        s.actions.extend(s.recognizer.handle(&[second], 1150 * MS));
        let counts: Vec<u32> = s.actions.iter().map(|a| match a { GestureAction::Tap { count, .. } => *count, _ => 0 }).collect();
        assert_eq!(counts, [1, 1]);
    }

    #[test]
    fn moving_beyond_the_slop_early_starts_a_drag_from_the_touch_down_point() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(70, &[moved(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(90, &[ended(0, 0.3 + mm_x(20.0), 0.3)]);
        assert_eq!(
            s.actions,
            [
                GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.3), hold: false },
                GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(12.0), 0.3) },
                GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(20.0), 0.3) },
                GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(20.0), 0.3) },
            ]
        );
    }

    #[test]
    fn hold_then_move_is_a_hold_drag() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(600, &[moved(0, 0.3 + mm_x(24.0), 0.3)]);
        assert_eq!(
            s.actions.first(),
            Some(&GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.3), hold: true })
        );
    }

    // Two fingers

    #[test]
    fn two_fingers_moving_together_scroll_and_end_with_velocity() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        s.at(40, &[began(1, 0.5, 0.4)]);
        s.at(60, &[moved(0, 0.4, 0.4 + mm_y(3.0)), moved(1, 0.5, 0.4 + mm_y(3.0))]);
        assert!(s.actions.is_empty(), "3 mm is below the scroll lock");
        s.at(70, &[moved(0, 0.4, 0.4 + mm_y(6.0)), moved(1, 0.5, 0.4 + mm_y(6.0))]);
        s.at(80, &[moved(0, 0.4, 0.4 + mm_y(9.0)), moved(1, 0.5, 0.4 + mm_y(9.0))]);
        s.at(90, &[ended(0, 0.4, 0.4 + mm_y(9.0))]);

        let Some(&GestureAction::Scroll { dx: dx0, dy: dy0, phase: GesturePhase::Began }) = s.actions.first() else {
            panic!("expected scroll began, got {:?}", s.actions);
        };
        assert!(f64::abs(dx0) < 1e-9);
        assert!(f64::abs(dy0 - mm_y(6.0)) < 1e-9, "the began event carries the movement since touch-down");
        let Some(&GestureAction::Scroll { dy: dy1, phase: GesturePhase::Changed, dx: _ }) = s.actions.get(1) else {
            panic!("expected scroll changed");
        };
        assert!(f64::abs(dy1 - mm_y(3.0)) < 1e-9);
        let Some(&GestureAction::ScrollEnded { velocity_x: vx, velocity_y: vy }) = s.actions.last() else {
            panic!("expected scrollEnded");
        };
        assert!(f64::abs(vx) < 1e-9);
        assert!(vy > 0.0);
    }

    #[test]
    fn spreading_two_fingers_apart_magnifies() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.45, 0.5), began(1, 0.55, 0.5)]);
        s.at(30, &[moved(0, 0.43, 0.5), moved(1, 0.57, 0.5)]); // distance x1.4
        s.at(45, &[moved(0, 0.42, 0.5), moved(1, 0.58, 0.5)]);
        s.at(60, &[ended(1, 0.58, 0.5)]);
        let Some(&GestureAction::Magnify { delta: d0, phase: GesturePhase::Began }) = s.actions.first() else {
            panic!("expected magnify began, got {:?}", s.actions);
        };
        assert!(f64::abs(d0 - 0.4) < 1e-9);
        let Some(&GestureAction::Magnify { delta: d1, phase: GesturePhase::Changed }) = s.actions.get(1) else {
            panic!("expected magnify changed");
        };
        assert!(d1 > 0.0);
        assert_eq!(s.actions.last(), Some(&GestureAction::Magnify { delta: 0.0, phase: GesturePhase::Ended }));
    }

    #[test]
    fn twisting_counterclockwise_rotates_positive() {
        let mut s = Script::new();
        // Two fingers 40 mm apart horizontally; the right finger moves up, the left finger down:
        // counterclockwise on screen.
        let (cx, cy) = (0.5, 0.5);
        s.at(0, &[began(0, cx - mm_x(20.0), cy), began(1, cx + mm_x(20.0), cy)]);
        s.at(30, &[moved(0, cx - mm_x(20.0), cy + mm_y(5.0)), moved(1, cx + mm_x(20.0), cy - mm_y(5.0))]);
        s.at(60, &[ended(0, cx - mm_x(20.0), cy + mm_y(5.0))]);
        let Some(&GestureAction::Rotate { degrees, phase: GesturePhase::Began }) = s.actions.first() else {
            panic!("expected rotate began, got {:?}", s.actions);
        };
        assert!(f64::abs(degrees - f64::atan2(5.0, 20.0) * 180.0 / PI) < 1e-6);
        assert_eq!(s.actions.last(), Some(&GestureAction::Rotate { degrees: 0.0, phase: GesturePhase::Ended }));
    }

    #[test]
    fn two_fingers_lifted_without_moving_do_nothing() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4), began(1, 0.5, 0.4)]);
        s.at(80, &[ended(0, 0.4, 0.4), ended(1, 0.5, 0.4)]);
        assert!(s.actions.is_empty());
    }

    #[test]
    fn a_second_finger_arriving_late_is_ignored() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(300, &[began(1, 0.6, 0.6)]);
        s.at(320, &[moved(0, 0.3 + mm_x(16.0), 0.3)]);
        assert_eq!(
            s.actions.first(),
            Some(&GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.3), hold: false })
        );
    }

    #[test]
    fn the_finger_left_after_a_two_finger_gesture_does_nothing_until_all_lift() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4), began(1, 0.5, 0.4)]);
        s.at(30, &[moved(0, 0.4, 0.4 + mm_y(10.0)), moved(1, 0.5, 0.4 + mm_y(10.0))]);
        s.at(40, &[ended(1, 0.5, 0.4 + mm_y(10.0))]);
        let count = s.actions.len();
        s.at(60, &[moved(0, 0.4 + mm_x(20.0), 0.4 + mm_y(10.0))]);
        s.at(80, &[ended(0, 0.4 + mm_x(20.0), 0.4 + mm_y(10.0))]);
        assert_eq!(s.actions.len(), count);
        s.at(500, &[began(2, 0.7, 0.7)]);
        s.at(560, &[ended(2, 0.7, 0.7)]);
        assert_eq!(s.actions.last(), Some(&GestureAction::Tap { at: PanelPoint::new(0.7, 0.7), count: 1 }));
    }

    // Palm

    #[test]
    fn morethan_three_fingers_cancels_an_active_drag_and_ignores_the_rest() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(60, &[began(1, 0.5, 0.5), began(2, 0.6, 0.6), began(3, 0.7, 0.7)]);
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(12.0), 0.3) })
        );
        let count = s.actions.len();
        s.at(80, &[moved(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(90, &[ended(0, 0.3, 0.3), ended(1, 0.5, 0.5), ended(2, 0.6, 0.6), ended(3, 0.7, 0.7)]);
        assert_eq!(s.actions.len(), count);
    }

    // Two-finger classification by how the fingers move

    #[test]
    fn a_sloppy_scroll_whose_finger_gap_changes_still_scrolls() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.45, 0.4), began(1, 0.55, 0.4)]); // 31 mm apart
        // Fingers drift apart by 10 % while starting to move down together...
        s.at(
            20,
            &[moved(0, 0.45 - mm_x(1.6), 0.4 + mm_y(2.0)), moved(1, 0.55 + mm_x(1.6), 0.4 + mm_y(2.0))],
        );
        // ...then keep moving down.
        s.at(
            40,
            &[moved(0, 0.45 - mm_x(2.0), 0.4 + mm_y(6.0)), moved(1, 0.55 + mm_x(2.0), 0.4 + mm_y(6.0))],
        );
        let Some(&GestureAction::Scroll { phase: GesturePhase::Began, .. }) = s.actions.first() else {
            panic!("expected scroll, got {:?}", s.actions);
        };
    }

    #[test]
    fn pinching_with_one_finger_held_still_magnifies() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.40, 0.5), began(1, 0.55, 0.5)]);
        s.at(20, &[moved(1, 0.55 - mm_x(3.0), 0.5)]);
        s.at(40, &[moved(1, 0.55 - mm_x(6.0), 0.5)]);
        let Some(&GestureAction::Magnify { delta, phase: GesturePhase::Began }) = s.actions.first() else {
            panic!("expected magnify, got {:?}", s.actions);
        };
        assert!(delta < 0.0);
    }

    #[test]
    fn circling_with_one_finger_held_still_rotates() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.45, 0.5), began(1, 0.55, 0.5)]); // 31 mm apart horizontally
        s.at(30, &[moved(1, 0.55, 0.5 - mm_y(6.0))]); // right finger moves up, around the left one
        let Some(&GestureAction::Rotate { phase: GesturePhase::Began, .. }) = s.actions.first() else {
            panic!("expected rotate, got {:?}", s.actions);
        };
    }

    // One-finger drag mode

    #[test]
    fn in_scroll_mode_a_one_finger_pan_scrolls_with_momentum() {
        let mut s = Script::build(OneFingerDragMode::Scroll, |c| c.drag_grace_ns = 0);
        s.at(0, &[began(0, 0.5, 0.5)]);
        s.at(30, &[moved(0, 0.5, 0.5 + mm_y(5.0))]);
        s.at(40, &[moved(0, 0.5, 0.5 + mm_y(12.0))]);
        s.at(50, &[moved(0, 0.5, 0.5 + mm_y(16.0))]);
        s.at(60, &[ended(0, 0.5, 0.5 + mm_y(16.0))]);
        let Some(&GestureAction::Scroll { dy: dy0, phase: GesturePhase::Began, dx: _ }) = s.actions.first() else {
            panic!("expected scroll began, got {:?}", s.actions);
        };
        assert!(f64::abs(dy0 - mm_y(12.0)) < 1e-9, "the began event carries the movement since touch-down");
        let Some(&GestureAction::Scroll { dy: dy1, phase: GesturePhase::Changed, dx: _ }) = s.actions.get(1) else {
            panic!("expected scroll changed");
        };
        assert!(f64::abs(dy1 - mm_y(4.0)) < 1e-9);
        let Some(&GestureAction::ScrollEnded { velocity_y: vy, .. }) = s.actions.last() else {
            panic!("expected scrollEnded");
        };
        assert!(vy > 0.0);
    }

    #[test]
    fn in_scroll_mode_hold_then_move_is_still_click_and_drag() {
        let mut s = Script::in_mode(OneFingerDragMode::Scroll);
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(600, &[moved(0, 0.3 + mm_x(24.0), 0.3)]);
        assert_eq!(
            s.actions.first(),
            Some(&GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.3), hold: true })
        );
    }

    // One-finger drags that start on a window's drag area (the pipeline supplies the zone)

    #[test]
    fn a_drag_on_a_title_bar_is_pressed_where_the_finger_is() {
        let mut s = Script::new();
        s.recognizer.drag_zone = Some(Box::new(|_| true));
        s.at(0, &[began(0, 0.3, 0.3)]);
        let finger = PanelPoint::new(0.3 + mm_x(12.0), 0.3);
        s.at(50, &[moved(0, finger.x, finger.y)]);
        // Nothing moves until the finger does: the window does not jump the dead travel.
        assert_eq!(s.actions, [GestureAction::DragBegan { at: finger, hold: false }]);
        s.at(70, &[moved(0, finger.x + mm_x(8.0), finger.y)]);
        s.at(90, &[ended(0, finger.x + mm_x(8.0), finger.y)]);
        assert_eq!(s.actions[1..], [GestureAction::DragMoved { to: PanelPoint::new(finger.x + mm_x(8.0), finger.y) },
            GestureAction::DragEnded { at: PanelPoint::new(finger.x + mm_x(8.0), finger.y) }]);
    }

    #[test]
    fn a_title_bar_drag_whose_finger_left_the_drag_area_is_pressed_where_it_landed() {
        let mut s = Script::new();
        s.recognizer.drag_zone = Some(Box::new(|point| point.x < 0.3 + mm_x(9.0)));
        s.at(0, &[began(0, 0.3, 0.3)]);
        let finger = PanelPoint::new(0.3 + mm_x(12.0), 0.3);
        s.at(50, &[moved(0, finger.x, finger.y)]);
        assert_eq!(
            s.actions,
            [
                GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.3), hold: false },
                GestureAction::DragMoved { to: finger },
            ]
        );
    }

    // Owner, 2026-11-25: "i have not been able to slow drag a window on this panel". A drag at
    // 10 mm/s on a title bar: 9 mm after 800 ms, inside the radius a planted finger had then
    // (24 mm), so it was never a drag and the lift was a right-click.
    #[test]
    fn a_slow_drag_on_a_title_bar_starts_at_the_tap_slop() {
        let mut s = Script::new();
        s.recognizer.drag_zone = Some(Box::new(|_| true));
        s.at(0, &[began(0, 0.3, 0.3)]);
        for step in 1u64..=8 {
            s.at(100 * step, &[moved(0, 0.3 + mm_x(step as f64), 0.3)]);
        }
        assert!(s.actions.is_empty(), "inside the tap slop nothing is decided");
        let finger = PanelPoint::new(0.3 + mm_x(9.0), 0.3);
        s.at(900, &[moved(0, finger.x, finger.y)]);
        assert_eq!(s.actions, [GestureAction::DragBegan { at: finger, hold: false }]);
        s.at(1000, &[ended(0, finger.x, finger.y)]);
        assert_eq!(s.actions.last(), Some(&GestureAction::DragEnded { at: finger }));
    }

    #[test]
    fn the_stationary_radius_grows_to_the_long_press_time_and_no_further() {
        let recognizer = GestureRecognizer::new(GestureConfig::default());
        let radius = |ms| recognizer.stationary_radius_mm(ms * MS);
        assert_eq!(radius(0), 8.0);
        assert_eq!(radius(150), 8.0);
        assert!((radius(325) - 12.375).abs() < 1e-9);
        assert!((radius(500) - 16.75).abs() < 1e-9);
        assert!((radius(5_000) - 16.75).abs() < 1e-9);
    }

    /// A swipe at 10 mm/s from the moment it lands: 17 mm after 1.7 s. The radius used to grow at
    /// 25 mm/s for as long as the finger was down, so this never classified, and the lift after
    /// half a second was a right-click.
    #[test]
    fn a_slow_swipe_scrolls_once_it_leaves_the_stationary_radius() {
        let mut s = Script::in_mode(OneFingerDragMode::Scroll);
        s.at(0, &[began(0, 0.5, 0.5)]);
        for step in 1u64..=16 {
            s.at(100 * step, &[moved(0, 0.5, 0.5 + mm_y(step as f64))]);
        }
        assert!(s.actions.is_empty(), "16 mm is still inside the radius");
        s.at(1_700, &[moved(0, 0.5, 0.5 + mm_y(17.0))]);
        assert_eq!(s.actions.first(), Some(&GestureAction::Scroll { dx: 0.0, dy: mm_y(17.0), phase: GesturePhase::Began }));
        s.at(1_800, &[ended(0, 0.5, 0.5 + mm_y(17.0))]);
        assert!(matches!(s.actions.last(), Some(GestureAction::ScrollEnded { .. })), "a swipe that scrolled does not right-click on lift: {:?}", s.actions);
    }

    // Drift of 2.5 mm in the first 100 ms, then clear travel at 600 ms: a swipe, classified by
    // when the finger first moved and how far it has travelled. Judged by the touch's age alone,
    // 600 ms made it a hold and the swipe selected text.
    #[test]
    fn a_swipe_that_drifts_within_its_first_100_ms_scrolls_however_late_it_travels() {
        let mut recognizer = GestureRecognizer::default();
        let _ = recognizer.handle(&[began(0, 0.5, 0.5)], 0);
        let _ = recognizer.handle(&[moved(0, 0.508, 0.5)], 100 * MS);
        let actions = recognizer.handle(&[moved(0, 0.59375, 0.5)], 600 * MS);
        assert_eq!(actions.first(), Some(&GestureAction::Scroll { dx: 0.09375, dy: 0.0, phase: GesturePhase::Began }));
    }

    /// The other side of the cap: a finger planted to right-click keeps 16.75 mm to settle in,
    /// however long it is held.
    #[test]
    fn a_planted_finger_keeps_its_drift_tolerance_however_long_it_is_held() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        s.at(500, &[moved(0, 0.4 + mm_x(8.0), 0.4)]);
        s.at(3_000, &[moved(0, 0.4 + mm_x(16.0), 0.4)]);
        assert!(s.actions.is_empty(), "slow drift stays a stationary hold: {:?}", s.actions);
        s.at(3_500, &[ended(0, 0.4 + mm_x(16.0), 0.4)]);
        assert_eq!(s.actions, [GestureAction::RightClick { at: PanelPoint::new(0.4 + mm_x(16.0), 0.4) }]);
    }

    /// Past that, travel is travel: 20 mm after a second is a click-and-drag from where the
    /// finger landed.
    #[test]
    fn a_finger_that_travels_past_the_tolerance_after_a_hold_drags() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        s.at(1_000, &[moved(0, 0.4 + mm_x(20.0), 0.4)]);
        assert_eq!(
            s.actions,
            [
                GestureAction::DragBegan { at: PanelPoint::new(0.4, 0.4), hold: true },
                GestureAction::DragMoved { to: PanelPoint::new(0.4 + mm_x(20.0), 0.4) },
            ]
        );
    }

    #[test]
    fn scroll_mode_is_the_default() {
        assert_eq!(GestureConfig::default().one_finger_drag, OneFingerDragMode::Scroll);
    }

    // Three-finger drag (like the trackpad setting)

    #[test]
    fn three_fingers_moving_together_drag_from_their_centre() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.5)]);
        s.at(30, &[began(1, 0.5, 0.5)]);
        s.at(60, &[began(2, 0.6, 0.5)]);
        s.at(
            100,
            &[
                moved(0, 0.4, 0.5 + mm_y(6.0)),
                moved(1, 0.5, 0.5 + mm_y(6.0)),
                moved(2, 0.6, 0.5 + mm_y(6.0)),
            ],
        );
        assert_eq!(s.actions.len(), 2);
        assert!(is_drag_began(s.actions.first(), PanelPoint::new(0.5, 0.5), true));
        assert!(is_drag_moved(s.actions.last(), PanelPoint::new(0.5, 0.5 + mm_y(6.0))));
        s.at(150, &[ended(1, 0.5, 0.5 + mm_y(6.0))]);
        assert!(
            is_drag_ended(s.actions.last(), PanelPoint::new(0.5, 0.5 + mm_y(6.0))),
            "the drag ends when the fingers lift"
        );
        let count = s.actions.len();
        s.at(170, &[ended(0, 0.4, 0.5 + mm_y(6.0)), ended(2, 0.6, 0.5 + mm_y(6.0))]);
        assert_eq!(s.actions.len(), count);
    }

    #[test]
    fn the_three_finger_drag_point_can_be_the_first_finger() {
        let mut s = Script::configured(|c| c.three_finger_anchor = ThreeFingerAnchor::FirstFinger);
        s.at(0, &[began(0, 0.4, 0.5)]);
        s.at(30, &[began(1, 0.5, 0.5)]);
        s.at(60, &[began(2, 0.6, 0.5)]);
        s.at(
            100,
            &[
                moved(0, 0.4, 0.5 + mm_y(6.0)),
                moved(1, 0.5, 0.5 + mm_y(6.0)),
                moved(2, 0.6, 0.5 + mm_y(6.0)),
            ],
        );
        assert_eq!(s.actions.len(), 2);
        assert!(is_drag_began(s.actions.first(), PanelPoint::new(0.4, 0.5), true));
        assert!(is_drag_moved(s.actions.last(), PanelPoint::new(0.4, 0.5 + mm_y(6.0))));
    }

    #[test]
    fn three_fingers_that_do_not_move_do_nothing() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.5), began(1, 0.5, 0.5), began(2, 0.6, 0.5)]);
        s.at(120, &[ended(0, 0.4, 0.5), ended(1, 0.5, 0.5), ended(2, 0.6, 0.5)]);
        assert!(s.actions.is_empty());
    }

    #[test]
    fn a_third_finger_during_a_two_finger_scroll_is_still_ignored() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4), began(1, 0.5, 0.4)]);
        s.at(30, &[moved(0, 0.4, 0.4 + mm_y(6.0)), moved(1, 0.5, 0.4 + mm_y(6.0))]); // scroll locks
        s.at(60, &[began(2, 0.6, 0.4 + mm_y(6.0))]);
        s.at(
            80,
            &[
                moved(0, 0.4, 0.4 + mm_y(9.0)),
                moved(1, 0.5, 0.4 + mm_y(9.0)),
                moved(2, 0.6, 0.4 + mm_y(9.0)),
            ],
        );
        assert!(!s.actions.iter().any(|a| matches!(a, GestureAction::DragBegan { .. })));
        let Some(&GestureAction::Scroll { phase: GesturePhase::Changed, .. }) = s.actions.last() else {
            panic!("scroll should continue, got {:?}", s.actions);
        };
    }

    #[test]
    fn a_third_finger_arriving_late_leaves_the_two_finger_gesture() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        s.at(30, &[began(1, 0.5, 0.4)]);
        s.at(400, &[began(2, 0.6, 0.4)]);
        s.at(
            440,
            &[
                moved(0, 0.4, 0.4 + mm_y(6.0)),
                moved(1, 0.5, 0.4 + mm_y(6.0)),
                moved(2, 0.6, 0.4 + mm_y(6.0)),
            ],
        );
        let Some(&GestureAction::Scroll { phase: GesturePhase::Began, .. }) = s.actions.first() else {
            panic!("expected the two-finger scroll, got {:?}", s.actions);
        };
    }

    #[test]
    fn with_three_finger_drag_off_three_fingers_behave_as_before() {
        let mut s = Script::configured(|c| c.three_finger_drag = false);
        s.at(0, &[began(0, 0.4, 0.4), began(1, 0.5, 0.4), began(2, 0.6, 0.4)]);
        s.at(
            40,
            &[
                moved(0, 0.4, 0.4 + mm_y(6.0)),
                moved(1, 0.5, 0.4 + mm_y(6.0)),
                moved(2, 0.6, 0.4 + mm_y(6.0)),
            ],
        );
        let Some(&GestureAction::Scroll { phase: GesturePhase::Began, .. }) = s.actions.first() else {
            panic!("expected the two-finger scroll, got {:?}", s.actions);
        };
    }

    #[test]
    fn a_palm_during_a_three_finger_drag_ends_it() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.5), began(1, 0.5, 0.5), began(2, 0.6, 0.5)]);
        s.at(
            40,
            &[
                moved(0, 0.4, 0.5 + mm_y(6.0)),
                moved(1, 0.5, 0.5 + mm_y(6.0)),
                moved(2, 0.6, 0.5 + mm_y(6.0)),
            ],
        );
        s.at(60, &[began(3, 0.7, 0.7)]);
        assert!(is_drag_ended(s.actions.last(), PanelPoint::new(0.5, 0.5 + mm_y(6.0))));
    }

    // Rest a finger, add two (owner's choice 2026-10-11: the drag grabs exactly under the resting finger)

    #[test]
    fn resting_a_finger_then_adding_two_drags_from_the_resting_finger() {
        // The owner's settings: one-finger drag scrolls, three-finger drag grabs at the centre of
        // the fingers. Before, the late fingers were ignored and the first finger's move scrolled.
        let mut s = Script::in_mode(OneFingerDragMode::Scroll);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(250, &[began(1, 0.45, 0.55)]);
        s.at(300, &[began(2, 0.5, 0.55)]);
        s.at(
            350,
            &[
                moved(0, 0.3, 0.5 + mm_y(6.0)),
                moved(1, 0.45, 0.55 + mm_y(6.0)),
                moved(2, 0.5, 0.55 + mm_y(6.0)),
            ],
        );
        assert_eq!(s.actions.len(), 2);
        assert!(is_drag_began(s.actions.first(), PanelPoint::new(0.3, 0.5), true));
        assert!(is_drag_moved(s.actions.last(), PanelPoint::new(0.3, 0.5 + mm_y(6.0))));
        s.at(400, &[ended(0, 0.3, 0.5 + mm_y(6.0)), ended(1, 0.45, 0.55 + mm_y(6.0)), ended(2, 0.5, 0.55 + mm_y(6.0))]);
        assert!(is_drag_ended(s.actions.last(), PanelPoint::new(0.3, 0.5 + mm_y(6.0))));
    }

    // Owner's pick 2026-10-11: grab where the resting finger first touched, not where it rolled to.
    #[test]
    fn the_resting_finger_grabs_where_it_first_touched() {
        let mut s = Script::in_mode(OneFingerDragMode::Scroll);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(150, &[moved(0, 0.3 + mm_x(2.0), 0.5)]); // the finger rolls 2 mm (within the tap slop) as the others land
        s.at(250, &[began(1, 0.45, 0.55)]);
        s.at(300, &[began(2, 0.5, 0.55)]);
        s.at(
            350,
            &[
                moved(0, 0.3 + mm_x(2.0), 0.5 + mm_y(6.0)),
                moved(1, 0.45, 0.55 + mm_y(6.0)),
                moved(2, 0.5, 0.55 + mm_y(6.0)),
            ],
        );
        assert!(
            is_drag_began(s.actions.first(), PanelPoint::new(0.3, 0.5), true),
            "where it first touched"
        );
        assert!(
            is_drag_moved(s.actions.last(), PanelPoint::new(0.3, 0.5 + mm_y(6.0))),
            "then moves with the hand, without jumping by the roll"
        );
    }

    #[test]
    fn resting_then_adding_two_without_moving_does_nothing() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(300, &[began(1, 0.45, 0.55), began(2, 0.5, 0.55)]);
        s.at(900, &[ended(1, 0.45, 0.55), ended(2, 0.5, 0.55)]);
        s.at(950, &[ended(0, 0.3, 0.5)]);
        assert!(s.actions.is_empty(), "no tap, no right-click");
    }

    #[test]
    fn one_late_finger_is_still_ignored() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(200, &[began(1, 0.45, 0.55)]);
        s.at(300, &[ended(1, 0.45, 0.55)]);
        s.at(350, &[ended(0, 0.3, 0.5)]);
        assert_eq!(s.actions, [GestureAction::Tap { at: PanelPoint::new(0.3, 0.5), count: 1 }]);
    }

    #[test]
    fn two_fingers_added_after_the_rest_window_are_ignored_as_before() {
        let mut s = Script::in_mode(OneFingerDragMode::Scroll);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(1600, &[began(1, 0.45, 0.55), began(2, 0.5, 0.55)]);
        s.at(1700, &[ended(1, 0.45, 0.55), ended(2, 0.5, 0.55)]);
        s.at(1750, &[ended(0, 0.3, 0.5)]);
        assert_eq!(
            s.actions,
            [GestureAction::RightClick { at: PanelPoint::new(0.3, 0.5) }],
            "a long press"
        );
    }

    #[test]
    fn with_three_finger_drag_off_resting_then_adding_two_behaves_as_before() {
        let mut s = Script::configured(|c| c.three_finger_drag = false);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(300, &[began(1, 0.45, 0.55), began(2, 0.5, 0.55)]);
        s.at(350, &[ended(1, 0.45, 0.55), ended(2, 0.5, 0.55)]);
        s.at(400, &[ended(0, 0.3, 0.5)]);
        assert_eq!(s.actions, [GestureAction::Tap { at: PanelPoint::new(0.3, 0.5), count: 1 }]);
    }

    // Drawing mode (owner's choice 2026-10-11: one finger is a pen, nothing else counts)
    // Strokes go through the window server (hold: true = Warp): delivered straight to the app
    // (Direct), no mouse button is pressed system-wide, and canvases took the movement for
    // hovering (owner test: nothing drew).

    #[test]
    fn in_drawing_mode_the_first_finger_draws_from_touch_down() {
        let mut s = Script::configured(|c| c.drawing_mode = true);
        s.at(0, &[began(0, 0.3, 0.5)]);
        assert_eq!(
            s.actions,
            [GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.5), hold: true }],
            "the stroke starts at touch-down"
        );
        s.at(16, &[moved(0, 0.31, 0.5)]);
        s.at(32, &[ended(0, 0.32, 0.5)]);
        assert_eq!(
            &s.actions[s.actions.len() - 2..],
            &[
                GestureAction::DragMoved { to: PanelPoint::new(0.31, 0.5) },
                GestureAction::DragEnded { at: PanelPoint::new(0.32, 0.5) },
            ][..]
        );
    }

    #[test]
    fn in_drawing_mode_a_tap_is_a_dot() {
        let mut s = Script::configured(|c| c.drawing_mode = true);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(60, &[ended(0, 0.3, 0.5)]);
        assert_eq!(
            s.actions,
            [
                GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.5), hold: true },
                GestureAction::DragEnded { at: PanelPoint::new(0.3, 0.5) },
            ]
        );
    }

    #[test]
    fn in_drawing_mode_a_palm_and_other_fingers_are_ignored() {
        let mut s = Script::configured(|c| c.drawing_mode = true);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(
            20,
            &[
                began(1, 0.6, 0.8),
                began(2, 0.62, 0.8),
                began(3, 0.64, 0.8),
                began(4, 0.66, 0.8),
                began(5, 0.68, 0.8),
                began(6, 0.7, 0.8),
            ],
        );
        s.at(40, &[moved(0, 0.35, 0.5), moved(1, 0.6, 0.85), moved(2, 0.62, 0.85)]);
        s.at(60, &[ended(0, 0.35, 0.5)]);
        assert_eq!(
            s.actions,
            [
                GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.5), hold: true },
                GestureAction::DragMoved { to: PanelPoint::new(0.35, 0.5) },
                GestureAction::DragEnded { at: PanelPoint::new(0.35, 0.5) },
            ],
            "a palm doesn't cancel the stroke"
        );
    }

    #[test]
    fn in_drawing_mode_no_gesture_is_recognized() {
        let mut s = Script::configured(|c| c.drawing_mode = true);
        s.at(0, &[began(0, 0.4, 0.4), began(1, 0.5, 0.4), began(2, 0.6, 0.4), began(3, 0.7, 0.4)]);
        s.at(
            30,
            &[
                moved(0, 0.4, 0.4 + mm_y(12.0)),
                moved(1, 0.5, 0.4 + mm_y(12.0)),
                moved(2, 0.6, 0.4 + mm_y(12.0)),
                moved(3, 0.7, 0.4 + mm_y(12.0)),
            ],
        );
        s.at(
            60,
            &[
                ended(0, 0.4, 0.4 + mm_y(12.0)),
                ended(1, 0.5, 0.4 + mm_y(12.0)),
                ended(2, 0.6, 0.4 + mm_y(12.0)),
                ended(3, 0.7, 0.4 + mm_y(12.0)),
            ],
        );
        assert!(
            s.actions.iter().all(|a| {
                matches!(
                    a,
                    GestureAction::DragBegan { .. } | GestureAction::DragMoved { .. } | GestureAction::DragEnded { .. }
                )
            }),
            "only the pen's stroke: no scroll, pinch, swipe or palm cancel"
        );
    }

    #[test]
    fn after_the_pen_lifts_the_next_new_finger_draws() {
        let mut s = Script::configured(|c| c.drawing_mode = true);
        s.at(0, &[began(0, 0.3, 0.5)]);
        s.at(10, &[began(1, 0.7, 0.5)]); // resting
        s.at(20, &[ended(0, 0.3, 0.5)]);
        s.at(40, &[moved(1, 0.72, 0.5)]); // never the pen: still ignored
        s.at(60, &[began(2, 0.2, 0.2)]);
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::DragBegan { at: PanelPoint::new(0.2, 0.2), hold: true })
        );
        assert!(!s.actions.contains(&GestureAction::DragMoved { to: PanelPoint::new(0.72, 0.5) }));
    }

    // Gestures switched off (trackpad settings)

    #[test]
    fn with_pinch_off_spreading_fingers_does_not_zoom() {
        let mut s = Script::configured(|c| c.pinch_enabled = false);
        s.at(0, &[began(0, 0.45, 0.5), began(1, 0.55, 0.5)]);
        s.at(30, &[moved(0, 0.43, 0.5), moved(1, 0.57, 0.5)]);
        s.at(60, &[ended(0, 0.43, 0.5), ended(1, 0.57, 0.5)]);
        assert!(!s.actions.iter().any(|a| matches!(a, GestureAction::Magnify { .. })));
    }

    #[test]
    fn with_rotate_off_twisting_does_not_rotate() {
        let mut s = Script::configured(|c| c.rotate_enabled = false);
        s.at(0, &[began(0, 0.5 - mm_x(20.0), 0.5), began(1, 0.5 + mm_x(20.0), 0.5)]);
        s.at(
            30,
            &[
                moved(0, 0.5 - mm_x(20.0), 0.5 + mm_y(5.0)),
                moved(1, 0.5 + mm_x(20.0), 0.5 - mm_y(5.0)),
            ],
        );
        assert!(!s.actions.iter().any(|a| matches!(a, GestureAction::Rotate { .. })));
    }

    #[test]
    fn with_scroll_off_two_fingers_moving_together_do_not_scroll() {
        let mut s = Script::configured(|c| c.scroll_enabled = false);
        s.at(0, &[began(0, 0.4, 0.4), began(1, 0.5, 0.4)]);
        s.at(30, &[moved(0, 0.4, 0.4 + mm_y(10.0)), moved(1, 0.5, 0.4 + mm_y(10.0))]);
        assert!(s.actions.is_empty());
    }

    // Four-/five-finger dock swipes and the Notification Center edge swipe

    fn four_down(s: &mut Script, y: f64, spread: f64) {
        s.at(0, &[began(0, 0.4, y), began(1, 0.4 + spread, y), began(2, 0.4 + 2.0 * spread, y)]);
        s.at(60, &[began(3, 0.4 + 3.0 * spread, y + 0.08)]);
    }

    fn dock_swipes(actions: &[GestureAction]) -> Vec<(DockSwipeKind, f64, GesturePhase)> {
        actions
            .iter()
            .filter_map(|a| match a {
                GestureAction::DockSwipe { kind, delta, phase } => Some((*kind, *delta, *phase)),
                _ => None,
            })
            .collect()
    }

    fn move_all(s: &mut Script, t: u64, ids: &[u8], start: &[(f64, f64)], dx: f64, dy: f64) {
        let events: Vec<ContactEvent> = ids
            .iter()
            .zip(start.iter())
            .map(|(&id, &(x, y))| moved(id, x + dx, y + dy))
            .collect();
        s.actions.extend(s.recognizer.handle(&events, t * MS));
    }

    const FOUR_START: [(f64, f64); 4] = [(0.4, 0.5), (0.45, 0.5), (0.5, 0.5), (0.55, 0.58)];

    #[test]
    fn four_fingers_swiping_up_open_mission_control() {
        let mut s = Script::new();
        four_down(&mut s, 0.5, 0.05);
        move_all(&mut s, 100, &[0, 1, 2, 3], &FOUR_START, 0.0, -mm_y(10.0));
        move_all(&mut s, 120, &[0, 1, 2, 3], &FOUR_START, 0.0, -mm_y(20.0));
        s.at(140, &[ended(0, 0.4, 0.5 - mm_y(20.0))]);
        let swipes = dock_swipes(&s.actions);
        assert!(matches!(swipes.first(), Some((DockSwipeKind::Vertical, _, GesturePhase::Began))));
        assert!(
            swipes
                .iter()
                .filter(|(_, _, phase)| *phase != GesturePhase::Ended)
                .all(|(_, delta, _)| *delta > 0.0),
            "up = positive = Mission Control"
        );
        assert!(matches!(swipes.last(), Some((_, _, GesturePhase::Ended))));
    }

    #[test]
    fn four_fingers_swiping_down_open_app_expose() {
        let mut s = Script::new();
        four_down(&mut s, 0.5, 0.05);
        move_all(&mut s, 100, &[0, 1, 2, 3], &FOUR_START, 0.0, mm_y(15.0));
        let swipes = dock_swipes(&s.actions);
        assert!(matches!(swipes.first(), Some((DockSwipeKind::Vertical, _, _))));
        assert!((swipes.first().map(|(_, delta, _)| *delta).unwrap_or(0.0)) < 0.0);
    }

    #[test]
    fn four_fingers_swiping_left_move_to_the_space_on_the_right() {
        let mut s = Script::new();
        four_down(&mut s, 0.5, 0.05);
        move_all(&mut s, 100, &[0, 1, 2, 3], &FOUR_START, -mm_x(15.0), 0.0);
        let swipes = dock_swipes(&s.actions);
        assert!(matches!(swipes.first(), Some((DockSwipeKind::Horizontal, _, _))));
        assert!(
            (swipes.first().map(|(_, delta, _)| *delta).unwrap_or(0.0)) > 0.0,
            "content follows the fingers: left = next Space on the right"
        );
    }

    #[test]
    fn thumb_and_fingers_pinching_in_open_launchpad() {
        let mut s = Script::new();
        four_down(&mut s, 0.5, 0.08);
        // Fingers move toward their centre (about 0.52, 0.52) by 40 %.
        let start: [(f64, f64); 4] = [(0.4, 0.5), (0.48, 0.5), (0.56, 0.5), (0.64, 0.58)];
        let centre_x = start.iter().map(|p| p.0).sum::<f64>() / 4.0;
        let centre_y = start.iter().map(|p| p.1).sum::<f64>() / 4.0;
        let events: Vec<ContactEvent> = [0u8, 1, 2, 3]
            .iter()
            .zip(start)
            .map(|(&id, (x, y))| moved(id, x + (centre_x - x) * 0.4, y + (centre_y - y) * 0.4))
            .collect();
        s.actions.extend(s.recognizer.handle(&events, 100 * MS));
        let swipes = dock_swipes(&s.actions);
        assert!(matches!(swipes.first(), Some((DockSwipeKind::Pinch, _, _))));
        assert!(
            (swipes.first().map(|(_, delta, _)| *delta).unwrap_or(0.0)) > 0.0,
            "pinch in = positive = Launchpad"
        );
    }

    #[test]
    fn with_mission_control_swipe_off_four_fingers_do_nothing_vertically() {
        let mut s = Script::configured(|c| c.four_finger_vertical_swipe = false);
        four_down(&mut s, 0.5, 0.05);
        move_all(&mut s, 100, &[0, 1, 2, 3], &FOUR_START, 0.0, -mm_y(20.0));
        assert!(dock_swipes(&s.actions).is_empty());
    }

    #[test]
    fn a_sixth_finger_cancels_a_dock_swipe() {
        let mut s = Script::new();
        four_down(&mut s, 0.5, 0.05);
        move_all(&mut s, 100, &[0, 1, 2, 3], &FOUR_START, 0.0, -mm_y(15.0));
        s.at(120, &[began(4, 0.2, 0.2), began(5, 0.25, 0.2)]);
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::DockSwipeCancelled { kind: DockSwipeKind::Vertical }),
            "a palm must not complete Mission Control"
        );
    }

    #[test]
    fn two_fingers_from_the_right_edge_moving_left_open_notification_center() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.99, 0.4), began(1, 0.995, 0.5)]);
        s.at(40, &[moved(0, 0.99 - mm_x(15.0), 0.4), moved(1, 0.995 - mm_x(15.0), 0.5)]);
        s.at(60, &[moved(0, 0.99 - mm_x(25.0), 0.4), moved(1, 0.995 - mm_x(25.0), 0.5)]);
        assert_eq!(s.actions, [GestureAction::NotificationCenter]);
    }

    #[test]
    fn with_the_edge_swipe_off_two_fingers_at_the_edge_scroll() {
        let mut s = Script::configured(|c| c.notification_center_swipe = false);
        s.at(0, &[began(0, 0.99, 0.4), began(1, 0.995, 0.5)]);
        s.at(40, &[moved(0, 0.99 - mm_x(15.0), 0.4), moved(1, 0.995 - mm_x(15.0), 0.5)]);
        let Some(&GestureAction::Scroll { phase: GesturePhase::Began, .. }) = s.actions.first() else {
            panic!("expected a scroll, got {:?}", s.actions);
        };
    }

    // A hand settling: four contacts, one slides far, the others barely move. Must not open
    // Mission Control.
    #[test]
    fn a_hand_settling_does_not_trigger_a_system_gesture() {
        let mut s = Script::new();
        four_down(&mut s, 0.5, 0.05);
        s.at(
            100,
            &[
                moved(0, 0.4, 0.5 - mm_y(40.0)),
                moved(1, 0.45, 0.5 - mm_y(1.0)),
                moved(2, 0.5, 0.5),
                moved(3, 0.55, 0.58 + mm_y(1.0)),
            ],
        );
        s.at(140, &[moved(0, 0.4, 0.5 - mm_y(48.0))]);
        assert!(dock_swipes(&s.actions).is_empty());
    }

    // Owner report: Notification Center was hard to trigger. Real attempts: fingers stacked
    // vertically a little inside the edge, moving left unevenly, ended up as rotate or scroll.
    #[test]
    fn an_uneven_edge_swipe_starting_a_few_millimetres_in_still_opens_notification_center() {
        let mut s = Script::new();
        let x = 1.0 - mm_x(10.0); // 10 mm in from the right edge
        s.at(0, &[began(0, x, 0.40), began(1, x, 0.47)]);
        s.at(40, &[moved(0, x - mm_x(9.0), 0.40), moved(1, x - mm_x(4.0), 0.47)]);
        s.at(70, &[moved(0, x - mm_x(16.0), 0.40), moved(1, x - mm_x(9.0), 0.47)]);
        assert!(s.actions.contains(&GestureAction::NotificationCenter));
        assert!(!s.actions.iter().any(|a| matches!(a, GestureAction::Rotate { .. })));
    }

    // Long-press with drift

    #[test]
    fn a_long_press_that_drifts_slowly_still_right_clicks() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.4, 0.4)]);
        // 4 mm of drift per 400 ms: 10 mm/s, exactly the tolerated rate, for a full second.
        s.at(400, &[moved(0, 0.4 + mm_x(4.0), 0.4)]);
        s.at(800, &[moved(0, 0.4 + mm_x(8.0), 0.4)]);
        s.at(1200, &[moved(0, 0.4 + mm_x(12.0), 0.4)]);
        assert!(s.actions.is_empty(), "slow drift stays a stationary hold");
        s.at(1250, &[ended(0, 0.4 + mm_x(12.0), 0.4)]);
        assert_eq!(s.actions, [GestureAction::RightClick { at: PanelPoint::new(0.4 + mm_x(12.0), 0.4) }]);
    }

    // Drag grace (off by default; the tracker bridges the panel's tip flaps)

    /// A lift ends the drag when the tracker reports it. The touch that follows is its own
    /// gesture, even where the drag stopped and soon after it: owner, 2026-11-25, "did it on the
    /// telegram windown and it didnt respond and then it jumped around".
    #[test]
    fn a_touch_after_the_drag_has_lifted_is_a_new_gesture() {
        let mut s = Script::new();
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(70, &[moved(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(80, &[ended(0, 0.3 + mm_x(20.0), 0.3)]);
        assert_eq!(s.actions.last(), Some(&GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(20.0), 0.3) }));
        assert_eq!(s.recognizer.next_deadline_ns(), None);
        s.at(380, &[began(1, 0.3 + mm_x(24.0), 0.3)]); // 4 mm on, 300 ms later
        s.at(440, &[ended(1, 0.3 + mm_x(24.0), 0.3)]);
        assert_eq!(s.actions.last(), Some(&GestureAction::Tap { at: PanelPoint::new(0.3 + mm_x(24.0), 0.3), count: 1 }));
        assert_eq!(s.actions.iter().filter(|a| matches!(a, GestureAction::DragBegan { .. })).count(), 1);
    }

    // Switched on, the grace holds a drag through a dropout. These tests say so each time.

    #[test]
    fn a_flapped_drag_resumes_without_re_grabbing() {
        let mut s = Script::configured(|c| c.drag_grace_ns = 150_000_000);
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(70, &[moved(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(80, &[ended(0, 0.3 + mm_x(20.0), 0.3)]); // the contact flaps off mid-drag
        s.at(140, &[began(1, 0.3 + mm_x(24.0), 0.3), moved(1, 0.3 + mm_x(30.0), 0.3)]); // it returns, renumbered
        s.at(400, &[ended(1, 0.3 + mm_x(30.0), 0.3)]);
        s.at(600, &[]); // nothing: the grace deadline passes quietly
        assert_eq!(
            s.actions,
            [
                GestureAction::DragBegan { at: PanelPoint::new(0.3, 0.3), hold: false },
                GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(12.0), 0.3) },
                GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(20.0), 0.3) },
                GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(24.0), 0.3) },
                GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(30.0), 0.3) },
                GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(30.0), 0.3) },
            ]
        );
    }

    #[test]
    fn an_unresumed_drag_ends_when_the_grace_passes() {
        let mut s = Script::configured(|c| c.drag_grace_ns = 150_000_000);
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(60, &[ended(0, 0.3 + mm_x(12.0), 0.3)]);
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::DragMoved { to: PanelPoint::new(0.3 + mm_x(12.0), 0.3) })
        );
        s.at(100, &[]); // inside the grace: still held
        assert_eq!(s.actions.len(), 2);
        s.at(220, &[]); // past the grace: the press commits
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(12.0), 0.3) })
        );
    }

    #[test]
    fn a_held_grace_reports_its_deadline_and_nothing_else_does() {
        let mut s = Script::configured(|c| c.drag_grace_ns = 150_000_000);
        assert_eq!(s.recognizer.next_deadline_ns(), None);
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        assert_eq!(s.recognizer.next_deadline_ns(), None);
        s.at(60, &[ended(0, 0.3 + mm_x(12.0), 0.3)]);
        assert_eq!(s.recognizer.next_deadline_ns(), Some(60 * MS + 150_000_000));
        s.at(220, &[]);
        assert_eq!(s.recognizer.next_deadline_ns(), None);
    }

    #[test]
    fn a_flapped_scroll_resumes_with_the_same_stream() {
        let mut s = Script::build(OneFingerDragMode::Scroll, |c| c.drag_grace_ns = 150_000_000);
        s.at(0, &[began(0, 0.5, 0.5)]);
        s.at(40, &[moved(0, 0.5, 0.5 + mm_y(12.0))]);
        s.at(50, &[ended(0, 0.5, 0.5 + mm_y(12.0))]);
        s.at(120, &[began(2, 0.5, 0.5 + mm_y(16.0))]);
        s.at(300, &[ended(2, 0.5, 0.5 + mm_y(16.0))]);
        s.at(500, &[]);
        let Some(&GestureAction::Scroll { dy: dy0, phase: phase0, dx: _ }) = s.actions.first() else {
            panic!("expected scroll began");
        };
        assert!(phase0 == GesturePhase::Began && f64::abs(dy0 - mm_y(12.0)) < 1e-9);
        let Some(&GestureAction::Scroll { dy: dy1, phase: phase1, dx: _ }) = s.actions.get(1) else {
            panic!("expected scroll changed on resume");
        };
        assert!(phase1 == GesturePhase::Changed && f64::abs(dy1 - mm_y(4.0)) < 1e-9);
        assert!(matches!(s.actions.last(), Some(GestureAction::ScrollEnded { .. })));
    }

    /// With the grace at the Swift build's 1.2 s, a long dropout mid-drag is held, and a finger
    /// resuming tens of millimetres along its way continues the same drag instead of re-grabbing
    /// wherever it reappeared.
    #[test]
    fn a_long_dropout_mid_drag_is_bridged_with_a_radius_grown_by_speed() {
        let mut s = Script::configured(|c| c.drag_grace_ns = 1_200_000_000);
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(70, &[moved(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(80, &[ended(0, 0.3 + mm_x(20.0), 0.3)]); // the panel drops out mid-drag
        s.at(680, &[began(1, 0.3 + mm_x(40.0), 0.3)]); // 600 ms later, 20 mm along the way
        s.at(900, &[ended(1, 0.3 + mm_x(44.0), 0.3)]);
        s.at(2200, &[]); // the second grace runs out quietly
        assert_eq!(
            s.actions.iter().filter(|a| matches!(a, GestureAction::DragBegan { .. })).count(),
            1
        );
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(44.0), 0.3) })
        );
    }

    /// A touch landing far from the held drag is not that drag's finger coming back: the drag
    /// commits its end and the new touch is its own gesture.
    #[test]
    fn a_touch_far_from_the_held_drag_commits_the_end_instead_of_resuming() {
        let mut s = Script::configured(|c| c.drag_grace_ns = 1_200_000_000);
        s.at(0, &[began(0, 0.3, 0.3)]);
        s.at(50, &[moved(0, 0.3 + mm_x(12.0), 0.3)]);
        s.at(70, &[moved(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(80, &[ended(0, 0.3 + mm_x(20.0), 0.3)]);
        s.at(380, &[began(1, 0.3 + mm_x(120.0), 0.3)]); // 100 mm away: not this drag's finger
        s.at(440, &[ended(1, 0.3 + mm_x(120.0), 0.3)]);
        assert!(s.actions.contains(&GestureAction::DragEnded { at: PanelPoint::new(0.3 + mm_x(20.0), 0.3) }));
        assert_eq!(
            s.actions.last(),
            Some(&GestureAction::Tap { at: PanelPoint::new(0.3 + mm_x(120.0), 0.3), count: 1 })
        );
    }
}
