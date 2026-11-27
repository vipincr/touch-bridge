//! The jump filter: position repair for the single-contact stream a WCH panel sends while in
//! mouse emulation, the only touch data available until the digitizer-mode switch takes hold.

use smallvec::{SmallVec, smallvec};

use crate::report::{Contact, Source, TouchFrame};

/// How the touch that [`SingleTouchFilter`] last passed on began.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landing {
    /// Where its first frame was, in millimetres, and when it came.
    pub x: f64,
    pub y: f64,
    pub at_ns: u64,
    /// It began within [`SingleTouchFilter::storm_start_ns`] of the lift report before it, farther
    /// from where the finger lifted than that finger could have got by then: too soon after that
    /// lift for one finger to have made both. What lifted was the panel's misread of this landing.
    pub misread_before: bool,
}

/// What the panel reported in the moment after a lift, as the filter still sees it
/// ([`SingleTouchFilter::awaiting`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awaiting {
    /// Nothing that began then is still being judged.
    Nothing,
    /// A touch that began then is being judged, near enough to where the finger lifted to be that finger again.
    Judging,
    /// A touch that began then is being judged, too far from where the finger lifted to be the same
    /// finger so soon. If it proves to be a touch, what lifted before it was the panel's misread of it.
    WouldMisread,
}

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

/// A touch that began amid the storm after a lift and is waiting to be proven a finger: its
/// frames, and how they were going on (as a chain does).
#[derive(Debug)]
struct Unproven {
    frames: SmallVec<[Seen; 16]>,
    link: Option<(f64, f64)>,
    staying: bool,
    /// A frame with the tip down that was not this touch's, or its lift report, has come since
    /// its last frame: the next of its frames must lie where this touch was heading.
    broken: bool,
    /// When the panel last said anything, this touch's frames included.
    last_frame_ns: u64,
    /// When a lift report came from where its last frame was.
    lifted_ns: Option<u64>,
}

/// How a frame goes on from a chain.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Go {
    /// The chain is at rest and the frame lies within the trust distance of where it is.
    Stays,
    /// The frame is the chain's next step.
    Steps((f64, f64)),
    /// No finger goes on like that.
    Breaks,
}

/// The speed of a finger that is standing still: the panel's reports of one wander by up to
/// 0.7 mm a frame. [`crate::tracker::ContactTracker`] holds a lift report for less time for a
/// finger this slow.
pub(crate) const REST_SPEED_MM_S: f64 = 50.0;

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
/// - A chain is believed on its own once it has [`Self::persist_frames`] frames, with no finger
///   down or after a lift report.
/// - A new press, with no finger to judge it against, is believed on its third frame, or on its
///   second when the panel reports its lift. One to four frames of a touch somewhere else come
///   just before a finger lands (probe 2026-11-24 17:20, 4.329 s: two frames 55 mm from where
///   the finger landed 70 ms later), and believed on two they were a click of their own.
/// - A chain of two frames that stands still on its third began at its second, not its first, if
///   it left the first faster than [`Self::land_speed_mm_s`]: the panel's frame from elsewhere,
///   a frame before the landing (2026-11-27, 00:21: 195.3 mm, then the finger at 183.5 mm). With
///   no finger down, or after its lift report: while a finger is down and none has come, a fast
///   step that stops is one of the panel's sweeps pausing.
///
/// While the finger is down and no lift report has come, a chain that moves from its first frames
/// is believed only where the finger could have got to, for as long as the panel keeps to other
/// places. The panel kept to a misread for seven frames in a row (title-bar 2026-11-25, 7.322 s:
/// three frames 51 mm from a resting finger, then four that moved on from there), and believing
/// them on the fifth moved a scroll 51 mm; and it reports sweeps along the finger's row, which
/// begin as fast as they go on (2026-11-26, 22:03:34: 76.2 mm, then 94.0 mm 16 ms later, 73 mm
/// from a finger held to right-click). A finger the panel hid comes back at rest and moves on
/// from there, and is taken where it could have got to, less exactly, once [`Self::persist_frames`]
/// frames have come without it (title-bar 2026-11-25, 29.859 s: three frames 9 mm to the right
/// of a slow drag, then five of the finger 15 mm to the left). After [`Self::lost_frames`] such
/// frames the finger is given up for lost, and a place is believed wherever it is: the
/// alternative is a touch ignored until the finger leaves.
///
/// Once a lift report has come (the tracker is then holding the contact, see [`crate::stream`]),
/// only belief on a chain's own counts: around a lift the panel's other places are its misreads,
/// in runs of up to four frames. The finger back from a flap is the exception. The panel reports
/// the tip off while the finger is down, and for a report or two after that says nothing; the
/// finger then comes back 13 to 22 mm on, within [`Self::flap_ns`] of the report, and stands
/// still. A still chain that begins there, where the tracker would still take the finger, is
/// believed on its third frame (2026-11-27, 16:25: seven flaps in one drag and the scrolls after
/// it stood the contact still for 70 ms each, and jumped 13 to 22 mm). The same is asked for [`Self::reach_ns`]
/// after a contact has ended of a press reported farther than [`Self::trust_mm`] from where the
/// last one was.
///
/// After a lift the panel goes on reporting touches of its own, in storms: 2026-11-26, in one
/// evening's use, after nearly every lift for 0.3 to 1.3 s, bursts of one to nine frames on the
/// finger's row (2026-11-26, 22:03: within 19 mm of where it lifted in 200 frames; 2026-11-27,
/// 00:21: 30 mm off that row and less, closing on where the finger landed 365 ms after the lift),
/// anywhere from side to side, most ended by a lift report, for as long as frames kept coming,
/// never more than 105 ms apart. In the evening's three recordings it never came back once the
/// panel had been quiet; in the afternoon's (origin-reports 2026-11-26) it started again after
/// 198 ms, 356 ms and 643 ms. They begin after a run of lift reports from the origin
/// (2026-11-26, 22:03: the report at 3.464 s, seven from the origin to 3.605 s, its frames from
/// 3.662 s). Believed as presses on the build of that evening, they were ten taps (one the second
/// click of a double-click on a window's title bar), two scrolls and a drag cut in two. So a
/// touch that begins amid the storm
/// ([`Self::amid_storm`]) is held until it proves itself a finger, and nothing else is believed
/// meanwhile, a press of two frames confirmed by its lift report included:
///
/// - It is believed with [`Self::storm_frames`] frames of its own, and sooner once it has gone
///   [`Self::storm_cross_mm`] across its row, further than along it: the panel's own places keep
///   to a row.
/// - A frame with the tip down that is not its own does not end it, but one that follows must
///   lie where the touch was heading. There is one finger, and around one that is down the panel
///   keeps sending places of its own.
/// - It is given up [`Self::reach_ns`] after its own lift report if none of its frames follows
///   before then, and after [`Self::storm_patience_ns`] without a frame of its own; with the
///   panel silent, that is [`Self::reach_ns`] after its last report. Around a finger that is down
///   the panel sends a frame of its own in place of the finger's every so often (2026-11-26,
///   22:03:03: eight among the thirty of a drag), and the touch must live through them.
///
/// The storm is over [`Self::storm_quiet_ns`] after the panel's last report, for good. What it
/// reports after that is judged as at any other time: one frame of its own 160 ms after a tap's
/// lift report, then the owner's next tap 50 ms later, is a tap (title-bar 2026-11-25, 12.310 s).
///
/// A finger cannot lift and land again in [`Self::storm_start_ns`]: a touch is held whatever else
/// the panel has said if it began within that of the lift report. One that proves itself there
/// and then, farther than a finger that stood still could have got from where it lifted, is the
/// landing the panel misread before it ([`Self::landing`]). And a touch that stands still within
/// [`Self::storm_mm`] of where the finger lifted, up to [`Self::storm_retap_ns`] after the lift
/// report, is the finger back, a tap of the owner's after the last: it is judged as at any other
/// time (title-bar 2026-11-25, 12.361 s and 12.450 s, the second and third of three taps on a
/// title bar, 1.4 and 5.5 mm from the first).
///
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
    /// Frames of one place before a chain is believed on its own, with no finger down or after a
    /// lift report (measured: the panel's runs of other places are up to four frames long, a
    /// finger seen again after a flap is back for five).
    pub persist_frames: usize,
    /// Frames with the finger down and none of them where it could have got to, before the finger
    /// is given up for lost and a place is believed wherever it is (measured: seven such frames in
    /// a run described on the type, while the finger was down; the tracker holds a lift for
    /// 250 ms, which is fifteen frames).
    pub lost_frames: usize,
    /// How long after its lift report at the most the panel goes on reporting touches of its own
    /// (measured, 2026-11-26: 1.3 s after one lift).
    pub storm_ns: u64,
    /// How long the panel has to send no report at all, tip down or up, for the storm to be over,
    /// for good (measured, 2026-11-26 and 2026-11-27: never more than 105 ms between two of its
    /// frames; after a lift it reports the tip off from its origin for up to 140 ms before they
    /// begin). The panel has been seen to start again after longer silences (origin-reports
    /// 2026-11-26: 198 ms, 356 ms and 643 ms); what begins then is judged as at any other time.
    pub storm_quiet_ns: u64,
    /// How long a touch held amid the storm may go without a frame of its own before it is given
    /// up (measured, 2026-11-26: the panel sent five frames of other places, 140 ms, in place of a
    /// finger's).
    pub storm_patience_ns: u64,
    /// How soon after a lift report no touch can be the same finger again, however still it
    /// lifted: one that begins there and then is held whatever else the panel has said (measured,
    /// 2026-11-24 to 2026-11-26: the panel's misread of a landing came up to 57 ms after such a
    /// report; a finger was back 106 ms after one).
    pub storm_start_ns: u64,
    /// How far from where the finger lifted a touch must begin to be too far for the finger back
    /// from a flap, for the storm's verdict on the touch that lifted, and how near one that
    /// stands still must begin to be the finger back once the contact has ended. It is the
    /// tracker's.
    pub storm_mm: f64,
    /// How soon after the lift report a touch that stands still near where the finger lifted is
    /// still taken for the finger back, the storm notwithstanding. The owner's came 56 to 386 ms
    /// after the lift report (measured, 2026-11-25 to 2026-11-27: a nudge, a drag, two holds to
    /// right-click, a tap on a title bar); the panel's own still run at the lift place 580 ms after
    /// it (origin-reports 2026-11-26, 9.426 s: five frames 1 mm from where a tap had lifted, then
    /// a frame 47 mm on).
    pub storm_retap_ns: u64,
    /// How many frames a touch must show of its own before it is believed amid the storm
    /// (measured: the longest burst was nine).
    pub storm_frames: usize,
    /// How far across its row a touch must have gone to be believed sooner: the tap slop.
    pub storm_cross_mm: f64,
    /// How fast a finger can be moving on the first step of a touch that stands still from its
    /// second frame (measured, 2026-11-24 to 2026-11-27: of 52 touches that stood still on their
    /// third frame after a step of 40 mm/s or more, the finger's ten had left their first frame at
    /// under 270 mm/s, 4.7 mm in one frame, and the panel's 42 at 470 mm/s and more).
    pub land_speed_mm_s: f64,
    /// How soon after a lift report the finger's own frames come back when the panel only flapped
    /// (it reported the tip off while the finger was down): a report or two is missing, and the
    /// finger is then 13 to 22 mm on and coming to rest (measured, 2026-11-26 and 2026-11-27: 17
    /// to 38 ms after the report). A still chain that begins there and then, where the tracker
    /// would still take the finger, is believed on its third frame like any still chain. The
    /// panel's own still places within that reach start later: origin-reports 2026-11-26, 11.640
    /// s has three frames 29 mm from a scroll's end, 177 ms after its lift report.
    pub flap_ns: u64,

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
    /// Where the finger was when its lift report came, and when: the storm that follows a lift is
    /// judged from here.
    storm: Option<Seen>,
    /// When the storm is over if no report comes first.
    storm_quiet_until_ns: u64,
    /// Whether the finger stood still where it lifted. Only then is a touch far from there in the
    /// moment after the landing the panel misread: one that was moving could have got further
    /// than the tracker's reach says.
    storm_rest: bool,
    /// The last frame with the tip down that was dropped: passed on by nothing, and in no chain.
    stray: Option<Seen>,
    /// Whether the storm was on when the chain's first frame came, before any frame of the
    /// chain's own could keep it going.
    chain_in_storm: bool,
    /// The first touch that began within `storm_start_ns` of the lift report, if one did: where
    /// its first frame was. Until it is believed or given up, the contact that lifted has no
    /// verdict ([`Self::awaiting`]).
    soon: Option<Seen>,
    /// When that touch was given up for another place, if it was: one finger cannot be in both,
    /// so what lifted was no misread of it.
    soon_over_ns: Option<u64>,
    /// Told by the stream with each report ([`Self::told_reach`]).
    told_reach_mm: Option<f64>,
    provisional: Option<Unproven>,
    landing: Option<Landing>,
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
            storm_ns: 1_500_000_000,
            storm_quiet_ns: 120_000_000,
            storm_patience_ns: 150_000_000,
            storm_start_ns: 100_000_000,
            storm_mm: crate::tracker::ContactTracker::DEFAULT_ADOPTION_BASE_MM,
            storm_retap_ns: 500_000_000,
            storm_frames: 10,
            storm_cross_mm: 8.0,
            land_speed_mm_s: 350.0,
            flap_ns: 50_000_000,
            finger: None,
            chain: SmallVec::new(),
            entry: None,
            link: None,
            staying: true,
            unconfirmed: 0,
            last_tip_down_ns: None,
            lifted: false,
            after: None,
            storm: None,
            storm_quiet_until_ns: 0,
            storm_rest: false,
            stray: None,
            chain_in_storm: false,
            soon: None,
            soon_over_ns: None,
            told_reach_mm: None,
            provisional: None,
            landing: None,
            panel_mm: (0.0, 0.0),
        }
    }

    /// Forgets everything: the panel has been released, or its display has changed under it.
    pub fn reset(&mut self) {
        self.forget();
        self.last_tip_down_ns = None;
        self.after = None;
        self.storm = None;
        self.storm_rest = false;
        self.stray = None;
        self.chain_in_storm = false;
        self.soon = None;
        self.soon_over_ns = None;
        self.provisional = None;
        self.landing = None;
    }

    /// Told by the stream with each report: how far from where the finger lifted the tracker would
    /// still take a touch for the contact whose lift it holds
    /// ([`crate::tracker::ContactTracker::adoption_reach_mm`]), or `None` when it holds none.
    /// While it holds one, a touch that begins within that distance is left to the tracker, as it
    /// always was: it is the finger back from a flap or not, by the tracker's rule.
    pub fn told_reach(&mut self, reach_mm: Option<f64>) {
        self.told_reach_mm = reach_mm;
    }

    /// How the touch that the last call passed on began, when it was a chain the filter had held:
    /// where and when its first frame came, and whether it came too soon after the lift before it
    /// for one finger to have made both. `None` when the last call passed a single frame on, or
    /// nothing.
    pub fn landing(&self) -> Option<Landing> {
        self.landing
    }

    /// What the panel reported in the [`Self::storm_start_ns`] after the last lift report, as it
    /// still stands at `at_ns`: the first touch that began then is judged until it is believed or
    /// given up ([`Self::held_until_ns`]). A second place in that time is the panel's own: then
    /// what lifted is no misread of a landing.
    pub fn awaiting(&self, at_ns: u64) -> Awaiting {
        let Some((first, last, waiting)) = self.judged() else {
            return Awaiting::Nothing;
        };
        let Some(lift) = self.storm else {
            return Awaiting::Nothing;
        };
        let given_up = match self.provisional.as_ref() {
            Some(provisional) if waiting => at_ns >= self.verdict_at(provisional),
            _ => at_ns.saturating_sub(last.at_ns) >= self.reach_ns,
        };
        if given_up {
            return Awaiting::Nothing;
        }
        if distance(first, lift) > self.storm_mm { Awaiting::WouldMisread } else { Awaiting::Judging }
    }

    /// When the touch that began in the [`Self::storm_start_ns`] after the last lift report, and
    /// is still being judged, is given up if nothing more comes; or when it was given up, if it
    /// was. `None` when none began.
    pub fn held_until_ns(&self) -> Option<u64> {
        match self.judged() {
            Some((_, last, waiting)) => match self.provisional.as_ref() {
                Some(provisional) if waiting => Some(self.verdict_at(provisional)),
                _ => Some(last.at_ns.saturating_add(self.reach_ns)),
            },
            None => self.soon_over_ns,
        }
    }

    /// The first touch that began in the moment after the last lift report, while it is still
    /// being judged: its first frame, its last, and whether it is a touch waiting to be proven,
    /// which comes before the chain.
    fn judged(&self) -> Option<(Seen, Seen, bool)> {
        let soon = self.soon?;
        if self.soon_over_ns.is_some() || self.storm.is_none() {
            return None;
        }
        if let Some(provisional) = &self.provisional {
            return (provisional.frames[0] == soon).then(|| (soon, provisional.frames[provisional.frames.len() - 1], true));
        }
        let first = *self.chain.first()?;
        if soon != first {
            return None;
        }
        Some((soon, *self.chain.last()?, false))
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

    /// The frames to feed the tracker instead of `frame`: none while a frame waits for support or
    /// a touch waits to be proven, the frame itself when it is believed at once, and a whole chain
    /// of them when one is believed.
    pub fn filter(
        &mut self,
        frame: TouchFrame,
        at_ns: u64,
        panel_width_mm: f64,
        panel_height_mm: f64,
    ) -> SmallVec<[TouchFrame; 8]> {
        self.landing = None;
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
        let seen = Seen {
            x: contact.x * panel_width_mm,
            y: contact.y * panel_height_mm,
            at_ns,
            id: contact.id,
            scan_time: frame.scan_time,
        };
        if !contact.touching {
            // A press of two frames is confirmed by the report of its lift, while the pair is
            // still hanging. Nothing is confirmed amid the storm: five such pairs in one evening
            // (2026-11-26) were the panel's every one.
            let mut frames = smallvec![];
            let pair = self.chain.len() == 2 && at_ns.saturating_sub(self.chain[1].at_ns) <= self.reach_ns;
            if self.finger.is_none()
                && self.provisional.is_none()
                && pair
                && !self.elsewhere(self.chain[0])
                && !self.chain_in_storm
            {
                frames = self.believe();
            }
            // A lift report from where a touch waiting to be proven was last seen is that touch's
            // own: it is given up if no frame of it follows, and one that follows must lie where
            // the touch was heading.
            if let Some(provisional) = self.provisional.as_mut() {
                provisional.last_frame_ns = at_ns;
                if distance(seen, provisional.frames[provisional.frames.len() - 1]) <= self.trust_mm {
                    provisional.lifted_ns = Some(at_ns);
                    provisional.broken = true;
                }
            }
            // Any report keeps the storm alive. After a lift the panel goes on reporting the tip
            // off, from its origin, before the frames of its own begin (2026-11-26, 22:03: the lift
            // report at 3.464 s, seven from the origin to 3.605 s, its frames from 3.662 s).
            self.storm_goes_on(at_ns);
            // The lift report of a finger the filter believes down: where the storm that follows
            // it is judged from. A later one changes nothing.
            if self.finger.is_some() && self.storm.is_none() {
                self.storm = self.finger.map(|finger| Seen { at_ns, ..finger.at });
                self.storm_rest = self.finger.is_some_and(|finger| speed((finger.vx, finger.vy)) < REST_SPEED_MM_S);
                self.storm_quiet_until_ns = at_ns.saturating_add(self.storm_quiet_ns);
                self.soon = None;
                self.soon_over_ns = None;
            }
            // A lift report, or a flap: whether the contact ends is the tracker's decision, and
            // a finger that comes back is judged against where it was.
            self.lifted |= self.finger.is_some();
            frames.push(frame);
            return frames;
        }
        self.panel_mm = (panel_width_mm, panel_height_mm);
        // A chain left hanging for this long belonged to a touch that is over (a frame or three
        // the panel reported after a lift): nothing is coming to support it.
        if self.last_tip_down_ns.is_some_and(|at| seen.at_ns.saturating_sub(at) > self.reach_ns) {
            self.forget_chain();
        }
        self.last_tip_down_ns = Some(seen.at_ns);
        // A touch waiting to be proven that the panel has left alone is given up (`over`): it was
        // a burst of the panel's, or a finger that lifted.
        if self.provisional.as_ref().is_some_and(|provisional| self.over(provisional, seen.at_ns)) {
            self.drop_provisional(seen.at_ns);
        }
        if let Some(mut provisional) = self.provisional.take() {
            provisional.last_frame_ns = seen.at_ns;
            if self.misled(&provisional.frames, provisional.link) && distance(seen, provisional.frames[1]) <= self.trust_mm {
                // As in `extend`: the finger stood still from the touch's second frame.
                let second = provisional.frames[1];
                let first = provisional.frames.remove(0);
                self.rebegin(first, second);
                provisional.link = None;
                provisional.staying = true;
            }
            let last = provisional.frames[provisional.frames.len() - 1];
            let go = self.goes_on(&provisional.frames, provisional.link, provisional.staying, None, seen);
            // A frame of its own goes on from the touch as a chain does, or, after a frame that
            // was not its own, lies where the touch was heading, as a frame of the finger's does.
            // A run of the panel's own that only accelerates its way back into the touch is not
            // let in (measured, 2026-11-26: a sweep that turned from 1075 mm/s one way to 1900 the
            // other in 67 ms).
            let heading = provisional.link.filter(|_| !provisional.staying).unwrap_or((0.0, 0.0));
            let held = seconds(seen.at_ns.saturating_sub(last.at_ns).min(self.reach_ns));
            let ahead = Seen { x: last.x + heading.0 * held, y: last.y + heading.1 * held, ..seen };
            let own = if provisional.broken {
                distance(seen, ahead) <= self.trust_mm
            } else {
                !matches!(go, Go::Breaks)
            };
            if own {
                match go {
                    Go::Stays => {}
                    Go::Steps(step) => {
                        provisional.link = Some(step);
                        provisional.staying = false;
                    }
                    Go::Breaks => {
                        provisional.link = Some(step(last, seen));
                        provisional.staying = false;
                    }
                }
                provisional.frames.push(seen);
                provisional.lifted_ns = None;
                provisional.broken = false;
                // The storm over, the panel is taken at its word again.
                if self.proven(&provisional.frames) || !self.storm_alive(seen.at_ns) {
                    return self.take_up(&provisional.frames, provisional.staying);
                }
                self.storm_goes_on(seen.at_ns);
                self.provisional = Some(provisional);
                return smallvec![];
            }
            // Somewhere else: the panel's other place or the next touch, a chain of its own below.
            provisional.broken = true;
            if self.soon == Some(provisional.frames[0]) && self.within_storm_start(seen.at_ns) {
                self.soon_over_ns = Some(seen.at_ns);
            }
            self.provisional = Some(provisional);
        }
        if let Some(finger) = self.finger {
            // Where the finger was heading, over no more of the gap than its reach is known for:
            // further on, a place that happens to lie on that line is no evidence.
            let held = seconds(seen.at_ns.saturating_sub(finger.at.at_ns).min(self.reach_ns));
            let ahead = Seen { x: finger.at.x + finger.vx * held, y: finger.at.y + finger.vy * held, ..seen };
            if distance(seen, ahead) <= self.trust_mm {
                // The finger was seen again: there is no storm and no other touch.
                self.forget_chain();
                self.unconfirmed = 0;
                self.lifted = false;
                self.finger = Some(finger.moved_to(seen));
                self.provisional = None;
                self.storm = None;
                self.stray = None;
                return smallvec![seen.frame(self.panel_mm)];
            }
        }
        self.unconfirmed += 1;
        self.extend(seen);
        let frames = self.settle();
        // A frame the filter held: the storm goes on while the panel keeps sending them. Whether
        // the chain began amid it was settled when the chain opened, before its own frames could
        // keep the storm alive.
        if frames.is_empty() {
            self.storm_goes_on(seen.at_ns);
        }
        frames
    }

    /// The storm goes on while the panel keeps talking: a report of any kind, or a frame the
    /// filter did not believe at once. One that comes after the storm was over starts no storm
    /// (title-bar 2026-11-25, 12.310 s: one frame of the panel's 160 ms after a tap's lift report,
    /// then the owner's next tap 50 ms later, 1.4 mm from the first).
    fn storm_goes_on(&mut self, at_ns: u64) {
        if self.storm_alive(at_ns) {
            self.storm_quiet_until_ns = at_ns.saturating_add(self.storm_quiet_ns);
        }
    }

    fn forget_chain(&mut self) {
        if let Some(last) = self.chain.last().copied() {
            self.stray = Some(last);
        }
        self.chain.clear();
        self.entry = None;
        self.link = None;
        self.staying = true;
        self.unconfirmed = 0;
    }

    /// Puts `seen` at the end of the chain when it goes on from there the way a finger can,
    /// and starts a new chain with it otherwise.
    fn extend(&mut self, seen: Seen) {
        if self.chain.is_empty() {
            self.open(seen);
            return;
        }
        if self.misled(&self.chain, self.link) && distance(seen, self.chain[1]) <= self.trust_mm {
            // The finger stood still from the chain's second frame: its first was the panel's.
            let second = self.chain[1];
            let first = self.chain.remove(0);
            self.rebegin(first, second);
            self.link = None;
            self.staying = true;
            self.entry = self.finger.map(|finger| step(finger.at, second));
        }
        match self.goes_on(&self.chain, self.link, self.staying, self.entry, seen) {
            Go::Stays => self.chain.push(seen),
            Go::Steps(step) => {
                self.link = Some(step);
                self.staying = false;
                self.chain.push(seen);
            }
            Go::Breaks => self.open(seen),
        }
    }

    /// How `seen` goes on from a chain. A chain that has stood still for two frames is at rest,
    /// and so is one that has come to a stop on the way: its last step was no more than a resting
    /// finger wanders. A moving one keeps its last step; a single frame has only the step that led
    /// to it, `entry`, when there was a finger to lead from it.
    fn goes_on(&self, chain: &[Seen], link: Option<(f64, f64)>, staying: bool, entry: Option<(f64, f64)>, seen: Seen) -> Go {
        if staying && distance(seen, chain[0]) <= self.trust_mm {
            return Go::Stays;
        }
        let last = chain[chain.len() - 1];
        let rested = link.is_some_and(|link| speed(link) <= REST_SPEED_MM_S);
        let at_rest = (staying && chain.len() >= 2) || rested;
        let was = link.or_else(|| at_rest.then_some((0.0, 0.0))).or(entry);
        let step = step(last, seen);
        // A finger leaves a place no faster than it lands and starts: the first step of a press
        // and the first step out of a resting chain are held to the same speed. (Measured,
        // 2026-11-24: three frames of a misread at rest, then one 17 mm on from them at 900 mm/s.
        // Taken with them, the chain ran on into the finger's own frames and was believed. And
        // 2026-11-25: a run that stopped for two frames, then the finger 29 mm back at 856 mm/s.)
        let fastest = if at_rest || was.is_none() { self.start_speed_mm_s } else { self.max_speed_mm_s };
        let turned = was.is_none_or(|was| {
            speed((step.0 - was.0, step.1 - was.1)) <= self.acceleration_mm_s2 * seconds(seen.at_ns.saturating_sub(last.at_ns))
        });
        if !(speed(step) <= fastest && turned) {
            return Go::Breaks;
        }
        Go::Steps(step)
    }

    /// Whether a finger is believed down and no lift report has come for it.
    fn finger_down(&self) -> bool {
        self.finger.is_some() && !self.lifted
    }

    /// Whether a chain of two frames may have begun one frame too soon: it left its first frame
    /// moving faster than a finger lands and stops ([`Self::land_speed_mm_s`]). A finger that
    /// then stands still on the chain's second frame did not come from its first: that frame was
    /// the panel's, and the chain begins at the second. Only where no finger is down without a
    /// lift report ([`Self::finger_down`]): there a fast step that stops is one of the panel's
    /// sweeps pausing, and a chain begun at its second frame would be one that began at rest
    /// (2026-11-26, 22:03:34).
    fn misled(&self, chain: &[Seen], link: Option<(f64, f64)>) -> bool {
        chain.len() == 2 && link.is_some_and(|link| speed(link) > self.land_speed_mm_s) && !self.finger_down()
    }

    /// A chain that began with a frame the finger did not come from ([`Self::misled`]) begins at
    /// its second, `second`: the first, `first`, is a frame the panel reported elsewhere, and
    /// what was settled for the chain when `first` came is settled again for `second`.
    fn rebegin(&mut self, first: Seen, second: Seen) {
        self.stray = Some(first);
        if self.soon == Some(first) || (self.soon.is_none() && self.within_storm_start(second.at_ns)) {
            self.soon = Some(second);
        }
        // In the storm if it was on when the second frame came. Until then the first frame, held,
        // was what the panel had said last: that is what kept the storm going, if it did.
        self.chain_in_storm = self.storm.is_some_and(|lift| {
            second.at_ns.saturating_sub(lift.at_ns) < self.storm_ns && second.at_ns.saturating_sub(first.at_ns) < self.storm_quiet_ns
        });
    }

    /// Starts a new chain with `seen` as its first frame. What the chain before it held is dropped.
    fn open(&mut self, seen: Seen) {
        if let Some(last) = self.chain.last().copied() {
            self.stray = Some(last);
        }
        // Another place where the first touch after the lift report was, within the time no finger
        // can be in two: that touch is over, and no landing of one finger.
        if self.chain.first().is_some_and(|first| Some(*first) == self.soon) && self.within_storm_start(seen.at_ns) {
            self.soon_over_ns = Some(seen.at_ns);
        }
        self.chain.clear();
        self.chain.push(seen);
        self.entry = self.finger.map(|finger| step(finger.at, seen));
        self.link = None;
        self.staying = true;
        self.chain_in_storm = self.storm_alive(seen.at_ns);
        if self.soon.is_none() && self.within_storm_start(seen.at_ns) {
            self.soon = Some(seen);
        }
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
        // A chain that keeps to its first frame is at rest, unless it is a finger leaving fast:
        // its first frame is the report right after the finger's last sighting, just outside the
        // trust distance, and its second moved on from it (2026-11-27, 16:25:08: after 105 ms
        // without the finger the filter took it for slow; its next frames, 8 mm and 5 mm on, were
        // held for a third, and the drag stood still for 36 ms). That is a chain in motion.
        let leaving_fast = match (self.finger, self.chain.first()) {
            (Some(finger), Some(&first)) => {
                self.chain.len() == 2
                    && first.at_ns.saturating_sub(finger.at.at_ns) <= 25_000_000
                    && distance(first, finger.at) <= 2.0 * self.trust_mm
                    && !self.last_step_rests()
            }
            _ => false,
        };
        let standing = self.staying && self.finger.is_some() && !leaving_fast;
        let needed = if standing { 3 } else { 2 };
        let unsupported = (self.lifted && !self.back_from_flap(first)) || self.elsewhere(first);
        // A new press waits for its third frame, or for the report of its lift (`filter`).
        if self.finger.is_none() && !unsupported && frames < 3 {
            return smallvec![];
        }
        let supported = !unsupported && reachable && at_rest && frames >= needed;
        // With the finger down and no lift report come, a chain that moves from its first frames is
        // believed only where the finger could have got to (`supported`): around a finger that is
        // down the panel reports places of its own, in sweeps along the finger's row (2026-11-26,
        // 22:03:34: a run 73 mm from a resting finger 89 ms after its last frame, on two frames
        // that moved on from each other, turned a hold to right-click into a scroll). A finger the
        // panel hid does come back that far, at rest and then moving on: so a chain that began at
        // rest is taken where the finger could have got to, less exactly, once
        // [`Self::persist_frames`] frames have come without it (title-bar 2026-11-25, 29.859 s:
        // three frames 9 mm to the right of a slow drag, then five of the finger 15 mm to the
        // left). Once [`Self::lost_frames`] such frames have come, the finger is given up for lost,
        // and a place is believed wherever it is: the alternative is a touch ignored until the
        // finger leaves.
        let down = self.finger_down();
        let lost = down && self.unconfirmed >= self.lost_frames;
        let persisted = (!down || lost) && frames >= self.persist_frames;
        let arrived = down && !lost && self.unconfirmed >= self.persist_frames && self.began_still() && self.attainable(first);
        if !(supported || persisted || arrived) {
            return smallvec![];
        }
        // A touch that begins amid the storm waits to be proven a finger, and nothing else is
        // believed meanwhile: the panel has one finger. What the chain holds waits with it.
        if self.amid_storm(first) {
            return if self.provisional.is_none() { self.hold() } else { smallvec![] };
        }
        self.believe()
    }

    /// Whether the chain's last step was no faster than a resting finger's wander
    /// ([`REST_SPEED_MM_S`]). A chain of one frame has no step: it is at rest.
    fn last_step_rests(&self) -> bool {
        self.chain.len() < 2 || speed(step(self.chain[self.chain.len() - 2], self.chain[self.chain.len() - 1])) <= REST_SPEED_MM_S
    }

    /// Whether a chain that stands still, whose first frame is `first`, is the finger back from one
    /// of the panel's flaps. A lift report has come ([`SingleTouchFilter::lifted`]), the chain's
    /// first frame came within [`Self::flap_ns`] of it, and it began where the tracker would still
    /// take the finger for the contact whose lift it holds (the reach it told of,
    /// [`Self::told_reach`]). So judged, it is believed on its third frame as any still chain is;
    /// a chain after a lift report otherwise waits for five.
    fn back_from_flap(&self, first: Seen) -> bool {
        self.staying
            && self.storm.as_ref().is_some_and(|lift| {
                first.at_ns.saturating_sub(lift.at_ns) <= self.flap_ns && self.told_reach_mm.is_some_and(|reach| distance(first, *lift) <= reach)
            })
    }

    /// Whether a touch whose first frame is `first` begins amid the storm that follows a lift:
    /// the storm was on when that frame came ([`Self::storm_alive`], before the touch's own frames
    /// could keep it going), it is neither a touch the tracker would still take for the finger
    /// that lifted (while the tracker holds the contact, one within the reach it was told of is
    /// the tracker's to judge, as it always was) nor one that stands still near where the finger
    /// lifted ([`Self::still_near`]), and it began within [`Self::storm_start_ns`] of the lift
    /// report, or the panel has reported another place since ([`Self::another_place_since`]).
    /// Where it lies makes no difference: the panel's places are on the finger's row, wherever the
    /// finger has gone.
    fn amid_storm(&self, first: Seen) -> bool {
        let Some(lift) = self.storm else {
            return false;
        };
        self.chain_in_storm
            && distance(first, lift) > self.lift_reach_mm().max(self.still_near(first))
            && (first.at_ns.saturating_sub(lift.at_ns) <= self.storm_start_ns || self.another_place_since(first))
    }

    /// How far from where the finger lifted a touch that stands still must begin to be of the
    /// storm once the tracker has let the contact go: [`Self::storm_mm`], and only for
    /// [`Self::storm_retap_ns`] after the lift report. Nearer and sooner, it is the finger back, a
    /// tap of the owner's after the last, and is judged as at any other time (title-bar 2026-11-25,
    /// 12.361 s: 1.4 mm from the first tap's lift report 212 ms after it, 37 ms after one of the
    /// panel's frames). A touch that moves from its first frame gets no such credit: the panel's
    /// sweeps begin where the finger was (2026-11-26, 22:03:34: eight of them from the left edge,
    /// 44 to 108 mm long, while a finger was held there to right-click).
    fn still_near(&self, first: Seen) -> f64 {
        let Some(lift) = self.storm else { return 0.0 };
        if self.lifted || !self.staying || first.at_ns.saturating_sub(lift.at_ns) > self.storm_retap_ns {
            return 0.0;
        }
        if distance(first, lift) <= self.storm_mm { self.storm_mm } else { 0.0 }
    }

    /// Whether the panel has reported a place since the lift report that is nowhere near `first`:
    /// a frame that was dropped, or one held for a touch that waits to be proven. One finger
    /// cannot be in both.
    fn another_place_since(&self, first: Seen) -> bool {
        let far = |place: Seen| place.at_ns.saturating_sub(self.storm.map_or(u64::MAX, |lift| lift.at_ns)) < u64::MAX && distance(place, first) > self.trust_mm;
        self.stray.is_some_and(far)
            || self.provisional.as_ref().is_some_and(|provisional| far(provisional.frames[provisional.frames.len() - 1]))
    }

    /// How far from where the finger lifted a touch must begin to be no return of the finger
    /// while the tracker holds the contact: the reach the stream told of, or [`Self::storm_mm`]
    /// when it told none. Nothing once the tracker has let the contact go: then every touch is a
    /// new one, wherever it begins.
    fn lift_reach_mm(&self) -> f64 {
        if self.lifted { self.told_reach_mm.unwrap_or(self.storm_mm) } else { 0.0 }
    }

    /// Whether the storm that follows a lift is still on at `at_ns`: no longer than
    /// [`Self::storm_ns`] after the lift report, and only until the panel has sent nothing for
    /// [`Self::storm_quiet_ns`]. After that it is over for good: a frame of the panel's that comes
    /// later is one before a landing, judged as any press is.
    fn storm_alive(&self, at_ns: u64) -> bool {
        self.storm.is_some_and(|lift| at_ns.saturating_sub(lift.at_ns) < self.storm_ns && at_ns < self.storm_quiet_until_ns)
    }

    /// Whether `at_ns` is within [`Self::storm_start_ns`] of the lift report.
    fn within_storm_start(&self, at_ns: u64) -> bool {
        self.storm.is_some_and(|lift| at_ns.saturating_sub(lift.at_ns) <= self.storm_start_ns)
    }

    /// Whether a touch whose first frame is `first` came in the moment after the lift report
    /// before it, farther from where a finger that stood still could have got by then
    /// ([`Self::lift_reach_mm`]): one finger could not have made both, so what lifted was the
    /// panel's misread of this landing. Not so for a touch that began before the lift report
    /// (probe-multifinger 2026-11-24, 10.805 s: one finger's frames 17 ms before the report of
    /// another's lift, in the single-contact stream), nor where the finger was moving when it
    /// lifted (the same recording, 15.619 s: a stroke at 102 mm/s, the report, and a touch
    /// 35 mm on 17 ms later): what a moving finger left was no misread.
    fn misread_before(&self, first: Seen) -> bool {
        self.storm_rest
            && self.within_storm_start(first.at_ns)
            && self.storm.is_some_and(|lift| first.at_ns > lift.at_ns && distance(first, lift) > self.lift_reach_mm())
    }

    /// Whether a touch waiting to be proven is given up by `at_ns`, for want of frames.
    fn over(&self, provisional: &Unproven, at_ns: u64) -> bool {
        at_ns >= self.given_up_at(provisional)
    }

    /// When the touch that lifted has its verdict on a touch waiting to be proven: at its lift
    /// report, and otherwise when it is given up. A landing that was misread before it shows
    /// itself from its first frames ([`Self::storm_cross_mm`]); one whose own lift report comes
    /// first was the panel's, and the touch before it no misread.
    fn verdict_at(&self, provisional: &Unproven) -> u64 {
        provisional.lifted_ns.unwrap_or_else(|| self.given_up_at(provisional))
    }

    /// When a touch waiting to be proven is given up if nothing more comes: when the panel has
    /// said nothing for [`Self::reach_ns`], when nothing of this touch has come for
    /// [`Self::storm_patience_ns`], and, its own lift report having come, [`Self::reach_ns`]
    /// after that, whatever else the panel says.
    fn given_up_at(&self, provisional: &Unproven) -> u64 {
        let last = provisional.frames[provisional.frames.len() - 1].at_ns;
        let since_lift = provisional.lifted_ns.map_or(u64::MAX, |lifted| lifted.max(last).saturating_add(self.reach_ns));
        provisional.last_frame_ns.saturating_add(self.reach_ns).min(last.saturating_add(self.storm_patience_ns)).min(since_lift)
    }

    /// Whether a touch that began amid the storm has shown itself a finger: [`Self::storm_frames`]
    /// frames of its own, or, on at least three, a way across the row where the finger lifted.
    fn proven(&self, frames: &[Seen]) -> bool {
        frames.len() >= self.storm_frames
            || (frames.len() >= 3
                && (frames[frames.len() - 1].y - frames[0].y).abs() >= self.storm_cross_mm
                && (frames[frames.len() - 1].y - frames[0].y).abs() >= (frames[frames.len() - 1].x - frames[0].x).abs())
    }

    /// Takes the chain for a touch that began amid the storm and waits for it to be proven.
    fn hold(&mut self) -> SmallVec<[TouchFrame; 8]> {
        let unproven = Unproven {
            frames: self.chain.iter().copied().collect(),
            link: self.link,
            staying: self.staying,
            broken: false,
            last_frame_ns: self.chain[self.chain.len() - 1].at_ns,
            lifted_ns: None,
        };
        self.chain.clear();
        self.entry = None;
        self.link = None;
        self.staying = true;
        self.unconfirmed = 0;
        if self.proven(&unproven.frames) {
            return self.take_up(&unproven.frames, unproven.staying);
        }
        self.provisional = Some(unproven);
        smallvec![]
    }

    /// The touch that waited is the panel's after all: its frames are dropped.
    fn drop_provisional(&mut self, at_ns: u64) {
        if let Some(provisional) = self.provisional.take()
            && let Some(last) = provisional.frames.last().copied()
        {
            self.stray = Some(last);
            if self.soon == Some(provisional.frames[0]) {
                self.soon_over_ns = Some(at_ns);
            }
        }
    }

    /// Whether the chain began at rest: its first three frames within [`Self::trust_mm`] of its
    /// first. A finger that was hidden arrives somewhere and stops. The panel's sweeps move from
    /// their first or second frame (2026-11-26, 22:03:34: 76.2 mm, then 94.0 mm 16 ms later).
    fn began_still(&self) -> bool {
        self.chain.len() >= 3 && self.chain[..3].iter().all(|frame| distance(*frame, self.chain[0]) <= self.trust_mm)
    }

    /// Whether a finger at its last sighting could have got to `place` by the time of it: within a
    /// trust distance, or a step no longer than its speed and what acceleration adds over the
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
        self.take_up(&chain, self.staying)
    }

    /// Takes `frames`, one after another as a finger went, for the finger: every frame is passed
    /// on, and the repair starts over from the last.
    fn take_up(&mut self, frames: &[Seen], staying: bool) -> SmallVec<[TouchFrame; 8]> {
        let (Some(&first), Some(&last)) = (frames.first(), frames.last()) else {
            return smallvec![];
        };
        let (vx, vy) = if staying || frames.len() < 2 { (0.0, 0.0) } else { step(frames[frames.len() - 2], last) };
        self.landing = Some(Landing { x: first.x, y: first.y, at_ns: first.at_ns, misread_before: self.misread_before(first) });
        self.finger = Some(Finger { at: last, vx, vy });
        self.chain.clear();
        self.entry = None;
        self.link = None;
        self.staying = true;
        self.unconfirmed = 0;
        self.lifted = false;
        self.after = None;
        self.storm = None;
        self.stray = None;
        self.provisional = None;
        frames.iter().map(|frame| frame.frame(self.panel_mm)).collect()
    }

    /// What the filter holds, in words, for a debug build's instruments.
    #[cfg(feature = "debug")]
    pub fn say(&self) -> String {
        let place = |seen: Seen| format!("({:.1},{:.1})@{:.3}", seen.x, seen.y, seen.at_ns as f64 / 1e9);
        let list = |frames: &[Seen]| frames.iter().map(|&frame| format!("({:.1},{:.1})", frame.x, frame.y)).collect::<Vec<_>>().join(" ");
        let mut parts: Vec<String> = Vec::new();
        match self.finger {
            Some(finger) => parts.push(format!("finger {} {} mm/s", place(finger.at), speed((finger.vx, finger.vy)) as i32)),
            None => parts.push("no finger".to_string()),
        }
        if let Some(lift) = self.storm {
            parts.push(format!("storm {} quiet after {:.3}", place(lift), self.storm_quiet_until_ns as f64 / 1e9));
        }
        if !self.chain.is_empty() {
            parts.push(format!("chain {} {} [{}]", self.chain.len(), if self.staying { "still" } else { "moving" }, list(&self.chain)));
        }
        if let Some(provisional) = &self.provisional {
            parts.push(format!(
                "unproven {} {}{}{} [{}]",
                provisional.frames.len(),
                if provisional.staying { "still" } else { "moving" },
                if provisional.broken { ", broken" } else { "" },
                if provisional.lifted_ns.is_some() { ", lifted" } else { "" },
                list(&provisional.frames)
            ));
        }
        if let Some(stray) = self.stray {
            parts.push(format!("stray {}", place(stray)));
        }
        if self.chain_in_storm {
            parts.push("chain in the storm".to_string());
        }
        if let Some(soon) = self.soon {
            parts.push(format!("soon {}{}", place(soon), if self.soon_over_ns.is_some() { ", over" } else { "" }));
        }
        if self.lifted {
            parts.push("lifted".to_string());
        }
        if let Some(after) = self.after {
            parts.push(format!("after {}", place(after)));
        }
        if let Some(reach) = self.told_reach_mm {
            parts.push(format!("tracker's reach {reach:.1} mm"));
        }
        parts.join(" | ")
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
    // on from there. The panel reports places of its own around a lift, so after a lift report a
    // frame off the finger's course counts only once it has persisted: five frames, and all five
    // are passed on. In the moment of the lift report itself, within 100 ms of it, a touch that
    // far off is held to ten frames of its own: that soon after a lift the panel's places come
    // the same way (2026-11-26, 22:03:01, 2.044 s).
    #[test]
    fn a_finger_off_its_course_after_a_lift_report_is_believed_once_it_has_persisted() {
        // 136 ms after the lift report, the panel having said nothing in between but the tip off
        // from its origin.
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        let _ = filter.filter(mouse(0.0, 0.0, false), t + 119 * MS, WIDTH, HEIGHT);
        let course = [(67.0, 70.0), (68.1, 70.0), (72.8, 70.0), (78.0, 70.0)];
        for (index, &(x, y)) in course.iter().enumerate() {
            let frames = feed(&mut filter, x, y, t + 17 * MS * (index as u64 + 8));
            assert!(frames.is_empty(), "{x}: {frames:?}");
        }
        assert_eq!(
            feed(&mut filter, 84.0, 70.0, t + 17 * MS * 12),
            vec![(67.0, 70.0), (68.1, 70.0), (72.8, 70.0), (78.0, 70.0), (84.0, 70.0)]
        );
        assert_eq!(feed(&mut filter, 89.0, 70.0, t + 17 * MS * 13), vec![(89.0, 70.0)]);

        // 51 ms after the lift report: the first nine frames are held, and all ten passed on.
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 33 * MS, WIDTH, HEIGHT);
        let course = [
            (67.0, 70.0),
            (68.1, 70.0),
            (72.8, 70.0),
            (78.0, 70.0),
            (84.0, 70.0),
            (90.0, 70.0),
            (96.0, 70.0),
            (102.0, 70.0),
            (108.0, 70.0),
        ];
        for (index, &(x, y)) in course.iter().enumerate() {
            let frames = feed(&mut filter, x, y, t + 17 * MS * (index as u64 + 4));
            assert!(frames.is_empty(), "{x}: {frames:?}");
        }
        assert_eq!(
            feed(&mut filter, 112.0, 70.0, t + 17 * MS * 13),
            vec![
                (67.0, 70.0),
                (68.1, 70.0),
                (72.8, 70.0),
                (78.0, 70.0),
                (84.0, 70.0),
                (90.0, 70.0),
                (96.0, 70.0),
                (102.0, 70.0),
                (108.0, 70.0),
                (112.0, 70.0)
            ]
        );
        assert_eq!(feed(&mut filter, 113.0, 70.0, t + 17 * MS * 14), vec![(113.0, 70.0)]);
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

    // Later than that moment a place 70 mm from the lift is the finger if it stands still for five
    // frames: where the finger went from where it was is no longer known. 17 ms after the lift
    // report, it is no finger however still it stands: it waits like any touch amid the storm for
    // ten frames of its own, and is passed on whole then.
    #[test]
    fn a_finger_that_stands_elsewhere_in_the_moment_of_a_lift_report_waits_to_prove_itself() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        for index in 0..9 {
            let frames = feed(&mut filter, 170.0, 70.0, t + 17 * MS * (index + 2));
            assert!(frames.is_empty(), "{} frames in: {frames:?}", index + 1);
        }
        let frames = feed(&mut filter, 170.0, 70.0, t + 17 * MS * 11);
        assert_eq!(frames.len(), 10, "{frames:?}");
        assert!(frames.iter().all(|frame| *frame == (170.0, 70.0)));
        assert_eq!(feed(&mut filter, 171.0, 70.0, t + 17 * MS * 12), vec![(171.0, 70.0)]);
    }

    #[test]
    fn a_finger_seen_again_only_after_wild_frames_is_believed_where_it_could_have_got() {
        // Six frames, each a sharp turn from the last and none near where the finger was, then
        // the finger 60 mm on from where it was last seen, standing still. Nothing supports the
        // wild places, and where the finger could have got to is believed on three still frames,
        // as anywhere.
        let (mut filter, t) = settled(100.0, 70.0);
        let zigzag = [(60.0, 70.0), (80.0, 60.0), (100.0, 50.0), (120.0, 60.0), (140.0, 70.0), (160.0, 60.0)];
        for (index, &(x, y)) in zigzag.iter().enumerate() {
            assert!(feed(&mut filter, x, y, t + 17 * MS * (index as u64 + 1)).is_empty());
        }
        assert!(feed(&mut filter, 164.0, 60.0, t + 17 * MS * 7).is_empty(), "two still frames: one more");
        assert_eq!(
            feed(&mut filter, 164.0, 60.0, t + 17 * MS * 8),
            vec![(160.0, 60.0), (164.0, 60.0), (164.0, 60.0)]
        );
    }

    // 2026-11-26, 22:03:01: after nearly every lift the panel went on reporting touches of its
    // own for up to 1.3 s, in bursts on the finger's row, most closed by a lift report of their
    // own. Believed as presses on the build of the recording, they were ten taps, two scrolls
    // and a drag cut in two. None of these is a touch.
    #[test]
    fn a_storm_after_a_lift_is_no_press() {
        let (mut filter, t) = settled(100.0, 70.0);
        let lift = |filter: &mut SingleTouchFilter, x: f64, at_ms: u64| filter.filter(mouse(x, 70.0, false), t + at_ms * MS, WIDTH, HEIGHT);
        assert_eq!(lift(&mut filter, 100.0, 17).to_vec(), vec![mouse(100.0, 70.0, false)]);
        // A pair far from the lift, closed by a lift report from its own place.
        assert!(feed(&mut filter, 180.0, 70.0, t + 34 * MS).is_empty());
        assert!(feed(&mut filter, 180.0, 70.0, t + 51 * MS).is_empty());
        assert_eq!(lift(&mut filter, 180.0, 68).to_vec(), vec![mouse(180.0, 70.0, false)]);
        // One frame somewhere else again, and its lift report.
        assert!(feed(&mut filter, 12.6, 70.0, t + 85 * MS).is_empty());
        assert_eq!(lift(&mut filter, 12.6, 102).to_vec(), vec![mouse(12.6, 70.0, false)]);
        // Nine still frames, 88 mm from the lift: one short of what a touch must show.
        for index in 0..8 {
            let frames = feed(&mut filter, 11.9, 70.0, t + 17 * MS * (index + 7));
            assert!(frames.is_empty(), "{} frames in: {frames:?}", index + 2);
        }
        assert_eq!(lift(&mut filter, 11.9, 255).to_vec(), vec![mouse(11.9, 70.0, false)]);
        // And the panel goes on.
        for index in 0..4 {
            let frames = feed(&mut filter, 130.1, 70.0, t + 17 * MS * (index + 16));
            assert!(frames.is_empty(), "{} frames of another place: {frames:?}", index + 1);
        }
        assert_eq!(lift(&mut filter, 130.1, 340).to_vec(), vec![mouse(130.1, 70.0, false)]);
    }

    #[test]
    fn a_touch_amid_the_storm_is_believed_on_its_tenth_frame_with_all_ten() {
        // 2026-11-26, 22:03:01, 2.759 s: a drag that lands 17 ms after a frame of the panel's and
        // its lift report. Believed on its fifth frame, the build of the recording cut it at
        // 3.042 s.
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 180.0, 70.0, t + 34 * MS).is_empty());
        let _ = filter.filter(mouse(180.0, 70.0, false), t + 51 * MS, WIDTH, HEIGHT);
        for index in 0..9 {
            let frames = feed(&mut filter, 150.0, 70.0, t + 17 * MS * (index + 4));
            assert!(frames.is_empty(), "{} frames in: {frames:?}", index + 1);
        }
        let frames = feed(&mut filter, 150.0, 70.0, t + 17 * MS * 13);
        assert_eq!(frames.len(), 10, "{frames:?}");
        assert!(frames.iter().all(|frame| *frame == (150.0, 70.0)));
        assert_eq!(feed(&mut filter, 151.0, 70.0, t + 17 * MS * 14), vec![(151.0, 70.0)]);
    }

    // 2026-11-26, 22:03:01, 3.889 s: a flick upward that lands amid the storm. The panel's own
    // places keep to a row, so a touch that has gone 8 mm across its row is a finger by its
    // third frame.
    #[test]
    fn a_touch_amid_the_storm_that_crosses_its_row_is_believed_on_its_third_frame() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        // The tracker lets the contact go; the panel goes on reporting the tip off, from its origin.
        filter.contact_ended(t + 34 * MS, (100.0, 70.0));
        let _ = filter.filter(mouse(0.0, 0.0, false), t + 51 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 150.0, 70.0, t + 68 * MS).is_empty());
        assert!(feed(&mut filter, 150.0, 75.0, t + 85 * MS).is_empty());
        assert_eq!(feed(&mut filter, 150.0, 80.0, t + 102 * MS).len(), 3);
        assert_eq!(feed(&mut filter, 150.0, 84.0, t + 119 * MS), vec![(150.0, 84.0)]);
    }

    // 2026-11-26, 22:03:03: eight of the thirty frames between a drag's were the panel's, in
    // place of the finger's. One that is not the touch's does not end it.
    #[test]
    fn a_frame_of_the_panels_between_a_touchs_frames_does_not_end_it() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        filter.contact_ended(t + 34 * MS, (100.0, 70.0));
        let _ = filter.filter(mouse(0.0, 0.0, false), t + 51 * MS, WIDTH, HEIGHT);
        for index in 0..3 {
            assert!(feed(&mut filter, 150.0, 70.0, t + 17 * MS * (index + 4)).is_empty(), "{index}");
        }
        assert!(feed(&mut filter, 40.0, 70.0, t + 119 * MS).is_empty(), "the panel's frame, 110 mm away");
        assert_eq!(filter.filter(mouse(40.0, 70.0, false), t + 136 * MS, WIDTH, HEIGHT).to_vec(), vec![mouse(40.0, 70.0, false)]);
        for index in 0..6 {
            let frames = feed(&mut filter, 150.0 + 0.1 * index as f64, 70.0, t + 17 * MS * (index + 9));
            assert!(frames.is_empty(), "{} frames of its own after: {frames:?}", index + 1);
        }
        let frames = feed(&mut filter, 150.6, 70.0, t + 17 * MS * 15);
        assert_eq!(frames.len(), 10, "{frames:?}");
        assert_eq!(frames[0], (150.0, 70.0));
    }

    // A touch held amid the storm that the panel stops is given up: 50 ms after its last frame,
    // 50 ms after its own lift report, and after 150 ms without a frame of its own. The 50 ms
    // after its lift report hold though the panel goes on talking of other places. What follows
    // at its place is judged afresh.
    #[test]
    fn a_touch_amid_the_storm_is_given_up_by_silence_by_its_lift_report_and_by_patience() {
        // The contact over, the storm on, and three frames of a touch 50 mm from the lift.
        fn held() -> (SingleTouchFilter, u64) {
            let (mut filter, t) = settled(100.0, 70.0);
            let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
            filter.contact_ended(t + 34 * MS, (100.0, 70.0));
            let _ = filter.filter(mouse(0.0, 0.0, false), t + 51 * MS, WIDTH, HEIGHT);
            for index in 0..3 {
                assert!(feed(&mut filter, 150.0, 70.0, t + 17 * MS * (index + 4)).is_empty(), "{index}");
            }
            (filter, t)
        }

        // Sixty milliseconds of silence.
        let (mut filter, t) = held();
        for at_ms in [162, 179] {
            let frames = feed(&mut filter, 150.0, 70.0, t + at_ms * MS);
            assert!(frames.is_empty(), "{at_ms} ms: {frames:?}");
        }
        assert_eq!(feed(&mut filter, 150.0, 70.0, t + 196 * MS).len(), 3);

        // Its own lift report: nothing follows it for 68 ms.
        let (mut filter, t) = held();
        assert_eq!(
            filter.filter(mouse(150.0, 70.0, false), t + 119 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse(150.0, 70.0, false)]
        );
        for at_ms in [187, 204] {
            let frames = feed(&mut filter, 150.0, 70.0, t + at_ms * MS);
            assert!(frames.is_empty(), "{at_ms} ms: {frames:?}");
        }
        assert_eq!(feed(&mut filter, 150.0, 70.0, t + 221 * MS).len(), 3);

        // The panel talks all the while, of other places with lift reports: 153 ms pass without a
        // frame of the touch's. What comes at its place then began after another place, and is
        // held to ten frames like any touch amid the storm.
        let (mut filter, t) = held();
        for index in 0..4u64 {
            let x = if index % 2 == 0 { 40.0 } else { 20.0 };
            let at = t + (119 + 34 * index) * MS;
            assert!(feed(&mut filter, x, 70.0, at).is_empty(), "{index}");
            let lift = filter.filter(mouse(x, 70.0, false), at + 17 * MS, WIDTH, HEIGHT);
            assert_eq!(lift.to_vec(), vec![mouse(x, 70.0, false)], "{index}");
        }
        for index in 0..9u64 {
            let frames = feed(&mut filter, 150.0, 70.0, t + (255 + 17 * index) * MS);
            assert!(frames.is_empty(), "{} frames of its own after: {frames:?}", index + 1);
        }
        assert_eq!(feed(&mut filter, 150.0, 70.0, t + 408 * MS).len(), 10);

        // Its own lift report, and the panel goes on talking of other places. The touch is over
        // 50 ms after that report all the same, and does not wait out the 150 ms of patience:
        // were it not, what came at its place would be taken for more of it. What comes there
        // 68 ms after the report is a new touch, held to ten frames of its own, as it began
        // after another place.
        let (mut filter, t) = held();
        assert_eq!(
            filter.filter(mouse(150.0, 70.0, false), t + 119 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse(150.0, 70.0, false)]
        );
        for at_ms in [136, 153] {
            assert!(feed(&mut filter, 40.0, 70.0, t + at_ms * MS).is_empty(), "{at_ms} ms");
        }
        assert_eq!(
            filter.filter(mouse(40.0, 70.0, false), t + 170 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse(40.0, 70.0, false)]
        );
        for index in 0..9u64 {
            let frames = feed(&mut filter, 150.0, 70.0, t + (187 + 17 * index) * MS);
            assert!(frames.is_empty(), "{} frames of what came after: {frames:?}", index + 1);
        }
        assert_eq!(feed(&mut filter, 150.0, 70.0, t + 356 * MS).len(), 10);
    }

    // The storm is over 120 ms after the panel's last report, for good: what comes later is
    // judged as at any other time. Title-bar 2026-11-25, 12.310 s: one frame of the panel's
    // 160 ms after a tap's lift report, then the owner's next tap 50 ms later.
    #[test]
    fn the_storm_is_over_for_good_once_the_panel_has_been_quiet() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        filter.contact_ended(t + 34 * MS, (100.0, 70.0));
        assert!(feed(&mut filter, 180.0, 70.0, t + 51 * MS).is_empty());
        assert!(feed(&mut filter, 40.0, 70.0, t + 100 * MS).is_empty(), "still in the storm, 83 ms after the lift report");
        // Quiet from 100 ms to 230 ms.
        assert!(feed(&mut filter, 48.8, 70.0, t + 230 * MS).is_empty(), "one frame of the panel's, 213 ms after the lift report");
        assert!(feed(&mut filter, 150.0, 70.0, t + 247 * MS).is_empty());
        assert!(feed(&mut filter, 150.0, 70.0, t + 264 * MS).is_empty());
        assert_eq!(feed(&mut filter, 150.0, 70.0, t + 281 * MS).len(), 3, "a press like any other");
    }

    // After a lift the panel reports the tip off from its origin for up to 140 ms before the
    // frames of its own begin (2026-11-26, 22:03:01: the report at 3.464 s, seven from the
    // origin to 3.605 s, its frames from 3.662 s). Judged over at 120 ms without a frame, the
    // storm was over by then, and the first still burst of it was a click at (1563, 688).
    #[test]
    fn the_panels_lift_reports_keep_the_storm_alive() {
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        filter.contact_ended(t + 34 * MS, (100.0, 70.0));
        for index in 0..5 {
            let _ = filter.filter(mouse(0.0, 0.0, false), t + 17 * MS * (index + 3), WIDTH, HEIGHT);
        }
        // A pair at the lift's own place, 0.6 mm from it: not confirmed by its lift report.
        assert!(feed(&mut filter, 100.6, 70.0, t + 17 * MS * 9).is_empty());
        assert!(feed(&mut filter, 100.6, 70.0, t + 17 * MS * 10).is_empty());
        assert_eq!(filter.filter(mouse(100.6, 70.0, false), t + 17 * MS * 11, WIDTH, HEIGHT).to_vec(), vec![mouse(100.6, 70.0, false)]);
        // Then 88 mm along the row.
        for index in 0..8 {
            let frames = feed(&mut filter, 11.9, 70.0, t + 17 * MS * (index + 12));
            assert!(frames.is_empty(), "{} frames: {frames:?}", index + 1);
        }
    }

    // Title-bar 2026-11-25, 12.149 s to 12.450 s: three taps on a title bar, 1.4 and 5.5 mm apart.
    // Between them the panel reported three frames of a touch 107 mm to the left. A tap that
    // stands still near where the finger lifted is the finger back, for half a second: the
    // storm notwithstanding, it is a press like any other. Later than that it waits like any
    // other touch amid the storm: the panel's own still runs come to the lift place too
    // (origin-reports 2026-11-26, 9.426 s: five frames 1 mm from where a tap had lifted, 580 ms
    // after it, then one 47 mm on). One that is moving by its third frame gets no credit at
    // any time: the panel's sweeps begin where the finger was.
    #[test]
    fn a_still_touch_near_where_the_finger_lifted_is_the_finger_back_for_half_a_second() {
        // A lift report, the contact over, one frame of the panel's 60 mm away and its lift
        // report: the storm is on.
        fn storming(filter: &mut SingleTouchFilter, t: u64) {
            let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
            filter.contact_ended(t + 34 * MS, (100.0, 70.0));
            assert!(feed(filter, 39.8, 70.0, t + 51 * MS).is_empty());
            let _ = filter.filter(mouse(39.8, 70.0, false), t + 68 * MS, WIDTH, HEIGHT);
        }

        // 219 ms after the lift report, 52 ms after one of the panel's frames.
        let (mut filter, t) = settled(100.0, 70.0);
        storming(&mut filter, t);
        assert!(feed(&mut filter, 48.8, 70.0, t + 150 * MS).is_empty());
        let _ = filter.filter(mouse(48.8, 70.0, false), t + 167 * MS, WIDTH, HEIGHT);
        for at_ms in [236, 253] {
            let frames = feed(&mut filter, 104.0, 70.0, t + at_ms * MS);
            assert!(frames.is_empty(), "{at_ms} ms: {frames:?}");
        }
        assert_eq!(
            feed(&mut filter, 104.0, 70.0, t + 270 * MS),
            vec![(104.0, 70.0), (104.0, 70.0), (104.0, 70.0)]
        );
        assert_eq!(filter.landing(), Some(Landing { x: 104.0, y: 70.0, at_ns: t + 236 * MS, misread_before: false }));

        // The same tap with the storm still going, 623 ms after the lift report.
        let (mut filter, t) = settled(100.0, 70.0);
        storming(&mut filter, t);
        for at_ms in [150, 250, 350, 450, 560] {
            assert!(feed(&mut filter, 48.8, 70.0, t + at_ms * MS).is_empty(), "{at_ms} ms");
            let lift = filter.filter(mouse(48.8, 70.0, false), t + (at_ms + 17) * MS, WIDTH, HEIGHT);
            assert_eq!(lift.to_vec(), vec![mouse(48.8, 70.0, false)], "{at_ms} ms");
        }
        for at_ms in [640, 657, 674] {
            let frames = feed(&mut filter, 104.0, 70.0, t + at_ms * MS);
            assert!(frames.is_empty(), "{at_ms} ms: {frames:?}");
        }
        assert_eq!(filter.landing(), None);

        // The same, 68 ms after the lift report and moving before it is believed.
        let (mut filter, t) = settled(100.0, 70.0);
        storming(&mut filter, t);
        assert!(feed(&mut filter, 104.0, 70.0, t + 85 * MS).is_empty());
        assert!(feed(&mut filter, 109.0, 70.0, t + 102 * MS).is_empty());
        assert!(feed(&mut filter, 114.0, 70.0, t + 119 * MS).is_empty());
        assert_eq!(filter.landing(), None);
    }

    // Probe 2026-11-24 17:20, 4.329 s, and 2026-11-26, 22:03:01, 2.007 s: one to four frames of
    // a touch somewhere else, their lift report, and the finger's landing. The frames were a
    // press of two; taken for one, the landing that followed was a click of its own at the wrong
    // place, before a scroll. So the touch that proves itself there and then, farther than a
    // finger that stood still could have got, is the landing the panel misread: no click went
    // before it.
    #[test]
    fn a_landing_that_follows_too_fast_after_a_lift_is_the_one_the_panel_misread_before_it() {
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 43.3, 70.0, 0).is_empty());
        assert!(feed(&mut filter, 43.3, 70.0, 17 * MS).is_empty());
        let lift = filter.filter(mouse(43.3, 70.0, false), 34 * MS, WIDTH, HEIGHT);
        assert_eq!(lift.to_vec(), vec![mouse(43.3, 70.0, true), mouse(43.3, 70.0, true), mouse(43.3, 70.0, false)]);
        for index in 0..9 {
            let frames = feed(&mut filter, 103.6, 70.0, 17 * MS * (index + 3));
            assert!(frames.is_empty(), "{index}: {frames:?}");
        }
        let frames = feed(&mut filter, 103.6, 70.0, 17 * MS * 12);
        assert_eq!(frames.len(), 10, "{frames:?}");
        assert_eq!(filter.landing(), Some(Landing { x: 103.6, y: 70.0, at_ns: 17 * MS * 3, misread_before: true }));

        // Where the finger was moving when it lifted, what it left was no misread of the landing:
        // the touch that follows, 28 mm off its course, is a new one.
        let (mut filter, t) = settled(100.0, 70.0);
        assert!(feed(&mut filter, 110.0, 70.0, t + 17 * MS).is_empty());
        assert_eq!(feed(&mut filter, 122.0, 70.0, t + 34 * MS).len(), 2);
        let _ = filter.filter(mouse(122.0, 70.0, false), t + 51 * MS, WIDTH, HEIGHT);
        for index in 0..9 {
            let frames = feed(&mut filter, 150.0, 98.0, t + 17 * MS * (index + 4));
            assert!(frames.is_empty(), "{index}: {frames:?}");
        }
        assert_eq!(feed(&mut filter, 150.0, 98.0, t + 17 * MS * 13).len(), 10);
        assert_eq!(filter.landing(), Some(Landing { x: 150.0, y: 98.0, at_ns: t + 17 * MS * 4, misread_before: false }));
    }

    // While the tracker holds the contact's lift, a touch within the reach the stream told of
    // ([`SingleTouchFilter::told_reach`]) is the tracker's to judge, as it always was: the
    // filter does not hold it to prove itself.
    #[test]
    fn a_touch_within_the_reach_the_tracker_still_takes_is_left_to_the_tracker() {
        for (reach, frames_at) in [(Some(40.0), 5), (Some(10.0), 10)] {
            let (mut filter, t) = settled(100.0, 70.0);
            let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
            filter.told_reach(reach);
            let mut believed_on = 0;
            for index in 0..10 {
                let frames = feed(&mut filter, 130.0, 70.0, t + 17 * MS * (index + 2));
                if !frames.is_empty() {
                    believed_on = index + 1;
                    assert_eq!(frames.len(), frames_at, "reach {reach:?}: {frames:?}");
                    break;
                }
            }
            assert_eq!(believed_on, frames_at as u64, "reach {reach:?}");
        }
    }

    // What the panel reports in the moment after a lift report is judged until it is believed or
    // given up, and meanwhile the stream holds the end of the contact that lifted.
    #[test]
    fn the_touch_that_begins_as_a_finger_lifts_is_judged_until_it_is_believed_or_given_up() {
        // Far from the lift: a touch there would be the landing the panel misread before.
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 180.0, 70.0, t + 34 * MS).is_empty());
        assert_eq!(filter.awaiting(t + 34 * MS), Awaiting::WouldMisread);
        assert_eq!(filter.held_until_ns(), Some(t + 84 * MS));
        // Another place within the time no finger can be in two: the first was the panel's.
        assert!(feed(&mut filter, 40.0, 70.0, t + 51 * MS).is_empty());
        assert_eq!(filter.awaiting(t + 51 * MS), Awaiting::Nothing);

        // Near the lift: the finger back, maybe.
        let (mut filter, t) = settled(100.0, 70.0);
        let _ = filter.filter(mouse(100.0, 70.0, false), t + 17 * MS, WIDTH, HEIGHT);
        assert!(feed(&mut filter, 110.0, 70.0, t + 34 * MS).is_empty());
        assert_eq!(filter.awaiting(t + 34 * MS), Awaiting::Judging);
        assert_eq!(filter.held_until_ns(), Some(t + 84 * MS));
        // Given up 50 ms after its only frame, were nothing more to come.
        assert_eq!(filter.awaiting(t + 84 * MS), Awaiting::Nothing);
        assert_eq!(filter.held_until_ns(), Some(t + 84 * MS));
    }

    // 2026-11-26, 22:03:34: a finger held at the panel's left edge to right-click, and around it
    // the panel reporting sweeps along its row. One began 27 mm from the finger and ran to
    // 94 mm, 73 mm from the finger 89 ms after its last frame: on two frames that moved on
    // from each other it was believed, and the hold became a scroll of 0.41.
    #[test]
    fn a_sweep_along_a_resting_fingers_row_is_not_the_finger() {
        let (mut filter, t) = settled(2.7, 70.0);
        let sweep = [29.6, 29.6, 36.7, 36.7, 76.2, 94.0, 94.0, 97.2];
        for (index, x) in sweep.iter().enumerate() {
            let frames = feed(&mut filter, *x, 70.0, t + 17 * MS * (index as u64 + 1));
            assert!(frames.is_empty(), "{x} mm: {frames:?}");
        }
        assert_eq!(feed(&mut filter, 2.2, 70.0, t + 17 * MS * 9), vec![(2.2, 70.0)]);
    }

    // 2026-11-26, 22:03:34, 4.513 s: the panel reported 130.1 mm twice, then 135.9 mm, with a
    // finger held at the left edge. Such a run, 80 mm from the finger and 51 ms after its last
    // frame, is out of the finger's reach and within what it could have covered had it moved:
    // a chain that began at rest is taken there on five frames (title-bar 2026-11-25, 29.859 s,
    // the next test). One that moved on from its first frames is the panel's, however long it
    // then stands.
    #[test]
    fn a_sweep_that_moves_on_and_then_stands_is_not_the_finger_however_long() {
        let (mut filter, t) = settled(50.0, 70.0);
        let sweep = [130.1, 130.1, 136.5, 136.5, 136.5, 136.5];
        for (index, x) in sweep.iter().enumerate() {
            let frames = feed(&mut filter, *x, 70.0, t + MS * (85 + 17 * index) as u64);
            assert!(frames.is_empty(), "{x} mm: {frames:?}");
        }
        assert_eq!(feed(&mut filter, 50.3, 70.0, t + 187 * MS), vec![(50.3, 70.0)]);
    }

    // 2026-11-27, 16:25:06: a drag going 160 mm/s. The panel reported the tip off from where the
    // finger was, said nothing for 37 ms, and reported the finger 17 mm on, at rest. The tracker
    // would still take it (it told of 19 mm). Believed on its fifth frame, the drag stood still
    // for 87 ms and then jumped; on its third, for 53.
    #[test]
    fn the_finger_back_from_a_flap_is_believed_on_its_third_frame() {
        let (mut filter, t) = settled(78.2, 81.8);
        assert_eq!(feed(&mut filter, 80.0, 79.7, t + 17 * MS), vec![(80.0, 79.7)]);
        let lift = filter.filter(mouse(80.0, 79.7, false), t + 34 * MS, WIDTH, HEIGHT);
        assert_eq!(lift.to_vec(), vec![mouse(80.0, 79.7, false)]);
        for index in 0..2u64 {
            filter.told_reach(Some(19.0));
            let frames = feed(&mut filter, 90.2, 66.6, t + (71 + 17 * index) * MS);
            assert!(frames.is_empty(), "{index}: {frames:?}");
        }
        filter.told_reach(Some(19.0));
        assert_eq!(feed(&mut filter, 90.4, 66.3, t + 105 * MS), vec![(90.2, 66.6), (90.2, 66.6), (90.4, 66.3)]);
    }

    // The same flap where the tracker would not take the finger back (it told of 10 mm): a touch
    // amid the storm, held to ten frames as any.
    #[test]
    fn a_still_chain_beyond_the_trackers_reach_after_a_lift_report_waits_as_before() {
        let (mut filter, t) = settled(78.2, 81.8);
        assert_eq!(feed(&mut filter, 80.0, 79.7, t + 17 * MS), vec![(80.0, 79.7)]);
        let lift = filter.filter(mouse(80.0, 79.7, false), t + 34 * MS, WIDTH, HEIGHT);
        assert_eq!(lift.to_vec(), vec![mouse(80.0, 79.7, false)]);
        for index in 0..9u64 {
            filter.told_reach(Some(10.0));
            let frames = feed(&mut filter, 90.2, 66.6, t + (71 + 17 * index) * MS);
            assert!(frames.is_empty(), "{index}: {frames:?}");
        }
        filter.told_reach(Some(10.0));
        assert_eq!(feed(&mut filter, 90.2, 66.6, t + 224 * MS).len(), 10);
    }

    // 2026-11-26 (origin-reports, 11.463 s): a scroll ended, and 177 ms after its lift report the
    // panel reported a place 29 mm away, three frames still. That is within what the tracker would
    // still have taken for the finger (it told of 32 mm by the third frame), and late: the panel's,
    // which waits for five frames like any chain after a lift report.
    #[test]
    fn a_still_place_long_after_a_lift_report_is_not_the_finger_back_from_a_flap() {
        let (mut filter, t) = settled(120.2, 80.8);
        let lift = filter.filter(mouse(120.2, 80.8, false), t + 17 * MS, WIDTH, HEIGHT);
        assert_eq!(lift.to_vec(), vec![mouse(120.2, 80.8, false)]);
        for index in 0..3u64 {
            filter.told_reach(Some(32.0));
            let frames = feed(&mut filter, 146.6, 69.8, t + (194 + 17 * index) * MS);
            assert!(frames.is_empty(), "{index}: {frames:?}");
        }
        // Their lift report confirms nothing: three frames, and not a pair.
        let lift = filter.filter(mouse(146.6, 69.8, false), t + 245 * MS, WIDTH, HEIGHT);
        assert_eq!(lift.to_vec(), vec![mouse(146.6, 69.8, false)]);
    }

    // 2026-11-27, 16:25:08: after 105 ms without the finger the filter took it for slow, and its
    // next frames were 8 mm and 5 mm on. Each within the trust distance of the chain's first
    // frame, so a chain at rest by the letter, though the finger was going 460 mm/s. Held for a
    // third frame, the drag stood still for 36 ms and jumped 14 mm. The first frame is the report
    // right after the finger's, so this is the finger leaving fast, believed on its second frame.
    // A chain whose first frame comes later is judged as before (`a_place_on_the_fingers_line_far_ahead_is_not_the_finger`).
    #[test]
    fn a_finger_that_leaves_fast_after_the_filter_took_it_for_slow_is_believed_on_its_second_frame() {
        let (mut filter, t) = settled(96.4, 125.0);
        assert_eq!(feed(&mut filter, 91.5, 127.5, t + 105 * MS), vec![(91.5, 127.5)]);
        assert!(feed(&mut filter, 83.6, 126.5, t + 122 * MS).is_empty());
        assert_eq!(feed(&mut filter, 79.0, 125.0, t + 139 * MS), vec![(83.6, 126.5), (79.0, 125.0)]);
    }

    // Nor one that starts a report or more after the finger's last sighting: 51 ms passed, and
    // the finger may have stopped.
    #[test]
    fn a_chain_that_starts_after_a_gap_is_no_finger_leaving_fast() {
        let (mut filter, t) = settled(96.4, 125.0);
        assert_eq!(feed(&mut filter, 91.5, 127.5, t + 17 * MS), vec![(91.5, 127.5)]);
        assert!(feed(&mut filter, 83.6, 126.5, t + 68 * MS).is_empty());
        assert!(feed(&mut filter, 79.0, 125.0, t + 85 * MS).is_empty(), "51 ms after the finger: a chain at rest still");
        assert_eq!(feed(&mut filter, 75.0, 124.0, t + 102 * MS), vec![(83.6, 126.5), (79.0, 125.0), (75.0, 124.0)]);
    }

    // The report right after the finger's, but 12 mm and more from where it was, is no finger
    // leaving fast: the chain is one at rest, and waits for its third frame.
    #[test]
    fn a_chain_that_starts_farther_than_twice_the_trust_distance_is_no_finger_leaving_fast() {
        let (mut filter, t) = settled(91.5, 127.5);
        assert!(feed(&mut filter, 79.2, 126.5, t + 17 * MS).is_empty());
        assert!(feed(&mut filter, 75.5, 126.0, t + 34 * MS).is_empty(), "12.4 mm from the finger: a chain at rest still");
        assert_eq!(feed(&mut filter, 72.0, 125.5, t + 51 * MS), vec![(79.2, 126.5), (75.5, 126.0), (72.0, 125.5)]);
    }

    // Title-bar 2026-11-25, 29.859 s: three frames 9 mm to the right of a slow drag, then the
    // finger 15 mm to the left of them, still. The last frames the filter believed were the
    // panel's, so the finger seems to have left where it was at 900 mm/s: out of reach by the
    // letter, and yet the finger, back at rest. A chain that began at rest is taken where the
    // finger could have got to, once five frames have come without it.
    #[test]
    fn a_finger_that_comes_back_at_rest_after_frames_of_the_panels_is_believed_on_five_frames() {
        let (mut filter, t) = settled(172.4, 70.0);
        assert_eq!(feed(&mut filter, 173.6, 70.0, t + 17 * MS), vec![(173.6, 70.0)]);
        assert_eq!(feed(&mut filter, 176.6, 70.0, t + 34 * MS), vec![(176.6, 70.0)]);
        assert_eq!(feed(&mut filter, 181.2, 70.0, t + 51 * MS), vec![(181.2, 70.0)]);
        for index in 0..4 {
            let frames = feed(&mut filter, 165.9, 70.0, t + 17 * MS * (index + 4));
            assert!(frames.is_empty(), "{} frames in: {frames:?}", index + 1);
        }
        assert_eq!(
            feed(&mut filter, 165.9, 70.0, t + 17 * MS * 8),
            vec![(165.9, 70.0), (165.9, 70.0), (165.9, 70.0), (165.9, 70.0), (165.9, 70.0)]
        );
        assert_eq!(feed(&mut filter, 165.9, 70.0, t + 17 * MS * 9), vec![(165.9, 70.0)]);
    }

    // 2026-11-27, 00:21:22, 1.844 s: one frame of a touch 11.8 mm from where the finger landed
    // 19 ms later, and the finger still from its first frame. Taken for the landing's first
    // frame, the contact began there and moved: a slow drag began with a jump, and a tap with a
    // nudge.
    #[test]
    fn a_frame_from_elsewhere_just_before_a_landing_is_no_part_of_it() {
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 195.3, 103.0, 0).is_empty());
        assert!(feed(&mut filter, 183.5, 103.1, 19 * MS).is_empty());
        assert!(feed(&mut filter, 183.7, 103.0, 36 * MS).is_empty());
        assert_eq!(
            feed(&mut filter, 184.5, 103.0, 53 * MS),
            vec![(183.5, 103.1), (183.7, 103.0), (184.5, 103.0)]
        );
        assert_eq!(filter.landing(), Some(Landing { x: 183.5, y: 103.1, at_ns: 19 * MS, misread_before: false }));

        // A first step a finger makes from where it landed is the landing's own.
        let mut filter = SingleTouchFilter::default();
        assert!(feed(&mut filter, 180.0, 70.0, 0).is_empty());
        assert!(feed(&mut filter, 186.5, 70.0, 19 * MS).is_empty());
        assert_eq!(feed(&mut filter, 186.5, 70.0, 38 * MS), vec![(180.0, 70.0), (186.5, 70.0), (186.5, 70.0)]);

        // Not while a finger is down with no lift report come: there a fast step that stops is
        // one of the panel's sweeps pausing, and the chain is dropped like any other.
        let (mut filter, t) = settled(100.0, 70.0);
        assert!(feed(&mut filter, 180.0, 70.0, t + 17 * MS).is_empty());
        assert!(feed(&mut filter, 197.0, 70.0, t + 34 * MS).is_empty());
        assert!(feed(&mut filter, 197.0, 70.0, t + 51 * MS).is_empty());
        assert!(feed(&mut filter, 197.0, 70.0, t + 68 * MS).is_empty());
        assert!(feed(&mut filter, 197.0, 70.0, t + 85 * MS).is_empty());
        assert_eq!(feed(&mut filter, 100.0, 70.0, t + 102 * MS), vec![(100.0, 70.0)]);
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
