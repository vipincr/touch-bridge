//! The panel's reports as contact events: the jump filter in front of the contact tracker, and
//! the two rules that need both.

use smallvec::SmallVec;

use crate::filter::SingleTouchFilter;
use crate::report::TouchFrame;
use crate::tracker::{ContactEvent, ContactPhase, ContactTracker};

/// The panel's reports turned into began, moved and ended events. [`SingleTouchFilter`] decides
/// which frames of the single-contact stream carry the finger, [`ContactTracker`] when a contact
/// begins and ends, and each knows something the other needs:
///
/// - A tip-down frame the filter holds back is the panel talking all the same. The tracker holds
///   a lift report until the panel has gone quiet, and around a lift the panel keeps reporting
///   places of its own, so the filter passes the word on ([`ContactTracker::heard`]).
/// - When the tracker ends a contact, the filter is told where it ended
///   ([`SingleTouchFilter::contact_ended`]): what the panel reports next is a new touch, judged
///   as one, and for a moment a touch somewhere else is still taken for a misread.
pub struct ContactStream {
    repair: SingleTouchFilter,
    tracker: ContactTracker,
    panel_width_mm: f64,
    panel_height_mm: f64,
}

impl ContactStream {
    /// A stream for a panel whose active area is `panel_width_mm` by `panel_height_mm`.
    pub fn for_panel(panel_width_mm: f64, panel_height_mm: f64) -> Self {
        Self {
            repair: SingleTouchFilter::default(),
            tracker: ContactTracker::for_panel(panel_width_mm, panel_height_mm),
            panel_width_mm,
            panel_height_mm,
        }
    }

    /// One report of the panel, which arrived at `at_ns`: the contact events it makes. A frame
    /// the filter holds back for support makes none.
    pub fn report(&mut self, frame: TouchFrame, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        let repaired = self.repair.filter(frame, at_ns, self.panel_width_mm, self.panel_height_mm);
        if repaired.is_empty() {
            self.tracker.heard(at_ns);
        }
        let mut events = SmallVec::new();
        for frame in repaired {
            let made = self.tracker.update(&frame, at_ns);
            self.ended(&made, at_ns);
            events.extend(made);
        }
        events
    }

    /// What has fallen due at `at_ns` with no report since: a lift report the panel has gone
    /// quiet after, or a press that has stopped reporting.
    pub fn expire(&mut self, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        let events = self.tracker.expire(at_ns);
        self.ended(&events, at_ns);
        events
    }

    /// The earliest moment the stream ends a contact with no further input, if it will. The
    /// caller sleeps until then or the next report, whichever comes first.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        self.tracker.next_deadline_ns()
    }

    /// Forgets every finger and every lift held: the panel has been released, or its display has
    /// changed under it.
    pub fn reset(&mut self) {
        self.tracker = ContactTracker::for_panel(self.panel_width_mm, self.panel_height_mm);
        self.repair.reset();
    }

    /// Tells the repair where a contact ended, unless another began in the same breath. That is
    /// the tracker splitting one touch from the next: the frames are the new touch's, and the
    /// repair is already following them.
    fn ended(&mut self, events: &[ContactEvent], at_ns: u64) {
        let Some(ended) = events.iter().find(|event| event.phase == ContactPhase::Ended) else {
            return;
        };
        if events.iter().any(|event| event.phase == ContactPhase::Began) {
            return;
        }
        self.repair.contact_ended(at_ns, (ended.x * self.panel_width_mm, ended.y * self.panel_height_mm));
    }
}
