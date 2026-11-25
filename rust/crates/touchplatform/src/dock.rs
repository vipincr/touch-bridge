//! Where the Dock and Launchpad are.
//!
//! Launchpad opens on the display that has the Dock, and the Dock moves to a display when the
//! cursor pushes against that display's bottom edge (`--system-ui-selftest`, lp-4).

use objc2_core_foundation::{CFPreferencesCopyAppValue, CFString};
use touchcore::mapper::{ScreenPoint, ScreenRect};

use crate::accessibility::{self, ROLE};
use crate::apps;
use crate::window::WindowLocator;

/// Locates the Dock and Launchpad.
pub struct DockLocator;

/// The Dock's process, while it runs.
fn dock_pid() -> Option<i32> {
    apps::with_bundle_id("com.apple.dock").map(|dock| dock.processIdentifier())
}

impl DockLocator {
    /// Only a Dock at the bottom moves between displays this way. True when the preference is
    /// unset or "bottom".
    pub fn is_at_bottom() -> bool {
        let orientation = CFString::from_str("orientation");
        let domain = CFString::from_str("com.apple.dock");
        let position = CFPreferencesCopyAppValue(&orientation, &domain).and_then(|value| {
            let text: &CFString = value.downcast_ref()?;
            Some(text.to_string())
        });
        match position {
            None => true,
            Some(position) => position == "bottom",
        }
    }

    /// A point just inside the bottom edge of the Dock's display: the Dock's tile list sits on that
    /// edge, or just below it while auto-hidden. `None` when Accessibility can't tell.
    pub fn anchor() -> Option<ScreenPoint> {
        let pid = dock_pid()?;
        let app = accessibility::application(pid, 0.2);
        let list = accessibility::children(&app)
            .into_iter()
            .find(|child| accessibility::string(child, ROLE).as_deref() == Some("AXList"))?;
        let frame = accessibility::frame(&list)?;
        Some(ScreenPoint { x: frame.mid().x, y: frame.origin.y - 1.0 })
    }

    /// Whether the Dock is on `display`; `None` when the anchor is unknown.
    pub fn is_dock(display: ScreenRect) -> Option<bool> {
        Self::anchor().map(|anchor| display.contains(anchor))
    }

    /// Launchpad's full-screen windows: the Dock's, above its own level (20) and below pop-up
    /// menus. Show Desktop and Mission Control use levels 18 and 20.
    pub fn launchpad_windows() -> Vec<ScreenRect> {
        let Some(pid) = dock_pid() else { return Vec::new() };
        WindowLocator::on_screen_windows()
            .into_iter()
            .filter(|window| window.pid == pid && window.layer > 20 && window.layer < 100)
            .map(|window| window.bounds)
            .collect()
    }
}
