//! Turns successive frames into began/moved/ended events per contact id: the
//! identity layer, playing the role the kernel's input-mt plays under Linux.

use std::collections::{BTreeMap, BTreeSet};

use smallvec::SmallVec;

use crate::report::{Contact, TouchFrame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactPhase {
    Began,
    Moved,
    Ended,
}

/// A per-finger lifecycle change. Coordinates are normalized to 0..=1 across
/// the panel.
// No Eq: coordinates are f64.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactEvent {
    pub id: u8,
    pub phase: ContactPhase,
    pub x: f64,
    pub y: f64,
}

/// One finger on the panel, with what the lift policy needs to know about it.
#[derive(Debug, Clone, Copy)]
struct Press {
    x: f64,
    y: f64,
    /// Frame time of the last report for this press, ns.
    last_at_ns: u64,
    /// Exponential average of speed, mm/s.
    speed: f64,
}

/// Turns successive frames into began/moved/ended events per contact id: the
/// identity layer, playing the role the kernel's input-mt plays under Linux. A
/// contact ends when its tip lifts, when it disappears from a frame, or when no
/// frame arrives for `lift_timeout_ns` (call [`ContactTracker::expire`] from a
/// timer to catch a lost lift report). The kernel's own timeout for "device
/// forgot the liftoff" is 100 ms (hid-multitouch release_timer); that is this
/// type's default (100 ms). A lift report is held back, because the JN-MD-IPST105FP reports
/// the tip off while the finger is still down (116 flaps in three recordings, 2026-11-24 and
/// 2026-11-25, the longest of eight frames). The press goes on if the tip is back within
/// `debounce_ns` (250 ms) and within a finger's travel of where it was. A held lift ends
/// sooner on two signs that the finger is gone, and a real lift is recognised 50 ms after the
/// panel's last report: no report of any kind for `lift_silence_ns` (50 ms; the panel reports
/// every 12 to 22 ms while a finger is down, 38 ms apart at the most when it skips one, and
/// falls quiet after a lift; a tip-on report the jump filter dropped still counts, see
/// [`ContactTracker::heard`]), or a lift report at the panel's origin.
///
/// A returning contact continues the press only within a velocity-scaled radius of where the
/// finger was, whatever the stream: after a lift report, a touch beyond a finger's travel is a
/// new touch (this panel reports one to four frames somewhere else as the finger leaves). The
/// return may carry a different contact id: controllers that renumber a finger mid-press
/// (digitizer id churn, or the mouse stream's contact 0 after a flap) would otherwise end and
/// restart the gesture. The kernel's input_mt_assign_slots solves the same problem with a
/// bipartite match; one finger at a time, a radius gate is that match.
#[derive(Debug)]
pub struct ContactTracker {
    active: BTreeMap<u8, Press>,
    /// Lifts held back, with the frame time they happened at.
    pending: BTreeMap<u8, (Press, u64)>,
    /// Frame contact id → the id this finger continues as (cross-id adoption).
    alias: BTreeMap<u8, u8>,
    last_frame_ns: u64,
    lift_timeout_ns: u64,
    debounce_ns: u64,
    lift_silence_ns: u64,
    adoption_base_mm: f64,
    adoption_speed_gain: f64,
    panel_width_mm: f64,
    panel_height_mm: f64,
}

impl ContactTracker {
    /// The kernel's 100 ms liftoff timeout; a WCH flap held for 250 ms, ended by 50 ms of
    /// silence; a 15 mm adoption base at 0.75 speed gain; and the JN-MD-IPST105FP's 221.9 mm by
    /// 147.9 mm active area.
    const DEFAULT_LIFT_TIMEOUT_NS: u64 = 100_000_000;
    const DEFAULT_DEBOUNCE_NS: u64 = 250_000_000;
    const DEFAULT_LIFT_SILENCE_NS: u64 = 50_000_000;
    const DEFAULT_ADOPTION_BASE_MM: f64 = 15.0;
    const DEFAULT_ADOPTION_SPEED_GAIN: f64 = 0.75;
    const DEFAULT_PANEL_WIDTH_MM: f64 = 221.9;
    const DEFAULT_PANEL_HEIGHT_MM: f64 = 147.9;

    pub fn new(
        lift_timeout_ns: u64,
        debounce_ns: u64,
        adoption_base_mm: f64,
        adoption_speed_gain: f64,
        panel_width_mm: f64,
        panel_height_mm: f64,
    ) -> Self {
        Self {
            active: BTreeMap::new(),
            pending: BTreeMap::new(),
            alias: BTreeMap::new(),
            last_frame_ns: 0,
            lift_timeout_ns,
            debounce_ns,
            lift_silence_ns: Self::DEFAULT_LIFT_SILENCE_NS,
            adoption_base_mm,
            adoption_speed_gain,
            panel_width_mm,
            panel_height_mm,
        }
    }

    /// A tracker with the default timeouts and adoption for a panel whose active area is
    /// `panel_width_mm` by `panel_height_mm`.
    pub fn for_panel(panel_width_mm: f64, panel_height_mm: f64) -> Self {
        Self::new(
            Self::DEFAULT_LIFT_TIMEOUT_NS,
            Self::DEFAULT_DEBOUNCE_NS,
            Self::DEFAULT_ADOPTION_BASE_MM,
            Self::DEFAULT_ADOPTION_SPEED_GAIN,
            panel_width_mm,
            panel_height_mm,
        )
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn update(&mut self, frame: &TouchFrame, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        self.last_frame_ns = at_ns;
        // A WCH report can carry the same contact id in two slots, one of them
        // stale (seen in the probe-session-1 replay: one frame with two slots
        // for id 3, one touched and one lifted). The touching instance is
        // authoritative; the last one in the report wins.
        let mut by_id: BTreeMap<u8, Contact> = BTreeMap::new();
        for contact in frame.contacts.iter().filter(|c| c.touching) {
            by_id.insert(contact.id, *contact);
        }
        let touching: SmallVec<[Contact; 10]> = by_id.values().copied().collect();
        let touching_ids: BTreeSet<u8> = by_id.into_keys().collect();

        let mut events = SmallVec::new();

        for contact in &touching {
            // Swift's `if let lift = pending[contact.id], ... {} else if`:
            // a same-id lift that misses the window or the radius still lets
            // the cross-id search run, so both are modelled here.
            let mut same_id_adopted = false;
            if let Some(&(press, lifted_at_ns)) = self.pending.get(&contact.id) {
                let gap_ns = at_ns.saturating_sub(lifted_at_ns);
                if gap_ns <= self.debounce_ns
                    && self.distance_mm(contact, &press) <= self.adoption_radius_mm(&press, gap_ns)
                {
                    // Same id back within the window and within a finger's
                    // travel: a tip flap. The press continues; the moved pass
                    // below reports where the finger went.
                    self.pending.remove(&contact.id);
                    self.active.insert(contact.id, press);
                    same_id_adopted = true;
                }
            }
            if !same_id_adopted
                && let Some((old_id, press)) = self.nearest_pending(contact, at_ns) {
                    // A different id near a held lift's finger: the same finger
                    // under a new number (id churn, or the mouse stream's
                    // contact 0 after a flap). The moved pass below reports the
                    // jump, under the id the gesture already knows.
                    self.pending.remove(&old_id);
                    self.alias.insert(contact.id, old_id);
                    self.active.insert(old_id, press);
                }
        }

        // An aliased contact keeps its press alive under the canonical id, this
        // frame and after.
        let live_ids: BTreeSet<u8> = touching
            .iter()
            .map(|c| self.alias.get(&c.id).copied().unwrap_or(c.id))
            .collect();

        for contact in frame.contacts.iter().filter(|c| !c.touching && !touching_ids.contains(&c.id)) {
            let key = self.alias.get(&contact.id).copied().unwrap_or(contact.id);
            let lifted = self.active.remove(&key).map(|press| (press, at_ns)).or_else(|| self.pending.remove(&key));
            if let Some((press, lifted_at_ns)) = lifted {
                // A lift report says the finger is gone; its coordinates are never the place of
                // anything. The JN-MD-IPST105FP sends the first one where the finger was and, a
                // frame or more later, the rest at the panel's origin: that one is final.
                if contact.x == 0.0 && contact.y == 0.0 {
                    self.alias.retain(|_, target| *target != key);
                    events.push(ContactEvent { id: key, phase: ContactPhase::Ended, x: press.x, y: press.y });
                } else {
                    self.pending.insert(key, (press, lifted_at_ns));
                }
            }
        }
        let vanished: SmallVec<[u8; 10]> = self
            .active
            .keys()
            .copied()
            .filter(|id| !live_ids.contains(id))
            .collect();
        for id in vanished {
            if let Some(press) = self.active.remove(&id) {
                self.pending.insert(id, (press, at_ns));
            }
        }
        self.active.retain(|id, _| live_ids.contains(id));

        self.flush_due(at_ns, &mut events);

        let mut began: SmallVec<[ContactEvent; 16]> = SmallVec::new();
        let mut moved: SmallVec<[ContactEvent; 16]> = SmallVec::new();
        for contact in &touching {
            let key = self.alias.get(&contact.id).copied().unwrap_or(contact.id);
            match self.active.get(&key).copied() {
                Some(previous) if previous.x != contact.x || previous.y != contact.y => {
                    moved.push(ContactEvent { id: key, phase: ContactPhase::Moved, x: contact.x, y: contact.y });
                }
                Some(_) => {}
                None => {
                    // The touch is genuinely new; any held lift for this id is
                    // over (the finger came back farther than a flap would put
                    // it), so believe it now.
                    if let Some((press, _)) = self.pending.remove(&key) {
                        events.push(ContactEvent { id: key, phase: ContactPhase::Ended, x: press.x, y: press.y });
                    }
                    began.push(ContactEvent { id: key, phase: ContactPhase::Began, x: contact.x, y: contact.y });
                }
            }
            let previous = self
                .active
                .get(&key)
                .copied()
                .unwrap_or(Press { x: contact.x, y: contact.y, last_at_ns: at_ns, speed: 0.0 });
            let next = self.advanced(previous, contact, at_ns);
            self.active.insert(key, next);
        }
        events.extend(began);
        events.extend(moved);
        events
    }

    /// A report arrived that carries no position the jump filter believes. The panel is not
    /// silent, so a lift report before it is not a lift yet.
    pub fn heard(&mut self, at_ns: u64) {
        self.last_frame_ns = at_ns;
    }

    /// The earliest moment the tracker ends a contact with no further input: a held lift's
    /// window or the silence after it, or a press that has gone quiet. `None` when only a new
    /// report can move it.
    pub fn next_deadline_ns(&self) -> Option<u64> {
        let lift = self
            .pending
            .values()
            .map(|(_, lifted_at_ns)| lifted_at_ns.saturating_add(self.debounce_ns))
            .min();
        // A press ends when the silence has lasted longer than the lift timeout, so the first moment
        // that is so is one nanosecond past it.
        let quiet_ns = if self.pending.is_empty() { self.lift_timeout_ns.saturating_add(1) } else { self.lift_silence_ns };
        let quiet = (!self.pending.is_empty() || !self.active.is_empty())
            .then(|| self.last_frame_ns.saturating_add(quiet_ns));
        lift.into_iter().chain(quiet).min()
    }

    /// Ends every press whose reports have gone silent for longer than the
    /// lift timeout, after flushing the lifts held back by the debounce.
    pub fn expire(&mut self, at_ns: u64) -> SmallVec<[ContactEvent; 16]> {
        let mut events = SmallVec::new();
        self.flush_due(at_ns, &mut events);
        if self.active.is_empty()
            || at_ns <= self.last_frame_ns
            || at_ns - self.last_frame_ns <= self.lift_timeout_ns
        {
            return events;
        }
        for (id, press) in &self.active {
            events.push(ContactEvent { id: *id, phase: ContactPhase::Ended, x: press.x, y: press.y });
        }
        self.active.clear();
        self.alias.clear();
        events
    }

    /// A held lift a returning contact under a different id could be: within
    /// the debounce window and within a velocity-scaled radius of where the
    /// finger was.
    fn nearest_pending(&self, contact: &Contact, at_ns: u64) -> Option<(u8, Press)> {
        let mut best: Option<(u8, Press)> = None;
        let mut best_mm = f64::INFINITY;
        for (&id, &(press, lifted_at_ns)) in &self.pending {
            if id == contact.id {
                continue;
            }
            let gap_ns = at_ns.saturating_sub(lifted_at_ns);
            if gap_ns > self.debounce_ns {
                continue;
            }
            let dist_mm = self.distance_mm(contact, &press);
            if dist_mm <= self.adoption_radius_mm(&press, gap_ns) && dist_mm < best_mm {
                best = Some((id, press));
                best_mm = dist_mm;
            }
        }
        best
    }

    /// How far a returning finger may have travelled over the gap: a base
    /// radius plus the press's speed over the gap.
    fn adoption_radius_mm(&self, press: &Press, gap_ns: u64) -> f64 {
        self.adoption_base_mm
            + press.speed * gap_ns as f64 / 1_000_000_000.0 * self.adoption_speed_gain
    }

    /// Emits the lifts whose hold window has passed or that the panel went silent after, and
    /// forgets their aliases.
    fn flush_due(&mut self, at_ns: u64, events: &mut SmallVec<[ContactEvent; 16]>) {
        let due: SmallVec<[u8; 16]> = self
            .pending
            .iter()
            .filter(|(_, (_, lifted_at_ns))| {
                at_ns.saturating_sub(*lifted_at_ns) >= self.debounce_ns
                    || at_ns.saturating_sub(self.last_frame_ns) >= self.lift_silence_ns
            })
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            if let Some((press, _)) = self.pending.remove(&id) {
                events.push(ContactEvent { id, phase: ContactPhase::Ended, x: press.x, y: press.y });
                self.alias.retain(|_, target| *target != id);
            }
        }
    }

    fn distance_mm(&self, contact: &Contact, press: &Press) -> f64 {
        ((contact.x - press.x) * self.panel_width_mm)
            .hypot((contact.y - press.y) * self.panel_height_mm)
    }

    fn advanced(&self, press: Press, contact: &Contact, at_ns: u64) -> Press {
        let mut next = press;
        let dt = at_ns.saturating_sub(press.last_at_ns) as f64 / 1_000_000_000.0;
        if dt >= 0.001 {
            let v = self.distance_mm(contact, &press) / dt;
            next.speed = if press.speed == 0.0 { v } else { press.speed * 0.7 + v * 0.3 };
        }
        next.x = contact.x;
        next.y = contact.y;
        next.last_at_ns = at_ns;
        next
    }
}

impl Default for ContactTracker {
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_LIFT_TIMEOUT_NS,
            Self::DEFAULT_DEBOUNCE_NS,
            Self::DEFAULT_ADOPTION_BASE_MM,
            Self::DEFAULT_ADOPTION_SPEED_GAIN,
            Self::DEFAULT_PANEL_WIDTH_MM,
            Self::DEFAULT_PANEL_HEIGHT_MM,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Source;

    const MS: u64 = 1_000_000;

    fn frame(contacts: &[(u8, bool, f64, f64)]) -> TouchFrame {
        TouchFrame {
            source: Source::Digitizer,
            contacts: contacts
                .iter()
                .map(|&(id, touching, x, y)| Contact { id, touching, x, y })
                .collect(),
            scan_time: Some(0),
        }
    }

    #[test]
    fn first_touch_begins() {
        let mut tracker = ContactTracker::default();
        let events = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert_eq!(events.as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Began, x: 0.2, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn movement_of_an_active_contact_is_reported() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert_eq!(
            tracker.update(&frame(&[(1, true, 0.25, 0.3)]), 8 * MS).as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Moved, x: 0.25, y: 0.3 }]);
    }

    #[test]
    fn unchanged_position_reports_nothing() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert!(tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 8 * MS).is_empty());
    }

    // The JN-MD-IPST105FP sends about one lift report in fourteen with the
    // coordinates (0, 0), and a tap is placed where the ended event says. A
    // lift report's coordinates are never used.
    #[test]
    fn tip_off_ends_the_contact_where_the_finger_last_was() {
        let mut tracker = ContactTracker::new(100_000_000, 0, 15.0, 0.75, 221.9, 147.9);
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert_eq!(tracker.update(&frame(&[(1, false, 0.0, 0.0)]), 8 * MS).as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn contact_missing_from_frame_ends_at_its_last_position() {
        let mut tracker = ContactTracker::new(100_000_000, 0, 15.0, 0.75, 221.9, 147.9);
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3), (2, true, 0.6, 0.6)]), 0);
        let events = tracker.update(&frame(&[(2, true, 0.6, 0.6)]), 8 * MS);
        assert_eq!(
            events.as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn lifted_contact_that_was_never_active_is_ignored() {
        let mut tracker = ContactTracker::default();
        assert!(tracker.update(&frame(&[(4, false, 0.5, 0.5)]), 0).is_empty());
    }

    #[test]
    fn silence_longer_than_the_lift_timeout_ends_everything() {
        let mut tracker = ContactTracker::new(100 * MS, 60_000_000, 15.0, 0.75, 221.9, 147.9);
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert!(tracker.expire(50 * MS).is_empty());
        assert_eq!(
            tracker.expire(150 * MS).as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn events_are_ordered_ended_then_began_then_moved() {
        let mut tracker = ContactTracker::new(100_000_000, 0, 15.0, 0.75, 221.9, 147.9);
        let _ = tracker.update(&frame(&[(1, true, 0.1, 0.1), (2, true, 0.5, 0.5)]), 0);
        let events = tracker.update(&frame(&[(2, true, 0.55, 0.5), (3, true, 0.9, 0.9)]), 8 * MS);
        let phases: Vec<ContactPhase> = events.iter().map(|e| e.phase).collect();
        let ids: Vec<u8> = events.iter().map(|e| e.id).collect();
        assert_eq!(phases, vec![ContactPhase::Ended, ContactPhase::Began, ContactPhase::Moved]);
        assert_eq!(ids, vec![1, 3, 2]);
    }

    // Owner requirement (2026-10-09): able to watch up to 10 fingers (future
    // painting apps / games).
    #[test]
    fn tracks_ten_simultaneous_fingers_through_their_whole_lifecycle() {
        let mut tracker = ContactTracker::new(100_000_000, 0, 15.0, 0.75, 221.9, 147.9);
        let down: SmallVec<[(u8, bool, f64, f64); 10]> = (0..10u8)
            .map(|i| (i, true, 0.05 + f64::from(i) * 0.09, 0.5))
            .collect();
        let began = tracker.update(&frame(&down), 0);
        assert_eq!(began.iter().filter(|e| e.phase == ContactPhase::Began).count(), 10);
        assert_eq!(tracker.active_count(), 10);

        let moved_targets: SmallVec<[(u8, bool, f64, f64); 10]> = down
            .iter()
            .map(|&(id, _, _, _)| (id, true, 0.2, 0.6))
            .collect();
        let moved = tracker.update(&frame(&moved_targets), 8 * MS);
        assert_eq!(moved.iter().filter(|e| e.phase == ContactPhase::Moved).count(), 10);

        let lifted_targets: SmallVec<[(u8, bool, f64, f64); 10]> = down
            .iter()
            .map(|&(id, _, _, _)| (id, false, 0.2, 0.6))
            .collect();
        let lifted = tracker.update(&frame(&lifted_targets), 16 * MS);
        assert_eq!(lifted.iter().filter(|e| e.phase == ContactPhase::Ended).count(), 10);
        assert_eq!(tracker.active_count(), 0);
    }

    // MARK: lift debounce (WCH contacts flap during a press or lift)

    #[test]
    fn a_tip_flap_continues_the_contact_instead_of_ending_and_beginning() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        // Tip off, back on 35 ms later at a drifted position: one contact, a
        // move, no ghost tap.
        let _ = tracker.update(&frame(&[(1, false, 0.21, 0.3)]), 10 * MS);
        let events = tracker.update(&frame(&[(1, true, 0.22, 0.3)]), 45 * MS);
        assert_eq!(
            events.as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Moved, x: 0.22, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 1);
    }

    // The panel falls silent after a lift and keeps reporting through a flap (measured
    // 2026-11-24 and 2026-11-25). It skips a frame now and then, 38 ms between reports at the
    // most, so 50 ms without one is the lift.
    #[test]
    fn a_lift_report_followed_by_silence_is_a_lift_in_50_ms() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert!(tracker.update(&frame(&[(1, false, 0.21, 0.3)]), 10 * MS).is_empty());
        assert!(tracker.expire(59 * MS).is_empty());
        assert_eq!(
            tracker.expire(60 * MS).as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 0);
    }

    // The other way round: as long as lift reports keep coming the finger may still be down.
    // They stop being believed at the debounce, 250 ms (measured: a finger whose tip came back
    // 194 ms after the lift report, the panel having misread it twice in between).
    #[test]
    fn a_run_of_lift_reports_is_a_lift_at_the_debounce() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert!(tracker.update(&frame(&[(1, false, 0.21, 0.3)]), 10 * MS).is_empty());
        for at in [50, 130, 210, 249] {
            assert!(tracker.update(&frame(&[(1, false, 0.21, 0.3)]), at * MS).is_empty(), "{at} ms");
        }
        assert_eq!(
            tracker.update(&frame(&[(1, false, 0.21, 0.3)]), 260 * MS).as_slice(),
            [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]
        );
        assert_eq!(tracker.active_count(), 0);
    }

    // A finger misread on its way back (probe 2026-11-25 6.458 s): the lift report, two frames
    // somewhere else that the jump filter drops, another lift report, and then the finger,
    // which the filter believes on its sixth frame. One contact, 194 ms after the lift report.
    #[test]
    fn a_finger_that_takes_a_moment_to_come_back_is_still_the_same_contact() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.200, 0.3)]), 0);
        let _ = tracker.update(&frame(&[(1, true, 0.220, 0.3)]), 17 * MS);
        assert!(tracker.update(&frame(&[(1, false, 0.220, 0.3)]), 34 * MS).is_empty());
        for at in [71, 88] {
            tracker.heard(at * MS);
            assert!(tracker.expire((at + 49) * MS).is_empty(), "{at} ms");
        }
        assert!(tracker.update(&frame(&[(1, false, 0.220, 0.3)]), 104 * MS).is_empty());
        for at in [141, 158, 175, 192] {
            tracker.heard(at * MS);
        }
        assert_eq!(
            tracker.update(&frame(&[(1, true, 0.224, 0.3)]), 228 * MS).as_slice(),
            [ContactEvent { id: 1, phase: ContactPhase::Moved, x: 0.224, y: 0.3 }]
        );
        assert_eq!(tracker.active_count(), 1);
    }

    // Once the finger has left, the JN-MD-IPST105FP reports the tip off at (0, 0). That report
    // is the lift, whatever the debounce: nothing else sends it.
    #[test]
    fn a_lift_report_at_the_panel_s_origin_is_the_lift_at_once() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert!(tracker.update(&frame(&[(1, false, 0.2, 0.3)]), 10 * MS).is_empty());
        assert_eq!(
            tracker.update(&frame(&[(1, false, 0.0, 0.0)]), 27 * MS).as_slice(),
            [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]
        );
        assert_eq!(tracker.active_count(), 0);
        assert!(tracker.update(&frame(&[(1, false, 0.0, 0.0)]), 44 * MS).is_empty());
    }

    // Frame times come from the panel's reports and the expiry from the pipeline's own clock.
    // Where the two cross, a held lift is not overdue before its silence has lasted.
    #[test]
    fn a_clock_behind_the_frame_time_does_not_flush_a_held_lift() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 100 * MS);
        assert!(tracker.update(&frame(&[(1, false, 0.2, 0.3)]), 110 * MS).is_empty());
        assert!(tracker.expire(109 * MS).is_empty());
        assert!(tracker.expire(159 * MS).is_empty());
        assert_eq!(
            tracker.expire(160 * MS).as_slice(),
            [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]
        );
        assert_eq!(tracker.active_count(), 0);
    }

    // A flap of eight frames (120 ms) during a press: the panel says the tip is off all along,
    // then the finger is back where it was. One contact.
    #[test]
    fn a_flap_of_eight_frames_does_not_end_the_press() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        let _ = tracker.update(&frame(&[(1, true, 0.22, 0.3)]), 17 * MS);
        for at in [34, 51, 67, 84, 100, 117, 134, 150] {
            assert!(tracker.update(&frame(&[(1, false, 0.22, 0.3)]), at * MS).is_empty(), "{at} ms");
        }
        assert_eq!(
            tracker.update(&frame(&[(1, true, 0.23, 0.3)]), 167 * MS).as_slice(),
            [ContactEvent { id: 1, phase: ContactPhase::Moved, x: 0.23, y: 0.3 }]
        );
        assert_eq!(tracker.active_count(), 1);
    }

    // A tip-on report the jump filter dropped is still the panel talking: the lift report before
    // it is not a lift until the panel has gone quiet after it.
    #[test]
    fn a_dropped_report_keeps_the_panel_from_counting_as_silent() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        assert!(tracker.update(&frame(&[(1, false, 0.2, 0.3)]), 10 * MS).is_empty());
        tracker.heard(30 * MS);
        assert!(tracker.expire(79 * MS).is_empty());
        tracker.heard(90 * MS);
        assert!(tracker.expire(139 * MS).is_empty());
        assert_eq!(
            tracker.expire(140 * MS).as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]
        );
    }

    // So that the pipeline's timer wakes for exactly these moments, and what it then finds due
    // has come due: it sleeps until this time and runs nothing else first.
    #[test]
    fn the_next_deadline_is_when_a_quiet_lift_or_press_ends() {
        let mut tracker = ContactTracker::default();
        assert_eq!(tracker.next_deadline_ns(), None);
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 20 * MS);
        let timeout = tracker.next_deadline_ns().unwrap();
        assert!(tracker.expire(timeout - 1).is_empty());
        assert_eq!(
            tracker.expire(timeout).as_slice(),
            [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.2, y: 0.3 }]
        );
        assert_eq!(tracker.next_deadline_ns(), None);

        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 200 * MS);
        let _ = tracker.update(&frame(&[(1, false, 0.2, 0.3)]), 210 * MS);
        assert_eq!(tracker.next_deadline_ns(), Some(260 * MS)); // the silence after the lift report
        tracker.heard(230 * MS);
        assert_eq!(tracker.next_deadline_ns(), Some(280 * MS));
        let _ = tracker.update(&frame(&[(1, false, 0.0, 0.0)]), 240 * MS);
        assert_eq!(tracker.next_deadline_ns(), None);

        // A lift report every 40 ms keeps the silence from lasting: the debounce is the deadline.
        let mut tracker = ContactTracker::new(100 * MS, 150 * MS, 15.0, 0.75, 221.9, 147.9);
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        for at in [10, 50, 90, 130] {
            let _ = tracker.update(&frame(&[(1, false, 0.2, 0.3)]), at * MS);
        }
        assert_eq!(tracker.next_deadline_ns(), Some(160 * MS));
    }

    // MARK: dropouts and id churn

    /// A long dropout is not the tracker's to bridge: holding it would delay
    /// every real lift of a moving finger. The contact ends on schedule.
    #[test]
    fn a_moving_press_dropout_ends_on_schedule() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.20, 0.3)]), 0);
        let _ = tracker.update(&frame(&[(1, true, 0.22, 0.3)]), 17 * MS);
        let _ = tracker.update(&frame(&[]), 34 * MS);
        let events = tracker.update(&frame(&[]), 634 * MS);
        assert_eq!(
            events.as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Ended, x: 0.22, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 0);
    }

    /// The controller renumbers the finger mid-press (frame-rate churn): the
    /// press continues under the id the gesture already knows.
    #[test]
    fn a_renumbered_finger_continues_the_same_contact() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.20, 0.3)]), 0);
        let _ = tracker.update(&frame(&[(1, true, 0.22, 0.3)]), 17 * MS);
        let _ = tracker.update(&frame(&[]), 34 * MS);
        let adopted = tracker.update(&frame(&[(7, true, 0.24, 0.3)]), 45 * MS); // 11 ms later, renumbered
        assert_eq!(
            adopted.as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Moved, x: 0.24, y: 0.3 }]);
        let next = tracker.update(&frame(&[(7, true, 0.26, 0.3)]), 62 * MS);
        assert_eq!(next.as_slice(), [ContactEvent { id: 1, phase: ContactPhase::Moved, x: 0.26, y: 0.3 }]);
        assert_eq!(tracker.active_count(), 1);
    }

    /// A still press that lifts and is pressed again must be two contacts:
    /// that is a double-tap, and no stack bridges a clean lift. (Before the
    /// debounce runs out the pipeline's timer has ended it on the panel's
    /// silence, 50 ms; here no timer runs, so it is the debounce that does.)
    #[test]
    fn a_still_press_dropout_is_not_bridged() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.2, 0.3)]), 0);
        let _ = tracker.update(&frame(&[]), 17 * MS);
        let events = tracker.update(&frame(&[(1, true, 0.201, 0.3)]), 350 * MS);
        let phases: Vec<ContactPhase> = events.iter().map(|e| e.phase).collect();
        let ids: Vec<u8> = events.iter().map(|e| e.id).collect();
        assert_eq!(phases, vec![ContactPhase::Ended, ContactPhase::Began]);
        assert_eq!(ids, vec![1, 1]);
    }

    /// A resume far outside the adoption radius is a different touch, not the
    /// drag.
    #[test]
    fn a_far_resume_is_not_adopted() {
        let mut tracker = ContactTracker::default();
        let _ = tracker.update(&frame(&[(1, true, 0.20, 0.3)]), 0);
        let _ = tracker.update(&frame(&[(1, true, 0.22, 0.3)]), 17 * MS);
        let _ = tracker.update(&frame(&[]), 34 * MS);
        let events = tracker.update(&frame(&[(1, true, 0.90, 0.3)]), 45 * MS);
        let phases: Vec<ContactPhase> = events.iter().map(|e| e.phase).collect();
        assert_eq!(phases, vec![ContactPhase::Ended, ContactPhase::Began]);
    }
}
