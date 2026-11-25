//! Opens Notification Center the way a click on the menu-bar clock does: macOS has no public call
//! for it, and it is not one of the dock swipes.
//!
//! Notification Center opens on the display that has the cursor at that moment, whichever clock
//! opened it (`--system-ui-selftest`, 2026-10-11). Accessibility only reports the clock of the
//! active menu bar.

use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFRetained, CGRect};
use touchcore::mapper::ScreenRect;

use crate::accessibility::{self, EXTRAS_MENU_BAR, IDENTIFIER};
use crate::apps;

/// The menu-bar clock, as the Accessibility API reports it.
pub struct Clock {
    /// The clock's status-item element, the one a press opens the panel from.
    pub element: CFRetained<AXUIElement>,
    /// The clock's frame in screen points.
    pub frame: CGRect,
    /// ControlCenter's process, which owns the clock's status-item window on every display.
    pub pid: i32,
}

/// Finds the menu-bar clock, presses it, and reads where the panel is open.
pub struct NotificationCenterOpener;

impl NotificationCenterOpener {
    /// The menu-bar clock of the active menu bar. `None` when ControlCenter is not running, or does
    /// not answer within its 0.2 s messaging timeout.
    pub fn clock() -> Option<Clock> {
        let control_center = apps::with_bundle_id("com.apple.controlcenter")?;
        let pid = control_center.processIdentifier();
        let app = accessibility::application(pid, 0.2);
        let bar = accessibility::element(&app, EXTRAS_MENU_BAR)?;
        let clock = accessibility::children(&bar).into_iter().find(|item| {
            accessibility::string(item, IDENTIFIER).as_deref() == Some("com.apple.menuextra.clock")
        })?;
        let frame = accessibility::frame(&clock)?;
        Some(Clock { element: clock, frame, pid })
    }

    /// The Accessibility "press" (no cursor movement). True when the action was accepted.
    pub fn press(clock: &Clock) -> bool {
        accessibility::press(&clock.element)
    }

    /// Where the panel is open: Notification Center's full-display windows (layer 23) on screen, as
    /// bounds.
    pub fn open_panels() -> Vec<ScreenRect> {
        let Some(pid) = apps::with_bundle_id("com.apple.notificationcenterui").map(|app| app.processIdentifier())
        else {
            return Vec::new();
        };
        crate::window::WindowLocator::on_screen_windows()
            .into_iter()
            .filter(|window| window.pid == pid && window.layer == 23)
            .map(|window| window.bounds)
            .collect()
    }
}
