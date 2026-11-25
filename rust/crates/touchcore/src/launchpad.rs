//! Launchpad opened from the touch display: what to do about the Dock, where
//! the Dock's home display is, and when "then back" pushes it back.

use std::str::FromStr;

use crate::mapper::{ScreenPoint, ScreenRect};

/// What to do about the Dock when Launchpad is opened from the touch display.
/// Launchpad opens on the display that has the Dock (`--system-ui-selftest`,
/// 2026-10-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaunchpadDockMode {
    /// Bring the Dock to the touch display first; it stays there, as if moved there by hand.
    BringDock,
    /// Bring the Dock over, then push it back once Launchpad has closed (the
    /// default: the trackpad keeps opening Launchpad on the other display).
    BringDockAndReturn,
    /// Leave the Dock alone; Launchpad opens wherever the Dock is.
    Leave,
}

impl LaunchpadDockMode {
    /// The Swift raw value, under which settings store the mode.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::BringDock => "bringDock",
            Self::BringDockAndReturn => "bringDockAndReturn",
            Self::Leave => "leave",
        }
    }

    /// Every mode, in Swift's `CaseIterable` order.
    pub const ALL: [Self; 3] = [Self::BringDock, Self::BringDockAndReturn, Self::Leave];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLaunchpadDockModeError;

impl FromStr for LaunchpadDockMode {
    type Err = ParseLaunchpadDockModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == s).ok_or(ParseLaunchpadDockModeError)
    }
}

/// Launchpad-Dock planning.
pub struct LaunchpadDock;

impl LaunchpadDock {
    /// Which Dock moves a Launchpad pinch needs.
    ///
    /// Pinching in opens Launchpad; spreading shows the desktop, which covers
    /// every display anyway.
    pub fn plan(mode: LaunchpadDockMode, pinch_inward: bool, dock_on_touch_display: bool) -> Plan {
        if !pinch_inward {
            return Plan::new(false, false);
        }
        match mode {
            LaunchpadDockMode::BringDock => Plan::new(!dock_on_touch_display, false),
            LaunchpadDockMode::BringDockAndReturn => Plan::new(!dock_on_touch_display, true),
            LaunchpadDockMode::Leave => Plan::new(false, false),
        }
    }

    /// Where "then back" returns the Dock: the display it was on, or, when it
    /// was already on the touch display, the main display (or any other one if
    /// the touch display is the main one).
    pub fn home(dock: ScreenPoint, touch: ScreenRect, displays: &[ScreenRect], main: Option<ScreenRect>) -> Option<ScreenRect> {
        if let Some(current) = displays.iter().copied().find(|d| *d != touch && d.contains(dock)) {
            return Some(current);
        }
        if let Some(main) = main {
            if main != touch {
                return Some(main);
            }
        }
        displays.iter().copied().find(|d| *d != touch)
    }
}

/// The Dock moves a Launchpad pinch calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// Push the Dock to the touch display before Launchpad opens.
    pub bring_dock: bool,
    /// Push it home once Launchpad has closed: with "then back" the Dock lives
    /// off the touch display, wherever it was found (owner report 2026-10-11:
    /// one left there earlier never went back).
    pub return_dock: bool,
}

impl Plan {
    pub fn new(bring_dock: bool, return_dock: bool) -> Self {
        Self { bring_dock, return_dock }
    }
}

/// "Bring the Dock over, then back": polled about every 250 ms after the Dock
/// was brought over, it says when to push the Dock back. That is once Launchpad
/// has closed (two closed polls in a row, so a flicker doesn't count) or never
/// opened within 2 s, and only while no finger is on the panel (the push moves
/// the cursor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockReturnWatch {
    started_ns: u64,
    saw_open: bool,
    closed_polls: u32,
}

/// What the watch wants done at this poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep watching.
    Wait,
    /// Push the Dock back to its home display now.
    PushBack,
    /// Ten minutes of watching changed nothing; give up.
    Stop,
}

impl DockReturnWatch {
    const NOT_OPENED_TIMEOUT_NS: u64 = 2_000_000_000;
    const CLOSED_POLLS_NEEDED: u32 = 2;
    const MAX_WATCH_NS: u64 = 600_000_000_000;

    pub fn new(started_ns: u64) -> Self {
        Self { started_ns, saw_open: false, closed_polls: 0 }
    }

    /// The caller polls (about every 250 ms); `launchpad_open` is whether
    /// Launchpad is showing, `fingers_down` whether a finger is on the panel.
    pub fn poll(&mut self, launchpad_open: bool, fingers_down: bool, now_ns: u64) -> Action {
        let elapsed = now_ns.wrapping_sub(self.started_ns);
        if elapsed >= Self::MAX_WATCH_NS {
            return Action::Stop;
        }
        if launchpad_open {
            self.saw_open = true;
            self.closed_polls = 0;
            return Action::Wait;
        }
        self.closed_polls += 1;
        let gone = if self.saw_open {
            self.closed_polls >= Self::CLOSED_POLLS_NEEDED
        } else {
            elapsed >= Self::NOT_OPENED_TIMEOUT_NS
        };
        if gone && !fingers_down {
            Action::PushBack
        } else {
            Action::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;
    const MS: u64 = 1_000_000;

    #[test]
    fn the_dock_goes_back_once_launchpad_has_closed() {
        let mut watch = DockReturnWatch::new(0);
        assert_eq!(watch.poll(true, false, 250 * MS), Action::Wait);
        assert_eq!(watch.poll(true, true, 4 * S), Action::Wait, "the owner taps around in Launchpad");
        assert_eq!(watch.poll(false, false, 5 * S), Action::Wait, "one closed poll could be a flicker");
        assert_eq!(watch.poll(false, false, 5 * S + 250 * MS), Action::PushBack);
    }

    #[test]
    fn a_flicker_of_launchpad_does_not_count_as_closed() {
        let mut watch = DockReturnWatch::new(0);
        watch.poll(true, false, 250 * MS);
        assert_eq!(watch.poll(false, false, 500 * MS), Action::Wait);
        assert_eq!(watch.poll(true, false, 750 * MS), Action::Wait);
        assert_eq!(watch.poll(false, false, 1 * S), Action::Wait);
    }

    #[test]
    fn if_launchpad_never_opens_the_dock_still_goes_back_after_two_seconds() {
        let mut watch = DockReturnWatch::new(0);
        assert_eq!(watch.poll(false, false, 1 * S), Action::Wait);
        assert_eq!(watch.poll(false, false, 2 * S), Action::PushBack);
    }

    #[test]
    fn the_dock_waits_until_the_fingers_are_off_the_panel() {
        let mut watch = DockReturnWatch::new(0);
        watch.poll(true, false, 250 * MS);
        watch.poll(false, true, 500 * MS);
        assert_eq!(watch.poll(false, true, 750 * MS), Action::Wait, "the cursor is busy with a touch");
        assert_eq!(watch.poll(false, false, 1 * S), Action::PushBack);
    }

    #[test]
    fn the_watch_gives_up_after_ten_minutes() {
        let mut watch = DockReturnWatch::new(0);
        watch.poll(true, false, 250 * MS);
        assert_eq!(watch.poll(true, false, 600 * S), Action::Stop);
    }

    const MACBOOK: ScreenRect = ScreenRect { x: 0.0, y: 0.0, width: 1728.0, height: 1117.0 };
    const ARZOPA: ScreenRect = ScreenRect { x: 1728.0, y: 0.0, width: 1920.0, height: 1080.0 };

    /// Owner report 2026-10-11: with the Dock already on the touch display,
    /// "then back" never sent it back.
    #[test]
    fn then_back_returns_the_dock_even_when_it_was_already_on_the_touch_display() {
        let plan = LaunchpadDock::plan(LaunchpadDockMode::BringDockAndReturn, true, true);
        assert_eq!(plan, Plan::new(false, true));
    }

    #[test]
    fn each_mode_plans_its_moves() {
        assert_eq!(LaunchpadDock::plan(LaunchpadDockMode::BringDock, true, false), Plan::new(true, false));
        assert_eq!(LaunchpadDock::plan(LaunchpadDockMode::BringDock, true, true), Plan::new(false, false));
        assert_eq!(LaunchpadDock::plan(LaunchpadDockMode::BringDockAndReturn, true, false), Plan::new(true, true));
        assert_eq!(LaunchpadDock::plan(LaunchpadDockMode::Leave, true, false), Plan::new(false, false));
        assert_eq!(
            LaunchpadDock::plan(LaunchpadDockMode::BringDockAndReturn, false, true),
            Plan::new(false, false),
            "spreading = Show Desktop"
        );
    }

    #[test]
    fn the_docks_home_is_where_it_came_from() {
        let dock = ScreenPoint { x: 864.0, y: 1116.0 }; // auto-hidden at the MacBook's bottom edge
        assert_eq!(
            LaunchpadDock::home(dock, ARZOPA, &[MACBOOK, ARZOPA], Some(MACBOOK)),
            Some(MACBOOK)
        );
    }

    #[test]
    fn a_dock_found_on_the_touch_display_goes_home_to_the_main_display() {
        let dock = ScreenPoint { x: 2688.0, y: 1079.0 };
        assert_eq!(
            LaunchpadDock::home(dock, ARZOPA, &[MACBOOK, ARZOPA], Some(MACBOOK)),
            Some(MACBOOK)
        );
    }

    #[test]
    fn when_the_touch_display_is_the_main_one_the_dock_goes_to_another() {
        let dock = ScreenPoint { x: 2688.0, y: 1079.0 };
        assert_eq!(LaunchpadDock::home(dock, ARZOPA, &[ARZOPA, MACBOOK], Some(ARZOPA)), Some(MACBOOK));
        assert_eq!(LaunchpadDock::home(dock, ARZOPA, &[ARZOPA], Some(ARZOPA)), None, "nowhere else to go");
    }
}
