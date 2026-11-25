//! After a Space switch on the touch display, macOS gives focus to that
//! display. When focus was on another display, the app that had it gets it
//! back: the first time focus moves away within 1.5 s of the swipe.

/// What the keeper wants done at this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep watching; nothing has changed yet.
    Wait,
    /// Re-activate the app with this pid, which the swipe stole focus from.
    Restore(i32),
    /// The 1.5 s window closed with focus still on the touched app; stop polling.
    Stop,
}

/// Watches focus for one app for the 1.5 s after a Space switch (owner report
/// 2026-10-11: the switch took focus away from the MacBook window).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusKeeper {
    /// The pid whose focus is being watched.
    pub pid: i32,
    started_ns: u64,
}

impl FocusKeeper {
    /// How long after the swipe a focus change is still restored.
    const WINDOW_NS: u64 = 1_500_000_000;

    pub fn new(pid: i32, started_ns: u64) -> Self {
        Self { pid, started_ns }
    }

    /// The caller polls (its own cadence); `frontmost` is the pid of the
    /// frontmost app, or `None` when macOS reports nothing frontmost.
    pub fn poll(&self, frontmost: Option<i32>, now_ns: u64) -> Action {
        if let Some(frontmost) = frontmost
            && frontmost != self.pid {
                return Action::Restore(self.pid);
            }
        if now_ns.wrapping_sub(self.started_ns) >= Self::WINDOW_NS {
            Action::Stop
        } else {
            Action::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    /// Owner report 2026-10-11: a Space switch on the touch display took focus
    /// away from the MacBook window.
    #[test]
    fn when_the_switch_moves_focus_away_it_is_given_back() {
        let keeper = FocusKeeper::new(501, 0);
        assert_eq!(keeper.poll(Some(501), 100 * MS), Action::Wait);
        assert_eq!(keeper.poll(Some(777), 400 * MS), Action::Restore(501));
    }

    #[test]
    fn if_focus_never_moves_the_keeper_stops_after_one_and_a_half_seconds() {
        let keeper = FocusKeeper::new(501, 0);
        assert_eq!(keeper.poll(Some(501), 1_400 * MS), Action::Wait);
        assert_eq!(keeper.poll(Some(501), 1_500 * MS), Action::Stop);
    }

    #[test]
    fn an_unknown_frontmost_app_is_not_a_change() {
        let keeper = FocusKeeper::new(501, 0);
        assert_eq!(keeper.poll(None, 200 * MS), Action::Wait);
    }
}
