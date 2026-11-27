//! The panel's reports as contact events: the jump filter in front of the contact tracker, and
//! the rules that need both.

use smallvec::{SmallVec, smallvec};

use crate::filter::{Awaiting, SingleTouchFilter};
use crate::report::TouchFrame;
use crate::tracker::{ContactEvent, ContactPhase, ContactTracker};

/// The panel's reports turned into began, moved and ended events. [`SingleTouchFilter`] decides
/// which frames of the single-contact stream carry the finger, [`ContactTracker`] when a contact
/// begins and ends, and each knows something the other needs:
///
/// - A tip-down frame the filter holds back is the panel talking all the same. The tracker holds
///   a lift report until the panel has gone quiet, and around a lift the panel keeps reporting
///   places of its own, so the filter passes the word on ([`ContactTracker::heard`]).
/// - When the end of a contact goes out, the filter is told where it ended
///   ([`SingleTouchFilter::contact_ended`]): what the panel reports next is a new touch. While the
///   tracker holds a contact's lift, the filter is told how far from it the tracker would still
///   take a touch for that contact ([`SingleTouchFilter::told_reach`]): such a touch is the
///   tracker's to judge, as it always was.
/// - A chain is passed on at once, when the filter believes it. Its first frame is announced with
///   the time it came, not the time it was believed.
/// - The end of a contact is not let out at once. A finger cannot lift and land again in
///   [`SingleTouchFilter::storm_start_ns`], so until then, and for as long as a touch that began
///   then is still being judged ([`SingleTouchFilter::awaiting`]), the end waits. If that touch
///   proves to be one, the contact that ended was the panel's misread of its landing
///   ([`SingleTouchFilter::landing`]): its end goes out marked ([`ContactEvent::misread`]) and is
///   no click. If it does not, the end goes out as it was.
pub struct ContactStream {
    repair: SingleTouchFilter,
    tracker: ContactTracker,
    panel_width_mm: f64,
    panel_height_mm: f64,
    /// The end of the last contact, while a touch that began in the moment of its lift is judged.
    held: Option<ContactEvent>,
}

impl ContactStream {
    /// A stream for a panel whose active area is `panel_width_mm` by `panel_height_mm`.
    pub fn for_panel(panel_width_mm: f64, panel_height_mm: f64) -> Self {
        Self {
            repair: SingleTouchFilter::default(),
            tracker: ContactTracker::for_panel(panel_width_mm, panel_height_mm),
            panel_width_mm,
            panel_height_mm,
            held: None,
        }
    }

    /// One report of the panel, which arrived at `at_ns`: the contact events it makes. A frame
    /// the filter holds back, for support or to be proven a finger, makes none.
    pub fn report(&mut self, frame: TouchFrame, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        self.repair.told_reach(self.tracker.adoption_reach_mm(at_ns));
        let repaired = self.repair.filter(frame, at_ns, self.panel_width_mm, self.panel_height_mm);
        if repaired.is_empty() {
            self.tracker.heard(at_ns);
        }
        let landing = self.repair.landing();
        let mut events = SmallVec::new();
        for (index, frame) in repaired.iter().enumerate() {
            let mut made = self.tracker.update(frame, at_ns);
            if index == 0
                && let Some(landing) = landing
                && let Some(began) = made.iter_mut().find(|event| event.phase == ContactPhase::Began)
            {
                began.at_ns = landing.at_ns;
            }
            let misread = landing.is_some_and(|landing| landing.misread_before);
            events.extend(self.let_out(made, misread, at_ns));
        }
        events.extend(self.release(at_ns));
        events
    }

    /// What has fallen due at `at_ns` with no report since: a lift report the panel has gone
    /// quiet after, a press that has stopped reporting, or an end that was waiting for the moment
    /// a finger could be back.
    pub fn expire(&mut self, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        let made = self.tracker.expire(at_ns);
        let mut events = self.let_out(made, false, at_ns);
        events.extend(self.release(at_ns));
        events
    }

    /// The earliest moment the stream ends a contact with no further input, if it will: the
    /// tracker's, or, with an end held, the moment the touch that began as the contact lifted is
    /// given up. The caller sleeps until then or the next report, whichever comes first.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        let mut earliest = self.tracker.next_deadline_ns();
        if self.held.is_some()
            && let Some(free) = self.repair.held_until_ns()
            && earliest.is_none_or(|known| free < known)
        {
            earliest = Some(free);
        }
        earliest
    }

    /// The repair, for a debug build's instruments.
    #[cfg(feature = "debug")]
    pub fn repair(&self) -> &SingleTouchFilter {
        &self.repair
    }

    /// The repair, to try other settings on a recording: a debug build's instruments only.
    #[cfg(feature = "debug")]
    pub fn repair_mut(&mut self) -> &mut SingleTouchFilter {
        &mut self.repair
    }

    /// Forgets every finger, every lift and every end held: the panel has been released, or its
    /// display has changed under it.
    pub fn reset(&mut self) {
        self.tracker = ContactTracker::for_panel(self.panel_width_mm, self.panel_height_mm);
        self.repair.reset();
        self.held = None;
    }

    /// What goes out of the tracker's events. An end goes out at once, unless a touch that began
    /// as the contact lifted is still being judged ([`Self::waits`]): then it waits for that
    /// touch's verdict, and the filter goes on judging with what it has ([`Self::ended`] waits
    /// for the end to leave: told the contact is over, it drops the chain). An end that was
    /// waiting goes out first. A touch that begins now has `misread` for its verdict on the
    /// contact that ended before it.
    fn let_out(&mut self, made: SmallVec<[ContactEvent; 16]>, misread: bool, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        let begins = made.iter().any(|event| event.phase == ContactPhase::Began);
        let mut events: SmallVec<[ContactEvent; 16]> = SmallVec::new();
        // A touch begins: the end that waited has its verdict, or was only waiting for the moment to pass.
        if begins
            && let Some(mut held) = self.held.take()
        {
            held.misread = misread;
            events.push(held);
        }
        for mut event in made {
            if event.phase != ContactPhase::Ended {
                events.push(event);
            } else if begins {
                // The tracker splitting one touch from the next: the frames are the new touch's,
                // and the filter is already following them.
                event.misread = misread;
                events.push(event);
            } else if self.waits(at_ns) {
                self.held = Some(event);
            } else {
                events.push(self.ended(event, misread, at_ns));
            }
        }
        events
    }

    /// The end that was held, once nothing waits any more.
    fn release(&mut self, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        match self.held.take() {
            Some(held) if !self.waits(at_ns) => smallvec![self.ended(held, held.misread, at_ns)],
            held => {
                self.held = held;
                smallvec![]
            }
        }
    }

    /// Whether an end must wait at `at_ns`: a touch that began in the moment after the lift
    /// report is still being judged.
    fn waits(&self, at_ns: u64) -> bool {
        self.repair.awaiting(at_ns) != Awaiting::Nothing
    }

    /// An end that goes out on its own at `at_ns`, with `misread`, the verdict on the touch that
    /// began as the contact lifted. The filter is told where the contact ended
    /// ([`SingleTouchFilter::contact_ended`]): what the panel reports next is a new touch.
    fn ended(&mut self, mut event: ContactEvent, misread: bool, at_ns: u64) -> ContactEvent {
        event.misread = misread;
        self.repair.contact_ended(at_ns, (event.x * self.panel_width_mm, event.y * self.panel_height_mm));
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Contact, Source};

    const WIDTH: f64 = 221.9;
    const HEIGHT: f64 = 147.9;
    const MS: u64 = 1_000_000;

    fn mouse(x: f64, y: f64, touching: bool) -> TouchFrame {
        TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact { id: 0, touching, x: x / WIDTH, y: y / HEIGHT }],
            scan_time: None,
        }
    }

    /// What goes out of the stream up to `at_ns` with no report: each deadline as it falls due,
    /// as the pipeline's timer runs it.
    fn quiet_until(stream: &mut ContactStream, at_ns: u64) -> Vec<ContactEvent> {
        let mut events = Vec::new();
        for _ in 0..16 {
            match stream.next_deadline_ns() {
                Some(deadline) if deadline < at_ns => events.extend(stream.expire(deadline)),
                _ => break,
            }
        }
        events
    }

    /// A report at `at_ns`: what fell due before it, then what it makes.
    fn report(stream: &mut ContactStream, frame: TouchFrame, at_ns: u64) -> Vec<ContactEvent> {
        let mut events = quiet_until(stream, at_ns);
        events.extend(stream.report(frame, at_ns));
        events
    }

    /// What a list of events says, to a tenth of a millimetre and a millisecond: the phase,
    /// where, when, and whether the end was marked misread.
    fn said(events: &[ContactEvent]) -> Vec<(ContactPhase, f64, f64, u64, bool)> {
        events
            .iter()
            .map(|event| {
                (
                    event.phase,
                    (event.x * WIDTH * 10.0).round() / 10.0,
                    (event.y * HEIGHT * 10.0).round() / 10.0,
                    event.at_ns / MS,
                    event.misread,
                )
            })
            .collect()
    }

    /// A tap at (`x`, `y`): its three frames and its lift report, and the stream's Began.
    fn tap(stream: &mut ContactStream, x: f64, y: f64) -> Vec<ContactEvent> {
        let mut events = Vec::new();
        for at_ms in [0, 17, 34] {
            events.extend(report(stream, mouse(x, y, at_ms != 34), at_ms * MS));
        }
        events
    }

    // 2026-11-26, 22:03:01, 2.007 s to 2.565 s: a press of two frames at (43, 21) mm, its lift
    // report, and 67 ms after that the finger landing 60 mm away. The two frames were a click at
    // (1700, 404) before the scroll. The scroll is dated from where and when it really began.
    #[test]
    fn the_end_of_the_touch_a_landing_proves_misread_goes_out_marked_and_the_landing_from_its_first_frame() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        assert_eq!(
            said(&tap(&mut stream, 180.0, 70.0)),
            vec![(ContactPhase::Began, 180.0, 70.0, 0, false)],
            "a press of two frames, confirmed by its lift report"
        );
        // The tracker gives the first contact up 150 ms after its lift report. That end waits.
        for index in 0..9 {
            let events = report(&mut stream, mouse(150.0, 70.0, true), (51 + 17 * index) * MS);
            assert!(events.is_empty(), "{index} frames of the landing in: {events:?}");
        }
        assert_eq!(
            said(&report(&mut stream, mouse(150.0, 70.0, true), 204 * MS)),
            vec![(ContactPhase::Ended, 180.0, 70.0, 34, true), (ContactPhase::Began, 150.0, 70.0, 51, false)]
        );
    }

    // 2026-11-26, 22:03:01, 7.030 s: a tap, then a pair of the panel's 30 mm away, closed by a
    // lift report of its own. Nothing proves itself, and the tap's end goes out as it was.
    #[test]
    fn the_end_of_a_touch_that_no_landing_proves_misread_goes_out_unmarked() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        assert_eq!(said(&tap(&mut stream, 180.0, 70.0)), vec![(ContactPhase::Began, 180.0, 70.0, 0, false)]);
        assert!(report(&mut stream, mouse(150.0, 70.0, true), 51 * MS).is_empty());
        assert!(report(&mut stream, mouse(150.0, 70.0, true), 68 * MS).is_empty());
        assert!(report(&mut stream, mouse(150.0, 70.0, false), 85 * MS).is_empty(), "a pair of the panel's, lift report or no");
        assert_eq!(said(&quiet_until(&mut stream, 400 * MS)), vec![(ContactPhase::Ended, 180.0, 70.0, 34, false)]);
    }

    // 2026-11-26, 22:03:01, 3.4 s to 4.0 s: a scroll ended, the panel reporting the tip off from
    // its origin, then a pair at the lift's place, eight still frames 88 mm along the row, and
    // their lift reports. The still burst was a click at (1563, 688).
    #[test]
    fn a_tap_followed_by_the_panels_storm_is_one_tap() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        assert_eq!(said(&tap(&mut stream, 100.0, 70.0)), vec![(ContactPhase::Began, 100.0, 70.0, 0, false)]);
        // The origin's report, after the finger's own: the lift.
        assert_eq!(said(&report(&mut stream, mouse(0.0, 0.0, false), 51 * MS)), vec![(ContactPhase::Ended, 100.0, 70.0, 34, false)]);
        for index in 1..5 {
            assert!(report(&mut stream, mouse(0.0, 0.0, false), (51 + 17 * index) * MS).is_empty(), "{index}");
        }
        assert!(report(&mut stream, mouse(100.6, 70.0, true), 153 * MS).is_empty());
        assert!(report(&mut stream, mouse(100.6, 70.0, true), 170 * MS).is_empty());
        assert!(report(&mut stream, mouse(100.6, 70.0, false), 187 * MS).is_empty());
        for index in 0..8 {
            assert!(report(&mut stream, mouse(11.9, 70.0, true), (204 + 17 * index) * MS).is_empty(), "{index}");
        }
        assert!(report(&mut stream, mouse(11.9, 70.0, false), 340 * MS).is_empty());
        assert!(quiet_until(&mut stream, 800 * MS).is_empty());
    }

    // Title-bar 2026-11-25, 12.149 s to 12.450 s: taps on a title bar. Between the first two the
    // panel reported three frames of a touch 107 mm to the left, the tip off from its origin, and
    // one frame 9 mm on from there. The first tap's end waits for a verdict on those frames and
    // goes out unmarked; the second tap is its own contact, dated from its first frame.
    #[test]
    fn two_taps_with_the_panels_places_between_them_are_two_contacts() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        assert_eq!(said(&tap(&mut stream, 147.5, 39.0)), vec![(ContactPhase::Began, 147.5, 39.0, 0, false)]);
        for at_ms in [55, 73, 90] {
            assert!(report(&mut stream, mouse(39.8, 39.2, true), at_ms * MS).is_empty(), "{at_ms} ms");
        }
        // The tracker gives the first tap up at the origin's report. Were a touch to prove itself
        // there, 107 mm away, what lifted would have been its misread: the end waits.
        assert!(report(&mut stream, mouse(0.0, 0.0, false), 107 * MS).is_empty());
        // Fifty milliseconds after their last frame, the frames are given up.
        assert_eq!(
            said(&report(&mut stream, mouse(48.8, 37.2, true), 159 * MS)),
            vec![(ContactPhase::Ended, 147.5, 39.0, 34, false)]
        );
        assert!(report(&mut stream, mouse(0.0, 0.0, false), 173 * MS).is_empty());
        for at_ms in [221, 238] {
            assert!(report(&mut stream, mouse(146.1, 39.0, true), at_ms * MS).is_empty(), "{at_ms} ms");
        }
        assert_eq!(
            said(&report(&mut stream, mouse(146.1, 39.0, true), 255 * MS)),
            vec![(ContactPhase::Began, 146.1, 39.0, 221, false)]
        );
        assert!(report(&mut stream, mouse(146.1, 39.0, false), 272 * MS).is_empty());
        assert_eq!(said(&quiet_until(&mut stream, 700 * MS)), vec![(ContactPhase::Ended, 146.1, 39.0, 255, false)]);
    }

    // A drag that flaps: the panel reports the lift while the finger is going, and the finger is
    // back 51 ms after its last frame, 20 mm on and off the course the filter still knows, and at
    // rest. The tracker holds a lift for a finger that was moving, and takes a touch for the same
    // contact for 250 ms: the stream tells the filter how far (30 mm by then), and the finger is
    // believed on its third frame, as any still chain is. Held as a place of the panel's in the
    // storm after a lift, the frames were given up, and the drag ended where it flapped; until
    // 2026-11-27 they waited for a fifth.
    #[test]
    fn a_drag_that_flaps_goes_on_where_the_tracker_still_takes_the_finger() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        let mut said_so_far = Vec::new();
        for (index, x) in [100.0, 110.2, 120.4, 130.6, 140.8].into_iter().enumerate() {
            said_so_far.extend(said(&report(&mut stream, mouse(x, 70.0, true), 17 * MS * index as u64)));
        }
        assert_eq!(
            said_so_far,
            vec![
                (ContactPhase::Began, 100.0, 70.0, 0, false),
                (ContactPhase::Moved, 110.2, 70.0, 34, false),
                (ContactPhase::Moved, 120.4, 70.0, 34, false),
                (ContactPhase::Moved, 130.6, 70.0, 51, false),
                (ContactPhase::Moved, 140.8, 70.0, 68, false),
            ]
        );
        // The lift report: held by the tracker.
        assert!(report(&mut stream, mouse(140.8, 70.0, false), 85 * MS).is_empty());
        for index in 0..2u64 {
            let events = report(&mut stream, mouse(161.2, 70.0, true), (119 + 17 * index) * MS);
            assert!(events.is_empty(), "{index}: {events:?}");
        }
        assert_eq!(said(&report(&mut stream, mouse(161.2, 70.0, true), 153 * MS)), vec![(ContactPhase::Moved, 161.2, 70.0, 153, false)]);
        // The finger stands there: its frames go through, and at one place make no event.
        for at_ms in [170, 187] {
            assert!(report(&mut stream, mouse(161.2, 70.0, true), at_ms * MS).is_empty(), "{at_ms} ms");
        }
        assert!(report(&mut stream, mouse(161.2, 70.0, false), 204 * MS).is_empty());
        assert_eq!(said(&quiet_until(&mut stream, 500 * MS)), vec![(ContactPhase::Ended, 161.2, 70.0, 187, false)]);
    }

    // The same flap, and the finger comes back moving on: it waits for five frames of its own, as
    // any chain after a lift report. Believed on their second frame where the tracker would still
    // take the finger (tried, 2026-11-27), moving chains cost the recordings taps: the panel's
    // sweeps begin where the finger lifted.
    #[test]
    fn a_finger_that_comes_back_from_a_flap_moving_on_waits_for_five_frames() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        let mut said_so_far = Vec::new();
        for (index, x) in [100.0, 110.2, 120.4, 130.6, 140.8].into_iter().enumerate() {
            said_so_far.extend(said(&report(&mut stream, mouse(x, 70.0, true), 17 * MS * index as u64)));
        }
        assert_eq!(said_so_far.len(), 5, "{said_so_far:?}");
        // The lift report: held by the tracker.
        assert!(report(&mut stream, mouse(140.8, 70.0, false), 85 * MS).is_empty());
        // The finger is back 34 ms after the lift report, 20 mm on, and its frames go on across
        // the finger's course at 200 mm/s: no frame of them lies where the finger was heading.
        for (index, y) in [70.0, 77.0, 84.0, 91.0].into_iter().enumerate() {
            let events = report(&mut stream, mouse(161.2, y, true), (119 + 34 * index as u64) * MS);
            assert!(events.is_empty(), "{index}: {events:?}");
        }
        // Believed on the fifth, the five go out together.
        assert_eq!(
            said(&report(&mut stream, mouse(161.2, 98.0, true), 255 * MS)),
            vec![
                (ContactPhase::Moved, 161.2, 70.0, 255, false),
                (ContactPhase::Moved, 161.2, 77.0, 255, false),
                (ContactPhase::Moved, 161.2, 84.0, 255, false),
                (ContactPhase::Moved, 161.2, 91.0, 255, false),
                (ContactPhase::Moved, 161.2, 98.0, 255, false),
            ]
        );
        assert!(report(&mut stream, mouse(161.2, 98.0, false), 289 * MS).is_empty());
        assert_eq!(said(&quiet_until(&mut stream, 600 * MS)), vec![(ContactPhase::Ended, 161.2, 98.0, 255, false)]);
    }

    // 2026-11-26, 22:03:01, 2.007 s, had the landing been a flick upward from 42 mm off, with two
    // frames of the panel's own in between (so that the tracker holds the lift, and does not
    // give the tap up for silence). After a lift report a touch is believed on five frames;
    // these came 119 ms after the press's lift report, while the tracker still holds it (150 ms).
    // The tracker ends the one contact and begins the other in one breath, and the end goes out
    // between them marked: the frames are the landing's, and the filter has judged them.
    #[test]
    fn the_end_the_tracker_makes_as_the_landing_begins_goes_out_marked() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        assert_eq!(said(&tap(&mut stream, 180.0, 70.0)), vec![(ContactPhase::Began, 180.0, 70.0, 0, false)]);
        for at_ms in [51, 68] {
            let events = report(&mut stream, mouse(11.9, 70.0, true), at_ms * MS);
            assert!(events.is_empty(), "{at_ms} ms: {events:?}");
        }
        for index in 0..4u64 {
            let events = report(&mut stream, mouse(150.0, 100.0 + 8.0 * index as f64, true), (85 + 17 * index) * MS);
            assert!(events.is_empty(), "{index}: {events:?}");
        }
        assert_eq!(
            said(&report(&mut stream, mouse(150.0, 132.0, true), 153 * MS)),
            vec![
                (ContactPhase::Ended, 180.0, 70.0, 34, true),
                (ContactPhase::Began, 150.0, 100.0, 85, false),
                (ContactPhase::Moved, 150.0, 108.0, 153, false),
                (ContactPhase::Moved, 150.0, 116.0, 153, false),
                (ContactPhase::Moved, 150.0, 124.0, 153, false),
                (ContactPhase::Moved, 150.0, 132.0, 153, false),
            ]
        );
    }

    // A tap, then a burst of the panel's that falls short of ten frames, and quiet. The tracker
    // gives the tap up 150 ms after its lift report; the burst is given up 50 ms after its last
    // frame. The tap's end waits for that, and goes out then with no report to wake the stream:
    // the stream's next deadline is the burst's.
    #[test]
    fn the_end_that_waits_for_the_panels_burst_goes_out_when_the_burst_is_given_up() {
        let mut stream = ContactStream::for_panel(WIDTH, HEIGHT);
        assert_eq!(said(&tap(&mut stream, 180.0, 70.0)), vec![(ContactPhase::Began, 180.0, 70.0, 0, false)]);
        for index in 0..8u64 {
            let events = report(&mut stream, mouse(150.0, 70.0, true), (51 + 17 * index) * MS);
            assert!(events.is_empty(), "{index}: {events:?}");
        }
        assert_eq!(said(&quiet_until(&mut stream, 400 * MS)), vec![(ContactPhase::Ended, 180.0, 70.0, 34, false)]);
    }
}
