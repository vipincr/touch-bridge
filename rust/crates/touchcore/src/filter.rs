//! The jump filter: position repair for the single-contact stream a WCH panel sends while in
//! mouse emulation, the only touch data available until the digitizer-mode switch takes hold.

use smallvec::{SmallVec, smallvec};

use crate::report::{Contact, Source, TouchFrame};

/// A frame of the panel's single-contact stream, in millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Seen {
    x: f64,
    y: f64,
    at_ns: u64,
    id: u8,
    scan_time: Option<u32>,
}

/// Where the finger is known to be, when it was there, and how fast it was moving (mm/s).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Finger {
    at: Seen,
    vx: f64,
    vy: f64,
}

/// The speed of a finger that is standing still: the panel's reports of one wander by up to
/// 0.7 mm a frame.
const REST_SPEED_MM_S: f64 = 50.0;

/// Position repair for the single-contact stream a WCH panel sends while in mouse
/// emulation, the only touch data available until the digitizer-mode switch takes hold.
///
/// Measured on three recordings of the JN-MD-IPST105FP (2026-11-24 and 2026-11-25, 2 100
/// reports): 7 frames in 100 with the tip down carry a position 10 to 200 mm from the finger,
/// three of them in four on the finger's own row; 6 in 100 are followed by one to eight tip-off
/// reports while the finger is still down; and where a finger lands or leaves, the panel reports
/// one to seven frames of a touch somewhere else. Taken one at a time, they ended touches that
/// had not ended, began ones that never happened, and put the finger where it never was: a drag
/// broke or jumped, and the touches around a lift landed as clicks wherever they were. So no
/// position is taken from one frame.
///
/// A frame is believed at once when it lies within [`Self::trust_mm`] of where the finger was
/// last seen, plus its speed times the time since, for at most [`Self::reach_ns`]. Any other
/// frame is held and opens a chain, which is believed only with support:
///
/// - A chain that keeps to its first frame is the finger standing somewhere else. It is believed
///   on its third frame, if that place is within the finger's reach (what
///   [`Self::acceleration_mm_s2`] covers from where the finger was and how fast it was moving)
///   and the finger could have come to rest there.
/// - A chain that moves on, each step changing speed by no more than that acceleration allows,
///   is believed on its second frame under the same condition on its first frame. A finger
///   leaving a place, a chain's first frame included, moves no faster in that step than
///   [`Self::start_speed_mm_s`].
/// - A chain is believed on its own once it has [`Self::persist_frames`] frames, or once that
///   many frames have passed without the finger being seen and two of them agree.
/// - A new press, with no finger to judge it against, is believed on its third frame, or on its
///   second when the panel reports its lift. One to four frames of a touch somewhere else come
///   just before a finger lands (probe 2026-11-24 17:20, 4.329 s: two frames 55 mm from where
///   the finger landed 70 ms later), and believed on two they were a click of their own.
///
/// While the finger is down and no lift report has come, a chain it could not have got to is
/// never believed on its own, however long the panel keeps to it. The panel kept to a misread for
/// seven frames in a row (title-bar 2026-11-25, 7.322 s: three frames 51 mm from a resting
/// finger, then four that moved on from there), and believing them on the fifth moved a scroll
/// 51 mm. After [`Self::lost_frames`] such frames the finger is given up for lost, and a place is
/// believed wherever it is: the alternative is a touch ignored until the finger leaves.
///
/// Once a lift report has come (the tracker is then holding the contact, see [`crate::stream`]),
/// only belief on a chain's own counts: around a lift the panel's other places are its misreads,
/// in runs of up to four frames. The same is asked for [`Self::reach_ns`] after a contact has
/// ended of a press reported farther than [`Self::trust_mm`] from where the last one was.
///
/// When a chain is believed, all its frames are passed on in order, so a fast swipe loses nothing
/// and is one frame late. A tip-off report is passed on as it is: whether it is a lift is the
/// tracker's decision, and a finger that comes back is judged against where it was.
/// [`SingleTouchFilter::contact_ended`] is called when a contact ends and [`Self::reset`] when
/// the panel has been released.
///
/// Identity is not this type's business: lifts and flaps live in
/// [`crate::tracker::ContactTracker`]. Digitizer frames pass through untouched and reset the
/// repair state.
#[derive(Debug)]
pub struct SingleTouchFilter {
    /// How far a frame may lie from where the finger was heading, and be believed at once.
    pub trust_mm: f64,
    /// How hard the finger can change speed or direction, mm/s². A swipe from rest takes at
    /// least 35 m/s² (measured: 8.9 mm in one 16 ms frame), and the default leaves it room.
    pub acceleration_mm_s2: f64,
    /// How fast the finger can already be moving on the first step of a press (measured:
    /// under 500 mm/s).
    pub start_speed_mm_s: f64,
    /// How fast a finger moves at all.
    pub max_speed_mm_s: f64,
    /// How far ahead of the finger's last sighting, or of a contact's end, the panel's reports
    /// are taken at face value. The finger's course and reach are worked out for this long, and
    /// a place is held to the finger for this long after a contact; past it the panel has been
    /// reporting places that cannot be believed, and a longer reach, a longer straight line or
    /// a longer memory would believe them.
    pub reach_ns: u64,
    /// Frames without a believed frame, or on one place, before a chain is believed on its own
    /// (measured: the panel's runs of other places are up to four frames long, a finger seen
    /// again after a flap is back for five).
    pub persist_frames: usize,
    /// Frames the panel has reported out of the finger's reach, with no lift report, before the
    /// finger is given up for lost (measured: seven, in the run described on the type; the
    /// tracker holds a lift for 250 ms, which is fifteen).
    pub lost_frames: usize,

    finger: Option<Finger>,
    chain: SmallVec<[Seen; 8]>,
    /// The chain's first step, from the finger to its first frame.
    entry: Option<(f64, f64)>,
    /// The chain's last step, once it left its first frame.
    link: Option<(f64, f64)>,
    /// Whether every frame of the chain lies within `trust_mm` of its first.
    staying: bool,
    /// Frames dropped since the finger was last believed.
    unconfirmed: usize,
    /// When the last frame with the tip down arrived, to drop a chain that was left hanging.
    last_tip_down_ns: Option<u64>,
    /// A lift report has come for the finger and the finger has not been seen since.
    lifted: bool,
    /// Where the contact that ended was, and when it ended.
    after: Option<Seen>,
    /// The panel's active area, as the last frame gave it, to put positions back in units of it.
    panel_mm: (f64, f64),
}

impl SingleTouchFilter {
    pub fn new(
        trust_mm: f64,
        acceleration_mm_s2: f64,
        start_speed_mm_s: f64,
        max_speed_mm_s: f64,
        reach_ns: u64,
        persist_frames: usize,
        lost_frames: usize,
    ) -> Self {
        Self {
            trust_mm,
            acceleration_mm_s2,
            start_speed_mm_s,
            max_speed_mm_s,
            reach_ns,
            persist_frames,
            lost_frames,
            finger: None,
            chain: SmallVec::new(),
            entry: None,
            link: None,
            staying: true,
            unconfirmed: 0,
            last_tip_down_ns: None,
            lifted: false,
            after: None,
            panel_mm: (0.0, 0.0),
        }
    }

    /// Forgets everything: the panel has been released, or its display has changed under it.
    pub fn reset(&mut self) {
        self.forget();
        self.last_tip_down_ns = None;
        self.after = None;
    }

    /// The contact is over, its last frame being where `ended` says (in millimetres). The panel
    /// goes on misreading around its lift for a few frames after that, so where it was is
    /// remembered for [`Self::reach_ns`].
    pub fn contact_ended(&mut self, at_ns: u64, ended: (f64, f64)) {
        self.forget();
        self.after = Some(Seen { x: ended.0, y: ended.1, at_ns, id: 0, scan_time: None });
    }

    fn forget(&mut self) {
        self.forget_chain();
        self.finger = None;
        self.lifted = false;
    }

    /// The frames to feed the tracker instead of `frame`: none while a frame waits for
    /// support, the frame itself when it is believed at once, and a whole chain of them when
    /// one is believed.
    pub fn filter(
        &mut self,
        frame: TouchFrame,
        at_ns: u64,
        panel_width_mm: f64,
        panel_height_mm: f64,
    ) -> SmallVec<[TouchFrame; 8]> {
        if frame.source != Source::Mouse {
            self.reset();
            return smallvec![frame];
        }
        let Some(contact) = frame.contacts.first().copied() else {
            return smallvec![frame];
        };
        if self.after.is_some_and(|last| at_ns.saturating_sub(last.at_ns) > self.reach_ns) {
            self.after = None;
        }
        if !contact.touching {
            // A press of two frames is confirmed by the report of its lift, while the pair is
            // still hanging. After a contact has ended the panel reports places of its own, and
            // there the pair waits like any other chain.
            let mut frames = smallvec![];
            let pair = self.chain.len() == 2 && at_ns.saturating_sub(self.chain[1].at_ns) <= self.reach_ns;
            if self.finger.is_none() && pair && !self.elsewhere(self.chain[0]) {
                frames = self.believe();
            }
            // A lift report, or a flap: whether the contact ends is the tracker's decision, and
            // a finger that comes back is judged against where it was.
            self.lifted |= self.finger.is_some();
            frames.push(frame);
            return frames;
        }
        self.panel_mm = (panel_width_mm, panel_height_mm);
        let seen = Seen {
            x: contact.x * panel_width_mm,
            y: contact.y * panel_height_mm,
            at_ns,
            id: contact.id,
            scan_time: frame.scan_time,
        };
        // A chain left hanging for this long belonged to a touch that is over (a frame or three
        // the panel reported after a lift): nothing is coming to support it.
        if self.last_tip_down_ns.is_some_and(|at| seen.at_ns.saturating_sub(at) > self.reach_ns) {
            self.forget_chain();
        }
        self.last_tip_down_ns = Some(seen.at_ns);
        if let Some(finger) = self.finger {
            // Where the finger was heading, over no more of the gap than its reach is known for:
            // further on, a place that happens to lie on that line is no evidence.
            let held = seconds((seen.at_ns.saturating_sub(finger.at.at_ns)).min(self.reach_ns));
            let ahead = Seen { x: finger.at.x + finger.vx * held, y: finger.at.y + finger.vy * held, ..seen };
            if distance(seen, ahead) <= self.trust_mm {
                self.chain.clear();
                self.unconfirmed = 0;
                self.lifted = false;
                self.finger = Some(finger.moved_to(seen));
                return smallvec![seen.frame(self.panel_mm)];
            }
        }
        self.unconfirmed += 1;
        self.extend(seen);
        self.settle()
    }

    fn forget_chain(&mut self) {
        self.chain.clear();
        self.entry = None;
        self.link = None;
        self.staying = true;
        self.unconfirmed = 0;
    }

    /// Puts `seen` at the end of the chain when it goes on from there the way a finger can,
    /// and starts a new chain with it otherwise.
    fn extend(&mut self, seen: Seen) {
        let Some(last) = self.chain.last().copied() else {
            self.open(seen);
            return;
        };
        if self.staying && distance(seen, self.chain[0]) <= self.trust_mm {
            self.chain.push(seen);
            return;
        }
        // A chain that has stood still for two frames is at rest, and so is one that has come to
        // a stop on the way: its last step was no more than a resting finger wanders. A moving
        // one keeps its last step; a single frame has only the step that led to it.
        let rested = self.link.is_some_and(|link| speed(link) <= REST_SPEED_MM_S);
        let at_rest = (self.staying && self.chain.len() >= 2) || rested;
        let was = self.link.or_else(|| at_rest.then_some((0.0, 0.0))).or(self.entry);
        let step = step(last, seen);
        // A finger leaves a place no faster than it lands and starts: the first step of a press
        // and the first step out of a resting chain are held to the same speed. (Measured,
        // 2026-11-24: three frames of a misread at rest, then one 17 mm on from them at 900 mm/s.
        // Taken with them, the chain ran on into the finger's own frames and was believed. And
        // 2026-11-25: a run that stopped for two frames, then the finger 29 mm back at 856 mm/s.)
        let starts = at_rest || (self.finger.is_none() && was.is_none());
        let fastest = if starts { self.start_speed_mm_s } else { self.max_speed_mm_s };
        let turned = was.is_none_or(|was| {
            speed((step.0 - was.0, step.1 - was.1))
                <= self.acceleration_mm_s2 * seconds(seen.at_ns.saturating_sub(last.at_ns))
        });
        if !(speed(step) <= fastest && turned) {
            self.open(seen);
            return;
        }
        self.link = Some(step);
        self.staying = false;
        self.chain.push(seen);
    }

    /// Starts a new chain with `seen` as its first frame.
    fn open(&mut self, seen: Seen) {
        self.chain.clear();
        self.chain.push(seen);
        self.entry = self.finger.map(|finger| step(finger.at, seen));
        self.link = None;
        self.staying = true;
    }

    /// Whether the chain is believed, and its frames if so.
    fn settle(&mut self) -> SmallVec<[TouchFrame; 8]> {
        let Some(first) = self.chain.first().copied() else {
            return smallvec![];
        };
        let (reachable, at_rest) = match self.finger {
            None => (true, true),
            Some(finger) => {
                let held = seconds(first.at_ns.saturating_sub(finger.at.at_ns)).min(seconds(self.reach_ns));
                let ahead = Seen { x: finger.at.x + finger.vx * held, y: finger.at.y + finger.vy * held, ..first };
                let entry = self.entry.unwrap_or_default();
                let leaving = speed((entry.0 - finger.vx, entry.1 - finger.vy));
                // A chain that stands still must have come to rest as a finger does: its arrival
                // speed is gone by its second frame.
                let stopped = !self.staying
                    || self.chain.len() < 2
                    || speed(entry) <= self.acceleration_mm_s2 * seconds(self.chain[1].at_ns.saturating_sub(first.at_ns)).max(0.001);
                (
                    distance(first, ahead) <= self.trust_mm + 0.5 * self.acceleration_mm_s2 * held * held
                        && leaving <= self.acceleration_mm_s2 * held,
                    stopped,
                )
            }
        };
        let frames = self.chain.len();
        let standing = self.staying && self.finger.is_some();
        let needed = if standing { 3 } else { 2 };
        let unsupported = self.lifted || self.elsewhere(first);
        // A new press waits for its third frame, or for the report of its lift (`filter`).
        if self.finger.is_none() && !unsupported && frames < 3 {
            return smallvec![];
        }
        let supported = !unsupported && reachable && at_rest && frames >= needed;
        // While the finger is down and no lift report has come, a place it could not have got to
        // is a misread, however long the panel keeps to it: believed on its own only once the
        // finger is given up for lost.
        let down = self.finger.is_some() && !self.lifted;
        let lost = self.unconfirmed >= self.lost_frames;
        let out_of_reach = down && !self.attainable(first) && !lost;
        let persisted = frames >= self.persist_frames && !out_of_reach;
        let given_up = !unsupported && !out_of_reach && self.unconfirmed >= self.persist_frames && frames >= 2;
        if !(supported || persisted || given_up) {
            return smallvec![];
        }
        self.believe()
    }

    /// Whether a finger at its last sighting could have got to `place` by the time of it: within
    /// a trust distance, or a step no longer than its speed and what acceleration adds over the
    /// time. The finger may have stopped or turned there, so its heading is not followed.
    fn attainable(&self, place: Seen) -> bool {
        let Some(finger) = self.finger else { return true };
        let held = seconds(place.at_ns.saturating_sub(finger.at.at_ns)).min(seconds(self.reach_ns));
        distance(place, finger.at) <= self.trust_mm + (speed((finger.vx, finger.vy)) + self.acceleration_mm_s2 * held) * held
    }

    /// Whether the chain that starts at `first` is a touch somewhere else than the one that just
    /// ended, within [`Self::reach_ns`] of its end: the panel's places around a lift come in runs
    /// of up to four frames, wherever the finger was last seen.
    fn elsewhere(&self, first: Seen) -> bool {
        self.after.is_some_and(|last| distance(first, last) > self.trust_mm)
    }

    /// Takes the chain for the finger: every frame of it is passed on, and the repair starts over
    /// from its last frame.
    fn believe(&mut self) -> SmallVec<[TouchFrame; 8]> {
        let chain = std::mem::take(&mut self.chain);
        let last = chain[chain.len() - 1];
        let (vx, vy) = if self.staying || chain.len() < 2 {
            (0.0, 0.0)
        } else {
            step(chain[chain.len() - 2], last)
        };
        self.finger = Some(Finger { at: last, vx, vy });
        self.entry = None;
        self.link = None;
        self.staying = true;
        self.unconfirmed = 0;
        self.lifted = false;
        self.after = None;
        chain.iter().map(|frame| frame.frame(self.panel_mm)).collect()
    }
}

impl Default for SingleTouchFilter {
    fn default() -> Self {
        Self::new(6.0, 50_000.0, 750.0, 2_500.0, 50_000_000, 5, 15)
    }
}

impl Finger {
    /// The finger after moving to `seen`.
    fn moved_to(self, seen: Seen) -> Self {
        let (vx, vy) = if seen.at_ns > self.at.at_ns { step(self.at, seen) } else { (self.vx, self.vy) };
        Self { at: seen, vx, vy }
    }
}

impl Seen {
    /// The frame as the tracker takes it: positions in units of the panel.
    fn frame(self, panel_mm: (f64, f64)) -> TouchFrame {
        TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact {
                id: self.id,
                touching: true,
                x: self.x / panel_mm.0,
                y: self.y / panel_mm.1,
            }],
            scan_time: self.scan_time,
        }
    }
}

/// The distance between two frames' positions, in mm.
fn distance(a: Seen, b: Seen) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// The speed that takes the finger from `from` to `to`, in mm/s.
fn step(from: Seen, to: Seen) -> (f64, f64) {
    let dt = seconds(to.at_ns.saturating_sub(from.at_ns)).max(0.001);
    ((to.x - from.x) / dt, (to.y - from.y) / dt)
}

fn speed(v: (f64, f64)) -> f64 {
    v.0.hypot(v.1)
}

fn seconds(ns: u64) -> f64 {
    ns as f64 / 1e9
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Source;

    const WIDTH: f64 = 221.9;
    const HEIGHT: f64 = 147.9;
    const MS: u64 = 1_000_000;

    fn at(x: f64, y: f64) -> (f64, f64) {
        (x / WIDTH, y / HEIGHT)
    }

    fn mouse(x: f64, y: f64, touching: bool) -> TouchFrame {
        let (x, y) = at(x, y);
        TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact { id: 0, touching, x, y }],
            scan_time: None,
        }
    }

    /// Feeds one report in millimetres and returns where the frames it passes on put the
    /// finger.
    fn feed(filter: &mut SingleTouchFilter, x: f64, y: f64, at_ns: u64) -> Vec<(f64, f64)> {
        filter
            .filter(mouse(x, y, true), at_ns, WIDTH, HEIGHT)
            .iter()
            .map(|frame| (frame.contacts[0].x * WIDTH, frame.contacts[0].y * HEIGHT))
            .collect()
    }

    /// A finger that has rested at (`x`, `y`) since time 0: believed, and at rest.
    fn settled(x: f64, y: f64) -> (SingleTouchFilter, u64) {
        let mut filter = SingleTouchFilter::default();
        for index in 0..3 {
            let _ = feed(&mut filter, x, y, index * 17 * MS);
        }
        (filter, 34 * MS)
    }

    #[test]
    fn a_press_is_believed_on_its_third_frame_with_all_three() {
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 100.0, 70.0, 0).is_empty(), "a press waits for support");
        assert!(feed(&mut filter, 100.0, 70.0, 17 * MS).is_empty(), "two frames agree, and may still be the panel's own");
        assert_eq!(feed(&mut filter, 100.0, 70.0, 34 * MS), vec![(100.0, 70.0), (100.0, 70.0), (100.0, 70.0)]);
    }

    // Probe 2026-11-24 17:20, 4.308 s to 4.425 s: after a tap, one frame of a place, two frames
    // of a touch 55 mm from where the finger landed, two of another place, the lift report, and
    // the finger 3 mm on from the last. Believed on their second frame, the first pair was a click
    // of its own.
    #[test]
    fn a_press_of_two_frames_is_confirmed_by_the_report_of_its_lift() {
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 50.0, 11.6, 0).is_empty());
        assert!(feed(&mut filter, 122.0, 11.5, 21 * MS).is_empty());
        assert!(feed(&mut filter, 122.0, 11.5, 37 * MS).is_empty(), "two frames agree, and are not the finger");
        assert!(feed(&mut filter, 62.3, 11.6, 49 * MS).is_empty(), "the panel went on to another place: the pair is dropped");
        assert!(feed(&mut filter, 62.3, 11.6, 66 * MS).is_empty());
        let lift = filter.filter(mouse(62.3, 11.6, false), 83 * MS, WIDTH, HEIGHT);
        assert_eq!(
            lift.to_vec(),
            vec![mouse(62.3, 11.6, true), mouse(62.3, 11.6, true), mouse(62.3, 11.6, false)]
        );
        // The finger lands 3 mm on: the same press goes on.
        assert_eq!(feed(&mut filter, 65.1, 11.8, 117 * MS), vec![(65.1, 11.8)]);

        // A pair that has been left hanging is not a press when a lift report comes long after.
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 62.3, 11.6, 0).is_empty());
        assert!(feed(&mut filter, 62.3, 11.6, 17 * MS).is_empty());
        let lift = filter.filter(mouse(62.3, 11.6, false), 400 * MS, WIDTH, HEIGHT);
        assert_eq!(lift.to_vec(), vec![mouse(62.3, 11.6, false)]);
    }

    #[test]
    fn a_touch_of_one_frame_is_not_a_press() {
        // Probe 2026-11-25 12.310 s: after a lift the panel reports one frame of a touch 9 mm
        // from the last one. It landed as a click.
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 48.8, 37.2, 0).is_empty());
        assert!(feed(&mut filter, 48.8, 37.2, 400 * MS).is_empty(), "the next frame is too late to be the same touch");
    }

    // Four frames of a place, a gap longer than the panel leaves between two frames of one touch
    // (38 ms at the most), then a fifth: the run was left hanging, and the fifth frame starts
    // another chain. Counted with the four, it made the five that are believed on their own.
    #[test]
    fn a_chain_left_hanging_is_forgotten() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        for index in 0..4 {
            assert!(feed(&mut filter, 170.0, 70.0, t + 17 * MS * (index + 2)).is_empty());
        }
        // Sixty milliseconds after the fourth frame.
        for index in 0..4 {
            assert!(feed(&mut filter, 170.0, 70.0, t + 145 * MS + 17 * MS * index).is_empty(), "{index}");
        }
        assert_eq!(feed(&mut filter, 170.0, 70.0, t + 145 * MS + 17 * MS * 4).len(), 5);
    }

    // The finger was last seen 91 ms before this chain begins, and nothing says it lifted. Its
    // reach is worked out for 50 ms, as it is for a frame taken at once. Without that limit the
    // chain here (80 mm on, moving on from there) is within reach, and is believed on its second
    // frame: at 50 m/s² a finger covers 207 mm in 91 ms, and 62 mm in 50.
    #[test]
    fn the_fingers_reach_is_worked_out_for_no_longer_than_its_horizon() {
        let (mut filter, t) = settled(100.0, 70.0);
        assert!(feed(&mut filter, 40.0, 20.0, t + 17 * MS).is_empty());
        assert!(feed(&mut filter, 60.0, 10.0, t + 34 * MS).is_empty());
        assert!(feed(&mut filter, 180.0, 70.0, t + 91 * MS).is_empty());
        assert!(feed(&mut filter, 188.0, 70.0, t + 108 * MS).is_empty(), "two frames agree, out of the finger's reach");
    }

    #[test]
    fn a_frame_near_the_finger_is_believed_at_once() {
        let (mut filter, t) = settled(100.0, 70.0);
        assert_eq!(feed(&mut filter, 102.0, 70.0, t + 17 * MS), vec![(102.0, 70.0)]);
        assert_eq!(feed(&mut filter, 104.0, 70.5, t + 34 * MS), vec![(104.0, 70.5)]);
    }

    #[test]
    fn a_wild_frame_is_dropped_and_the_finger_goes_on_from_where_it_was() {
        // Probe 2026-11-25 0.419 s: three frames 101 mm to the left of a slow finger, one 40 mm
        // on from them, then the finger again.
        let (mut filter, t) = settled(163.0, 13.0);
        assert!(feed(&mut filter, 61.4, 11.5, t + 17 * MS).is_empty());
        assert!(feed(&mut filter, 61.4, 11.5, t + 34 * MS).is_empty());
        assert!(feed(&mut filter, 61.4, 11.5, t + 51 * MS).is_empty());
        assert!(feed(&mut filter, 123.7, 11.5, t + 68 * MS).is_empty());
        assert_eq!(feed(&mut filter, 161.0, 13.3, t + 85 * MS), vec![(161.0, 13.3)]);
    }

    #[test]
    fn a_touch_reported_somewhere_else_after_a_lift_is_dropped() {
        // Probe 2026-11-25 12.149 s to 12.324 s: the tip-off, three frames of a touch 107 mm to
        // the left, the tip-off, and one frame 9 mm on from there. Each was a click on the
        // desktop, which put Telegram behind Finder.
        let (mut filter, t) = settled(147.5, 39.0);
        for at in [t + 17 * MS, t + 107 * MS, t + 173 * MS] {
            let lift = filter.filter(mouse(147.5, 39.0, false), at, WIDTH, HEIGHT);
            assert_eq!(lift.to_vec(), vec![mouse(147.5, 39.0, false)]);
        }
        assert!(feed(&mut filter, 39.8, 39.2, t + 55 * MS).is_empty());
        assert!(feed(&mut filter, 39.8, 39.2, t + 73 * MS).is_empty());
        assert!(feed(&mut filter, 39.8, 39.2, t + 90 * MS).is_empty());
        assert!(feed(&mut filter, 48.8, 37.2, t + 159 * MS).is_empty());
    }

    #[test]
    fn a_fast_swipe_from_rest_is_believed_one_frame_late_with_all_of_its_frames() {
        let (mut filter, t) = settled(100.0, 70.0);
        assert!(feed(&mut filter, 110.0, 70.0, t + 17 * MS).is_empty(), "the first step waits");
        assert_eq!(feed(&mut filter, 122.0, 70.0, t + 34 * MS), vec![(110.0, 70.0), (122.0, 70.0)]);
        assert_eq!(feed(&mut filter, 134.0, 70.0, t + 51 * MS), vec![(134.0, 70.0)]);
        assert_eq!(feed(&mut filter, 146.0, 70.0, t + 68 * MS), vec![(146.0, 70.0)]);
    }

    #[test]
    fn a_wild_frame_inside_a_fast_swipe_does_not_break_it() {
        // Two frames 77 mm behind a swipe at 700 mm/s: dropped, and the swipe is taken up again
        // one frame later.
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = feed(&mut filter, 110.0, 70.0, t + 17 * MS);
        let _ = feed(&mut filter, 122.0, 70.0, t + 34 * MS);
        assert_eq!(feed(&mut filter, 134.0, 70.0, t + 51 * MS), vec![(134.0, 70.0)]);
        assert!(feed(&mut filter, 57.0, 70.0, t + 68 * MS).is_empty());
        assert!(feed(&mut filter, 57.0, 70.0, t + 85 * MS).is_empty());
        assert!(feed(&mut filter, 146.0, 70.0, t + 102 * MS).is_empty());
        assert_eq!(feed(&mut filter, 158.0, 70.0, t + 119 * MS), vec![(146.0, 70.0), (158.0, 70.0)]);
    }

    #[test]
    fn a_swipe_that_starts_with_a_wild_frame_begins_where_the_finger_is() {
        // Probe 2026-11-25 12.310 s and 12.361 s: a frame 98 mm from where the finger lands.
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 48.8, 37.2, 0).is_empty());
        assert!(feed(&mut filter, 147.0, 40.4, 51 * MS).is_empty(), "nothing agrees with either frame yet");
        assert!(feed(&mut filter, 147.0, 40.4, 68 * MS).is_empty(), "two frames, and the first place may have been the panel's");
        assert_eq!(feed(&mut filter, 147.0, 40.4, 85 * MS), vec![(147.0, 40.4), (147.0, 40.4), (147.0, 40.4)]);
    }

    #[test]
    fn a_finger_that_comes_back_after_a_flap_is_judged_against_where_it_was() {
        // Probe 2026-11-25 25.745 s: two tip-off reports, three frames of a stale place 118 mm
        // away, then the finger.
        let (mut filter, t) = settled(100.0, 70.0);
        for at in [t + 17 * MS, t + 33 * MS] {
            let lift = filter.filter(mouse(100.0, 70.0, false), at, WIDTH, HEIGHT);
            assert_eq!(lift.to_vec(), vec![mouse(100.0, 70.0, false)]);
        }
        assert!(feed(&mut filter, 218.0, 70.0, t + 50 * MS).is_empty());
        assert!(feed(&mut filter, 218.0, 70.0, t + 67 * MS).is_empty());
        assert!(feed(&mut filter, 218.0, 70.0, t + 84 * MS).is_empty());
        assert_eq!(feed(&mut filter, 100.5, 70.0, t + 103 * MS), vec![(100.5, 70.0)]);
    }

    // The same flap (probe 2026-11-25 25.778 s to 25.867 s), with frames 33 mm back that move
    // on from there. The panel reports places of its own around a lift (up to four frames of
    // them), so after a lift report a frame off the finger's course counts only once it has
    // persisted: all five are passed on together then.
    #[test]
    fn a_finger_off_its_course_after_a_lift_report_is_believed_once_it_has_persisted() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 33 * MS, WIDTH, HEIGHT);
        let course = [(67.0, 70.0), (68.1, 70.0), (72.8, 70.0), (78.0, 70.0)];
        for (index, &(x, y)) in course.iter().enumerate() {
            assert!(feed(&mut filter, x, y, t + 17 * MS * (index as u64 + 4)).is_empty(), "{x}");
        }
        assert_eq!(
            feed(&mut filter, 84.0, 70.0, t + 17 * MS * 8),
            vec![(67.0, 70.0), (68.1, 70.0), (72.8, 70.0), (78.0, 70.0), (84.0, 70.0)]
        );
        assert_eq!(feed(&mut filter, 90.0, 70.0, t + 17 * MS * 9), vec![(90.0, 70.0)]);
    }

    // Probe 2026-11-25 19.414 s to 19.503 s: after the lift report one frame 80 mm away, then
    // four frames of a touch 12 mm beside the finger. Not the finger moving on: believed on
    // their third frame, they were a click of their own.
    #[test]
    fn a_touch_beside_a_lift_report_is_not_the_finger_moving_on() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 180.0, 70.0, t + 34 * MS).is_empty());
        for index in 0..4 {
            assert!(feed(&mut filter, 112.0, 70.0, t + 17 * MS * (index + 3)).is_empty(), "{index}");
        }
        // Persisting is another matter: five frames on one place are a finger.
        assert_eq!(feed(&mut filter, 112.0, 70.0, t + 17 * MS * 7).len(), 5);
        assert_eq!(feed(&mut filter, 113.0, 70.0, t + 17 * MS * 8), vec![(113.0, 70.0)]);
    }

    // Where the finger was heading is still where it is: a swipe that flaps is taken up at once,
    // and from then on a frame off its course is judged by reach again.
    #[test]
    fn a_finger_back_on_its_course_after_a_lift_report_is_believed_at_once() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = feed(&mut filter, 105.0, 70.0, t + 17 * MS);
        let _ = feed(&mut filter, 110.0, 70.0, t + 34 * MS);
        let _ = filter.filter(mouse(110.0, 70.0, false), t + 51 * MS, WIDTH, HEIGHT);
        assert_eq!(feed(&mut filter, 120.0, 70.0, t + 68 * MS), vec![(120.0, 70.0)]);
        assert_eq!(feed(&mut filter, 122.0, 70.0, t + 85 * MS), vec![(122.0, 70.0)]);
        assert_eq!(feed(&mut filter, 127.0, 70.0, t + 102 * MS), vec![(127.0, 70.0)]);
        assert_eq!(feed(&mut filter, 127.0, 70.0, t + 119 * MS), vec![(127.0, 70.0)]);
        assert!(feed(&mut filter, 137.0, 70.0, t + 136 * MS).is_empty());
        assert!(feed(&mut filter, 137.0, 70.0, t + 153 * MS).is_empty());
        assert_eq!(
            feed(&mut filter, 137.0, 70.0, t + 170 * MS),
            vec![(137.0, 70.0), (137.0, 70.0), (137.0, 70.0)]
        );
    }

    // Probe 2026-11-24 7.449 s to 7.571 s: three frames of a misread at rest 93 mm from the
    // finger, then one 17 mm on from them, then the finger 108 mm on again. Each step was within
    // an acceleration of the last, so the chain ran from the misread into the finger's own frames
    // and was believed whole: a drag ended where the finger was and began again at the misread.
    // A finger does not leave a place at 900 mm/s.
    #[test]
    fn a_place_does_not_run_on_into_the_finger_faster_than_a_finger_starts() {
        // The finger sweeping up to the right at 500 mm/s, its last three frames.
        let (mut filter, t) = settled(91.5, 70.0);
        assert!(feed(&mut filter, 99.5, 70.0, t + 17 * MS).is_empty());
        assert_eq!(feed(&mut filter, 107.9, 70.0, t + 34 * MS), vec![(99.5, 70.0), (107.9, 70.0)]);
        assert!(feed(&mut filter, 14.0, 70.0, t + 54 * MS).is_empty());
        assert!(feed(&mut filter, 14.0, 70.0, t + 73 * MS).is_empty());
        assert!(feed(&mut filter, 14.0, 70.0, t + 92 * MS).is_empty());
        assert!(feed(&mut filter, 32.0, 70.0, t + 111 * MS).is_empty(), "too fast out of rest: a chain of its own");
        let _ = filter.filter(mouse(32.0, 70.0, false), t + 125 * MS, WIDTH, HEIGHT);
        let _ = filter.filter(mouse(32.0, 70.0, false), t + 140 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 107.5, 70.0, t + 157 * MS).is_empty());
        assert!(feed(&mut filter, 106.3, 70.0, t + 176 * MS).is_empty());
        assert!(feed(&mut filter, 101.7, 70.0, t + 193 * MS).is_empty());
        assert!(feed(&mut filter, 95.1, 70.0, t + 212 * MS).is_empty());
        // Five frames of the finger on its own: believed, and none of the misread with them.
        let frames = feed(&mut filter, 87.1, 70.0, t + 229 * MS);
        assert_eq!(frames.first(), Some(&(107.5, 70.0)));
        assert_eq!(frames.len(), 5);
        assert_eq!(feed(&mut filter, 82.4, 70.0, t + 246 * MS), vec![(82.4, 70.0)]);
    }

    // Probe 2026-11-24 7.799 s: two frames of a touch 75 mm from the finger, 30 ms after the
    // tracker gave the contact up. A new press by the two-frame rule, it was a tap of its own.
    #[test]
    fn a_touch_far_from_the_contact_that_just_ended_persists_before_it_counts() {
        let mut filter = SingleTouchFilter::default();
        filter.contact_ended(0, (82.0, 70.0));
        assert!(feed(&mut filter, 7.0, 70.0, 29 * MS).is_empty());
        assert!(feed(&mut filter, 7.0, 70.0, 46 * MS).is_empty(), "two frames agree, but not near the last touch");
        // Fifty milliseconds past the end the panel is taken at its word again: the three frames are a press.
        assert_eq!(feed(&mut filter, 7.0, 70.0, 63 * MS), vec![(7.0, 70.0), (7.0, 70.0), (7.0, 70.0)]);

        // Inside that time only persistence counts.
        let mut filter = SingleTouchFilter::default();
        filter.contact_ended(0, (82.0, 70.0));
        for at in [5, 15, 25, 35] {
            assert!(feed(&mut filter, 7.0, 70.0, at * MS).is_empty(), "{at} ms");
        }
        assert_eq!(feed(&mut filter, 7.0, 70.0, 45 * MS).len(), 5);

        // Near the last touch a press is a press, on its third frame like any other.
        let mut filter = SingleTouchFilter::default();
        filter.contact_ended(0, (82.0, 70.0));
        assert!(feed(&mut filter, 83.0, 70.0, 29 * MS).is_empty());
        assert!(feed(&mut filter, 83.0, 70.0, 46 * MS).is_empty());
        assert_eq!(feed(&mut filter, 83.0, 70.0, 63 * MS), vec![(83.0, 70.0), (83.0, 70.0), (83.0, 70.0)]);
        // And so is one anywhere, once the panel has had [`SingleTouchFilter::reach_ns`] to settle.
        let mut filter = SingleTouchFilter::default();
        filter.contact_ended(0, (9.0, 70.0));
        assert!(feed(&mut filter, 90.0, 70.0, 34 * MS).is_empty());
        assert!(feed(&mut filter, 90.0, 70.0, 46 * MS).is_empty(), "still inside the horizon");
        assert_eq!(feed(&mut filter, 90.0, 70.0, 63 * MS), vec![(90.0, 70.0), (90.0, 70.0), (90.0, 70.0)]);
    }

    // The panel reports the tip off at its origin before a finger lands. With no finger to
    // lose, that is nothing: the landing is a press like any other.
    #[test]
    fn lift_reports_before_a_press_do_not_delay_it() {
        let mut filter = SingleTouchFilter::default();
        let _ = filter.filter(mouse(0.0, 0.0, false), 0, WIDTH, HEIGHT);
        let _ = filter.filter(mouse(180.0, 12.0, false), 17 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 60.0, 70.0, 34 * MS).is_empty());
        assert!(feed(&mut filter, 60.0, 70.0, 51 * MS).is_empty());
        assert_eq!(feed(&mut filter, 60.0, 70.0, 68 * MS), vec![(60.0, 70.0), (60.0, 70.0), (60.0, 70.0)]);
    }

    // Probe 2026-11-25 19.715 s to 19.928 s: a swipe to the left at 400 mm/s, then four frames
    // of places that move to the right, two of which lie where the finger would have been had
    // it kept straight on at that speed (124.8 after 88 ms, then 68.1 across six lift reports).
    // Taken for the finger, they were a touch of its own. A line is followed for 50 ms.
    #[test]
    fn a_place_on_the_fingers_line_far_ahead_is_not_the_finger() {
        let (mut filter, t) = settled(160.0, 70.0);
        assert_eq!(feed(&mut filter, 155.0, 70.0, t + 17 * MS), vec![(155.0, 70.0)]);
        assert_eq!(feed(&mut filter, 147.0, 70.0, t + 34 * MS), vec![(147.0, 70.0)]);
        assert!(feed(&mut filter, 58.0, 70.0, t + 51 * MS).is_empty());
        assert!(feed(&mut filter, 74.0, 70.0, t + 68 * MS).is_empty());
        assert!(feed(&mut filter, 93.0, 70.0, t + 88 * MS).is_empty());
        assert!(feed(&mut filter, 99.0, 70.0, t + 136 * MS).is_empty(), "on the line, 102 ms on");
        for index in 0..6 {
            let lift = filter.filter(mouse(99.0, 70.0, false), t + 17 * MS * (index + 9), WIDTH, HEIGHT);
            assert_eq!(lift.to_vec(), vec![mouse(99.0, 70.0, false)]);
        }
        assert!(feed(&mut filter, 68.0, 70.0, t + 17 * MS * 16).is_empty());

        // Right after the last sighting the line still counts: the swipe goes on from there.
        let (mut filter, t) = settled(160.0, 70.0);
        assert_eq!(feed(&mut filter, 155.0, 70.0, t + 17 * MS), vec![(155.0, 70.0)]);
        assert_eq!(feed(&mut filter, 150.0, 70.0, t + 34 * MS), vec![(150.0, 70.0)]);
        assert!(feed(&mut filter, 40.0, 70.0, t + 51 * MS).is_empty());
        assert_eq!(feed(&mut filter, 145.0, 70.0, t + 68 * MS), vec![(145.0, 70.0)]);
    }

    // Title-bar 2026-11-25, 7.307 s to 7.533 s: a finger at rest on a title bar, then three frames
    // of a place 51 mm to the right of it and four that move on from there, then the lift report,
    // and the finger 6 mm from where it had rested. Believed on their fifth frame, the run moved
    // the scroll 51 mm and back part of the way.
    #[test]
    fn a_run_of_places_out_of_the_fingers_reach_is_not_the_finger_while_it_is_down() {
        let (mut filter, t) = settled(158.3, 11.5);
        let run = [209.8, 209.8, 209.8, 203.0, 194.2, 194.2, 193.8];
        for (index, x) in run.iter().enumerate() {
            assert!(feed(&mut filter, *x, 11.5, t + 17 * MS * (index as u64 + 1)).is_empty(), "{x}");
        }
        let _ = filter.filter(mouse(193.8, 11.5, false), t + 17 * MS * 8, WIDTH, HEIGHT);
        // The finger comes back 6 mm from where it had rested, its first frame just out of reach
        // and the next within it: the press goes on from there, and none of the run with it.
        let back = [164.7, 164.1, 163.8, 164.3, 165.0];
        for (index, x) in back.iter().enumerate() {
            let frames = feed(&mut filter, *x, 11.5, t + 17 * MS * (index as u64 + 9));
            if index == 0 {
                assert!(frames.is_empty(), "the first frame waits");
            } else {
                assert_eq!(frames, vec![(*x, 11.5)]);
            }
        }
    }

    // Once the finger is given up for lost, a place is believed wherever it is: the alternative is
    // a touch that is ignored until the finger leaves the panel.
    #[test]
    fn the_finger_is_given_up_for_lost_after_lost_frames_out_of_its_reach() {
        let (mut filter, t) = settled(100.0, 70.0);
        // The default: fifteen frames.
        let lost = 15;
        assert_eq!(filter.lost_frames, lost);
        for index in 1..lost {
            assert!(feed(&mut filter, 170.0, 70.0, t + 17 * MS * index as u64).is_empty(), "{index} frames");
        }
        let frames = feed(&mut filter, 170.0, 70.0, t + 17 * MS * lost as u64);
        assert_eq!(frames.len(), lost);
    }

    // Once a lift report has come, a place is the finger if it stands still for five frames:
    // where the finger went from where it was is no longer known.
    #[test]
    fn a_finger_that_stands_elsewhere_long_enough_after_a_lift_report_is_believed() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        for index in 0..4 {
            assert!(feed(&mut filter, 170.0, 70.0, t + 17 * MS * (index + 2)).is_empty());
        }
        assert_eq!(feed(&mut filter, 170.0, 70.0, t + 17 * MS * 6).len(), 5);
        assert_eq!(feed(&mut filter, 171.0, 70.0, t + 17 * MS * 7), vec![(171.0, 70.0)]);
    }

    #[test]
    fn a_finger_that_is_never_seen_again_is_given_up_on() {
        // Seven frames, each a sharp turn from the last and none near where the finger was:
        // nothing supports a place, and the finger cannot be held where it was for ever.
        let (mut filter, t) = settled(100.0, 70.0);
        let zigzag = [(60.0, 70.0), (80.0, 60.0), (100.0, 50.0), (120.0, 60.0), (140.0, 70.0), (160.0, 60.0)];
        for (index, &(x, y)) in zigzag.iter().enumerate() {
            assert!(feed(&mut filter, x, y, t + 17 * MS * (index as u64 + 1)).is_empty());
        }
        assert_eq!(feed(&mut filter, 164.0, 60.0, t + 17 * MS * 7), vec![(160.0, 60.0), (164.0, 60.0)]);
    }

    #[test]
    fn reset_forgets_the_finger() {
        let (mut filter, t) = settled(100.0, 70.0);
        filter.reset();
        assert!(feed(&mut filter, 20.0, 20.0, t + 17 * MS).is_empty());
        assert!(feed(&mut filter, 20.0, 20.0, t + 34 * MS).is_empty());
        assert_eq!(feed(&mut filter, 20.0, 20.0, t + 51 * MS),
            vec![(20.0, 20.0), (20.0, 20.0), (20.0, 20.0)]);
    }

    #[test]
    fn digitizer_frames_pass_and_reset_the_repair() {
        let (mut filter, t) = settled(100.0, 70.0);
        let digitizer = TouchFrame {
            source: Source::Digitizer,
            contacts: smallvec![
                Contact { id: 1, touching: true, x: 0.1, y: 0.1 },
                Contact { id: 2, touching: true, x: 0.9, y: 0.9 },
            ],
            scan_time: Some(5),
        };
        assert_eq!(filter.filter(digitizer.clone(), t + 17 * MS, WIDTH, HEIGHT).to_vec(), vec![digitizer]);
        assert!(feed(&mut filter, 100.0, 70.0, t + 34 * MS).is_empty(), "the mouse stream starts over");
    }
}
