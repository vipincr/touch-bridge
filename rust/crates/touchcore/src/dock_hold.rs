//! Holds a dock swipe's first steps back while something is prepared (the Dock
//! brought to the touch display before Launchpad opens), then releases them as
//! one `began` step.

use smallvec::{smallvec, SmallVec};

use crate::gestures::GesturePhase;

/// One scroll step of a dock swipe, as the platform posts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub delta: f64,
    pub phase: GesturePhase,
}

/// The hold between a dock swipe's steps and the system while Launchpad is prepared.
#[derive(Debug, Default)]
pub struct DockSwipeHold {
    is_holding: bool,
    held: f64,
    began: bool,
    ended: bool,
}

impl DockSwipeHold {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether steps are currently being held back.
    pub fn is_holding(&self) -> bool {
        self.is_holding
    }

    /// Opens a hold. Any hold in progress is restarted.
    pub fn start(&mut self) {
        self.is_holding = true;
        self.held = 0.0;
        self.began = false;
        self.ended = false;
    }

    /// While holding, keeps the step; otherwise returns it to be posted now.
    pub fn pass(&mut self, delta: f64, phase: GesturePhase) -> SmallVec<[Step; 2]> {
        if !self.is_holding {
            return smallvec![Step { delta, phase }];
        }
        match phase {
            GesturePhase::Began => {
                self.began = true;
                self.held = delta;
            }
            GesturePhase::Changed => self.held += delta,
            GesturePhase::Ended => self.ended = true,
        }
        SmallVec::new()
    }

    /// Ends the hold: the held steps merged into one `began`, plus `ended` if
    /// the fingers already lifted.
    pub fn release(&mut self) -> SmallVec<[Step; 2]> {
        if !self.is_holding {
            return SmallVec::new();
        }
        self.is_holding = false;
        if !self.began {
            return SmallVec::new();
        }
        let began = Step { delta: self.held, phase: GesturePhase::Began };
        if self.ended {
            smallvec![began, Step { delta: 0.0, phase: GesturePhase::Ended }]
        } else {
            smallvec![began]
        }
    }

    /// Drops a held swipe. True when nothing had reached the system yet (so
    /// there is nothing to cancel).
    pub fn cancel(&mut self) -> bool {
        if !self.is_holding {
            return false;
        }
        self.is_holding = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_hold_steps_pass_straight_through() {
        let mut hold = DockSwipeHold::new();
        let passed = hold.pass(0.2, GesturePhase::Began);
        assert_eq!(passed.iter().map(|s| s.delta).collect::<Vec<_>>(), [0.2]);
        let passed = hold.pass(0.1, GesturePhase::Changed);
        assert_eq!(passed.iter().map(|s| s.phase).collect::<Vec<_>>(), [GesturePhase::Changed]);
    }

    #[test]
    fn held_steps_are_released_as_one_began_with_their_sum() {
        let mut hold = DockSwipeHold::new();
        hold.start();
        assert!(hold.pass(0.2, GesturePhase::Began).is_empty());
        assert!(hold.pass(0.1, GesturePhase::Changed).is_empty());
        assert!(hold.pass(0.05, GesturePhase::Changed).is_empty());
        let released = hold.release();
        assert_eq!(released.len(), 1);
        assert_eq!(released.first().map(|s| s.phase), Some(GesturePhase::Began));
        assert!((released.first().map_or(0.0, |s| s.delta) - 0.35).abs() < 1e-9);
        let passed = hold.pass(0.1, GesturePhase::Changed);
        assert_eq!(passed.iter().map(|s| s.delta).collect::<Vec<_>>(), [0.1], "after the release, steps flow again");
    }

    #[test]
    fn fingers_lifted_during_the_hold_release_began_then_ended() {
        let mut hold = DockSwipeHold::new();
        hold.start();
        hold.pass(0.6, GesturePhase::Began);
        hold.pass(0.0, GesturePhase::Ended);
        let released = hold.release();
        assert_eq!(released.iter().map(|s| s.phase).collect::<Vec<_>>(), [GesturePhase::Began, GesturePhase::Ended]);
    }

    #[test]
    fn a_cancel_during_the_hold_drops_everything() {
        let mut hold = DockSwipeHold::new();
        hold.start();
        hold.pass(0.3, GesturePhase::Began);
        assert!(hold.cancel(), "nothing reached the system, so nothing needs cancelling");
        assert!(hold.release().is_empty());
        assert!(!hold.cancel());
    }
}
