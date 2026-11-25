//! Coordinate repair for the single-contact stream a WCH panel sends while in
//! mouse emulation, the only touch data available until the digitizer-mode
//! switch takes hold.

use smallvec::{SmallVec, smallvec};

use crate::report::{Contact, Source, TouchFrame};

/// Coordinate repair for the single-contact stream a WCH panel sends while in
/// mouse emulation, the only touch data available until the digitizer-mode
/// switch takes hold. Measured on the JN-MD-IPST105FP (probe 2026-11-24), a
/// slow drag contains single-frame jumps of up to 129 mm while the tip stays
/// down (p95: 21 mm): the contact centroid glitching, not the finger. Trusted,
/// each one teleports the drag anchor, which is the "tabs dancing while my
/// finger is still" symptom.
///
/// The policy is libinput's touchpad jump detector (tp_detect_jumps), because
/// that is the maintained, shipped implementation of exactly this problem for
/// exactly this feel: normalize the frame's motion to a 12 ms reference
/// interval; a jump is travel over `jump_max_mm` (20 mm) or growth over
/// `jump_growth_mm` (7 mm) versus the previous frame; a jump is discarded, the
/// held position is emitted instead, and the reference stays put, so a
/// wandering centroid reports nothing. Samples staler than `jump_stale_ns`
/// (30 ms) bypass the test (irregular frames are not evidence); after
/// `jump_accept_cap` consecutive discards the position is accepted anyway (the
/// contact genuinely relocated, a two-finger handover, rather than glitching).
///
/// Identity is not this type's business: lifts, flaps and dropout bridging live
/// in [`crate::tracker::ContactTracker`]. Digitizer frames pass through
/// untouched and reset the repair state.
#[derive(Debug)]
pub struct SingleTouchFilter {
    /// Reference frame length the jump thresholds are normalized to.
    pub jump_reference_ns: u64,
    /// Normalized travel beyond this is a jump (libinput: 20 mm).
    pub jump_max_mm: f64,
    /// Normalized growth beyond this over the previous frame is a jump
    /// (libinput: 7 mm).
    pub jump_growth_mm: f64,
    /// Samples staler than this skip the jump test (libinput: 30 ms).
    pub jump_stale_ns: u64,
    /// Consecutive discards before a new position is accepted as real.
    pub jump_accept_cap: usize,

    last: Option<(f64, f64, u64, f64)>,
    discards: usize,
}

impl SingleTouchFilter {
    pub fn new(
        jump_reference_ns: u64,
        jump_max_mm: f64,
        jump_growth_mm: f64,
        jump_stale_ns: u64,
        jump_accept_cap: usize,
    ) -> Self {
        Self {
            jump_reference_ns,
            jump_max_mm,
            jump_growth_mm,
            jump_stale_ns,
            jump_accept_cap,
            last: None,
            discards: 0,
        }
    }

    /// The frames to feed the tracker instead of `frame`. A discarded jump
    /// contributes the held position; everything else passes as reported.
    pub fn filter(
        &mut self,
        frame: TouchFrame,
        at_ns: u64,
        panel_width_mm: f64,
        panel_height_mm: f64,
    ) -> SmallVec<[TouchFrame; 2]> {
        if frame.source != Source::Mouse {
            self.last = None;
            self.discards = 0;
            return smallvec![frame];
        }
        let contact = match frame.contacts.first().copied() {
            Some(contact) if contact.touching => contact,
            _ => {
                self.last = None;
                self.discards = 0;
                return smallvec![frame];
            }
        };
        let previous = match self.last {
            Some(previous) => previous,
            None => {
                self.last = Some((contact.x, contact.y, at_ns, 0.0));
                self.discards = 0;
                return smallvec![frame];
            }
        };

        let dt = at_ns - previous.2;
        if dt > self.jump_stale_ns || dt == 0 {
            // Irregular frame (after a dropout, or the same timestamp): not
            // jump evidence.
            self.last = Some((contact.x, contact.y, at_ns, 0.0));
            self.discards = 0;
            return smallvec![frame];
        }
        let dist_mm =
            ((contact.x - previous.0) * panel_width_mm).hypot((contact.y - previous.1) * panel_height_mm);
        let normalized = dist_mm * self.jump_reference_ns as f64 / dt as f64;
        if normalized <= self.jump_max_mm && (normalized - previous.3).abs() <= self.jump_growth_mm {
            self.last = Some((contact.x, contact.y, at_ns, normalized));
            self.discards = 0;
            return smallvec![frame];
        }

        // A jump. Discard it, unless it has persisted, when it is a real
        // relocation. The reference's clock advances even while held, so a run
        // of discards is not mistaken for one stale sample that would bypass
        // the test.
        self.discards += 1;
        if self.discards >= self.jump_accept_cap {
            self.last = Some((contact.x, contact.y, at_ns, 0.0));
            self.discards = 0;
            return smallvec![frame];
        }
        self.last = Some((previous.0, previous.1, at_ns, 0.0));
        smallvec![TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact { id: contact.id, touching: true, x: previous.0, y: previous.1 }],
            scan_time: frame.scan_time,
        }]
    }
}

impl Default for SingleTouchFilter {
    /// libinput's thresholds on the 12 ms reference the probe's frames are
    /// normalized to.
    fn default() -> Self {
        Self::new(12_000_000, 20.0, 7.0, 30_000_000, 6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gestures::{GestureAction, GesturePhase, GestureRecognizer, PanelPoint};
    use crate::tracker::{ContactEvent, ContactPhase};

    const WIDTH: f64 = 221.9;
    const HEIGHT: f64 = 147.9;
    const MS: u64 = 1_000_000;

    fn mouse_frame(x: f64, y: f64, touching: bool) -> TouchFrame {
        TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact { id: 0, touching, x, y }],
            scan_time: None,
        }
    }

    #[test]
    fn small_mouse_movement_passes_through() {
        let mut filter = SingleTouchFilter::default();
        assert_eq!(
            filter.filter(mouse_frame(0.5, 0.5, true), 0, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.5, 0.5, true)]
        );
        assert_eq!(
            filter.filter(mouse_frame(0.52, 0.5, true), 17 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.52, 0.5, true)]
        );
    }

    #[test]
    fn a_slow_press_glitch_is_discarded_and_the_position_held() {
        // Probe 2026-11-24: single-frame jumps up to 129 mm while the tip stays
        // down. libinput's jump rule: 20 mm per 12 ms normalized (7 mm growth)
        // is not finger motion, so discard it.
        let mut filter = SingleTouchFilter::default();
        let _ = filter.filter(mouse_frame(0.5, 0.5, true), 0, WIDTH, HEIGHT);
        let _ = filter.filter(mouse_frame(0.51, 0.5, true), 17 * MS, WIDTH, HEIGHT);
        let frames = filter.filter(mouse_frame(0.9, 0.5, true), 34 * MS, WIDTH, HEIGHT);
        assert_eq!(frames.to_vec(), vec![mouse_frame(0.51, 0.5, true)]);
        // A report 2 mm from the held point is real finger motion again, and
        // passes.
        assert_eq!(
            filter.filter(mouse_frame(0.5, 0.5, true), 51 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.5, 0.5, true)]
        );
    }

    #[test]
    fn a_fast_flick_passes() {
        // 0.04 x-units per 17 ms is about 520 mm/s: under the 20 mm/12 ms
        // normalized ceiling.
        let mut filter = SingleTouchFilter::default();
        let _ = filter.filter(mouse_frame(0.50, 0.5, true), 0, WIDTH, HEIGHT);
        assert_eq!(
            filter.filter(mouse_frame(0.54, 0.5, true), 17 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.54, 0.5, true)]
        );
        assert_eq!(
            filter.filter(mouse_frame(0.58, 0.5, true), 34 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.58, 0.5, true)]
        );
    }

    #[test]
    fn a_persistent_relocation_is_accepted_after_the_cap() {
        let mut filter = SingleTouchFilter { jump_accept_cap: 3, ..SingleTouchFilter::default() };
        let _ = filter.filter(mouse_frame(0.1, 0.1, true), 0, WIDTH, HEIGHT);
        for t in 1..=2u64 {
            assert_eq!(
                filter.filter(mouse_frame(0.8, 0.7, true), t * 17 * MS, WIDTH, HEIGHT).to_vec(),
                vec![mouse_frame(0.1, 0.1, true)]
            );
        }
        assert_eq!(
            filter.filter(mouse_frame(0.8, 0.7, true), 3 * 17 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.8, 0.7, true)]
        );
    }

    #[test]
    fn a_stale_sample_bypasses_the_jump_test() {
        // libinput: frames staler than 30 ms are not jump evidence (the finger
        // may be anywhere).
        let mut filter = SingleTouchFilter::default();
        let _ = filter.filter(mouse_frame(0.1, 0.1, true), 0, WIDTH, HEIGHT);
        assert_eq!(
            filter.filter(mouse_frame(0.9, 0.9, true), 200 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.9, 0.9, true)]
        );
    }

    #[test]
    fn lift_and_digitizer_frames_reset_the_repair_state() {
        let mut filter = SingleTouchFilter::default();
        let _ = filter.filter(mouse_frame(0.1, 0.1, true), 0, WIDTH, HEIGHT);
        let _ = filter.filter(mouse_frame(0.1, 0.1, false), 17 * MS, WIDTH, HEIGHT);
        // A new press starts from its own position, not held against the old
        // one.
        assert_eq!(
            filter.filter(mouse_frame(0.8, 0.7, true), 100 * MS, WIDTH, HEIGHT).to_vec(),
            vec![mouse_frame(0.8, 0.7, true)]
        );

        let digitizer = TouchFrame {
            source: Source::Digitizer,
            contacts: smallvec![
                Contact { id: 1, touching: true, x: 0.1, y: 0.1 },
                Contact { id: 2, touching: true, x: 0.9, y: 0.9 },
            ],
            scan_time: Some(5),
        };
        assert_eq!(
            filter.filter(digitizer.clone(), 120 * MS, WIDTH, HEIGHT).to_vec(),
            vec![digitizer]
        );
    }

    #[test]
    fn a_linger_before_moving_is_a_hold_drag_and_a_slow_swipe_scrolls() {
        // 100 ms of drift past 1 mm, then clear travel: scrolls (previously it
        // selected text, because the drag was classified by touch age instead
        // of by stillness).
        let mut recognizer = GestureRecognizer::default();
        let _ = recognizer.handle(&[ContactEvent { id: 0, phase: ContactPhase::Began, x: 0.5, y: 0.5 }], 0);
        let _ = recognizer.handle(&[ContactEvent { id: 0, phase: ContactPhase::Moved, x: 0.508, y: 0.5 }], 100_000_000);
        let actions = recognizer.handle(&[ContactEvent { id: 0, phase: ContactPhase::Moved, x: 0.59375, y: 0.5 }], 600_000_000);
        assert_eq!(
            actions.first(),
            Some(&GestureAction::Scroll { dx: 0.09375, dy: 0.0, phase: GesturePhase::Began })
        );

        // A finger still for 700 ms and only then moving: click-and-drag.
        let mut planted = GestureRecognizer::default();
        let _ = planted.handle(&[ContactEvent { id: 0, phase: ContactPhase::Began, x: 0.5, y: 0.5 }], 0);
        let hold_actions = planted.handle(&[ContactEvent { id: 0, phase: ContactPhase::Moved, x: 0.65, y: 0.5 }], 700_000_000);
        assert_eq!(
            hold_actions.first(),
            Some(&GestureAction::DragBegan { at: PanelPoint { x: 0.5, y: 0.5 }, hold: true })
        );
    }
}
