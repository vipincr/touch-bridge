//! The on-screen windows, the apps behind them, and the drag zones of a window's top edge.
//!
//! [`WindowLocator`] reads the window server's list. [`AppDirectory`] answers per-process questions
//! and remembers them by bundle path. [`WindowDragArea`] asks the Accessibility API what sits under
//! a point and applies the core's drag rules to it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use objc2_application_services::AXUIElement;
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGWindowAlpha, kCGWindowBounds, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerName,
    kCGWindowOwnerPID,
};
use touchcore::drag_rules::WindowDragRules;
use touchcore::policy::WindowInfo;
use touchcore::traits::AppTraits;
use touchhid::clock;

use crate::accessibility::{self, ROLE, SUBROLE};
use crate::apps;
use crate::geometry::screen_rect;

/// Reads the on-screen window list, front to back. Bounds, pid and layer need no Screen Recording
/// permission.
pub struct WindowLocator;

impl WindowLocator {
    /// The on-screen windows, front to back, as the window server reports them. A window without a
    /// number, an owner pid or readable bounds is skipped; the owner name defaults to empty, the
    /// layer to 0 and the alpha to 1.
    pub fn on_screen_windows() -> Vec<WindowInfo> {
        type WindowEntry = CFDictionary<CFString, CFType>;
        let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, 0);
        // SAFETY: the window server fills the array with dictionaries keyed by CFString, exactly
        // what `WindowEntry` says.
        let list: Option<CFRetained<CFArray<WindowEntry>>> =
            list.map(|list| unsafe { CFRetained::cast_unchecked(list) });
        let mut windows = Vec::new();
        for entry in list.into_iter().flat_map(|list| list.to_vec()) {
            if let Some(window) = Self::window_info(&entry) {
                windows.push(window);
            }
        }
        windows
    }

    /// One entry of the window list as a [`WindowInfo`]; `None` when the number, the owner pid or
    /// the bounds are missing.
    fn window_info(entry: &CFDictionary<CFString, CFType>) -> Option<WindowInfo> {
        let number = entry.get(unsafe { kCGWindowNumber })?.downcast_ref::<CFNumber>()?.as_i64()?;
        let pid = entry.get(unsafe { kCGWindowOwnerPID })?.downcast_ref::<CFNumber>()?.as_i32()?;
        let bounds = entry.get(unsafe { kCGWindowBounds })?;
        let bounds: &CFDictionary = bounds.downcast_ref()?;
        let mut rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
        // SAFETY: `rect` is a valid, writable out slot, and `bounds` is the dictionary CoreGraphics
        // documents this call to read.
        if !unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut rect) } {
            return None;
        }
        let owner_name = entry
            .get(unsafe { kCGWindowOwnerName })
            .and_then(|value| {
                let text: &CFString = value.downcast_ref()?;
                Some(text.to_string())
            })
            .unwrap_or_default();
        let layer = entry
            .get(unsafe { kCGWindowLayer })
            .and_then(|value| {
                let number: &CFNumber = value.downcast_ref()?;
                number.as_i64()
            })
            .unwrap_or(0);
        let alpha = entry
            .get(unsafe { kCGWindowAlpha })
            .and_then(|value| {
                let number: &CFNumber = value.downcast_ref()?;
                number.as_f64()
            })
            .unwrap_or(1.0);
        Some(WindowInfo {
            number,
            pid,
            owner_name,
            layer,
            bounds: screen_rect(rect),
            alpha,
        })
    }
}

/// Per-process facts, cached by bundle path: each app's `Contents/Frameworks` is scanned once for
/// the Chromium marker framework, however often its windows are asked about.
pub struct AppDirectory {
    /// Bundle path -> whether that app ships a Chromium framework.
    chromium: Mutex<HashMap<String, bool>>,
}

impl AppDirectory {
    /// An empty directory; every pid is looked up on first use.
    pub fn new() -> Self {
        Self { chromium: Mutex::new(HashMap::new()) }
    }

    /// The bundle identifier of the app that runs as `pid`; `None` when no running app has it.
    pub fn bundle_id(&self, pid: i32) -> Option<String> {
        apps::with_pid(pid)?.bundleIdentifier().map(|id| id.to_string())
    }

    /// Whether the app that runs as `pid` ships a Chromium or Electron framework (its windows
    /// ignore synthesized events posted straight to them, so the caller warps instead). The answer
    /// is cached by bundle path, so `Contents/Frameworks` is scanned once per app.
    pub fn is_chromium(&self, pid: i32) -> bool {
        let Some(url) = apps::with_pid(pid).and_then(|app| app.bundleURL()) else { return false };
        let path = url.path().map(|path| path.to_string()).unwrap_or_default();
        let mut chromium = self.chromium.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(known) = chromium.get(&path) {
            return *known;
        }
        let ships_chromium = Self::scan_frameworks(&path);
        chromium.insert(path, ships_chromium);
        ships_chromium
    }

    /// Whether `<bundle path>/Contents/Frameworks` holds a Chromium marker framework. An unreadable
    /// directory counts as no, as the Swift `try?` does.
    fn scan_frameworks(path: &str) -> bool {
        let Ok(entries) = std::fs::read_dir(format!("{path}/Contents/Frameworks")) else { return false };
        entries
            .filter_map(|entry| entry.ok())
            .any(|entry| AppTraits::is_chromium_framework(&entry.file_name().to_string_lossy()))
    }
}

impl Default for AppDirectory {
    fn default() -> Self {
        Self::new()
    }
}

/// Asks the Accessibility API what is under a point and applies [`WindowDragRules`].
///
/// Only consulted for pans and drags that start near the top of a window, with a short timeout; any
/// failure falls back to the original 28 pt title-bar rule.
pub struct WindowDragArea;

/// The system-wide element [`WindowDragArea`] hits against, created once with its per-message
/// timeout, as the Swift static is.
static SYSTEM: OnceLock<SharedSystem> = OnceLock::new();

/// An [`AXUIElement`] shared across threads. The API answers synchronous inter-process messages on
/// any thread, and the Swift static was `nonisolated(unsafe)` for the same reason.
struct SharedSystem(CFRetained<AXUIElement>);
// SAFETY: a CF object is refcounted atomically, and an element holds no thread-bound state; every
// call on it is an independent message.
unsafe impl Send for SharedSystem {}
// SAFETY: as above, calls on the shared element are independent messages.
unsafe impl Sync for SharedSystem {}

impl WindowDragArea {
    /// Toolbars and title bars live in the top of a window; Accessibility is skipped below this.
    pub const SEARCH_BAND: f64 = 120.0;
    /// Per Accessibility message, and for the whole lookup (the walk stops when the budget is spent).
    pub const TIMEOUT: f32 = 0.05;
    /// The whole lookup, in nanoseconds of the driver's clock.
    pub const BUDGET_NS: u64 = 60_000_000;

    /// Whether a pan that starts at `point` should move `window`. With `use_accessibility` false,
    /// the original 28 pt title-bar rule decides on its own.
    pub fn is_drag_area(point: CGPoint, window: &WindowInfo, use_accessibility: bool) -> bool {
        let offset = point.y - window.bounds.y;
        if !(0.0..Self::SEARCH_BAND).contains(&offset) {
            return false;
        }
        let chain = if use_accessibility { Self::chain(point, window.pid) } else { Vec::new() };
        WindowDragRules::is_drag_area(&chain, offset)
    }

    /// "Role" or "Role/Subrole" from the element at the point up to its window; empty when
    /// Accessibility does not answer in time or the element belongs to another app. At most 8
    /// entries, and never past the budget.
    pub fn chain(point: CGPoint, expected_pid: i32) -> Vec<String> {
        let deadline = clock::now_ns() + Self::BUDGET_NS;
        let Some(element) = accessibility::element_at(&Self::system().0, point) else { return Vec::new() };
        if accessibility::pid(&element) != Some(expected_pid) {
            return Vec::new();
        }
        let mut chain = Vec::new();
        let mut current = Some(element);
        while let Some(element) = current {
            if chain.len() >= 8 || clock::now_ns() >= deadline {
                break;
            }
            let role = strip(&accessibility::string(&element, ROLE).unwrap_or_else(|| "Unknown".to_string()));
            let subrole = accessibility::string(&element, SUBROLE)
                .map(|subrole| format!("/{}", strip(&subrole)))
                .unwrap_or_default();
            chain.push(format!("{role}{subrole}"));
            if role == "Window" {
                break;
            }
            current = accessibility::parent(&element);
        }
        chain
    }

    /// The shared system-wide element, created on first use.
    fn system() -> &'static SharedSystem {
        SYSTEM.get_or_init(|| SharedSystem(accessibility::system_wide(Self::TIMEOUT)))
    }
}

/// Removes the "AX" prefix a role or subrole carries ("AXButton" becomes "Button").
fn strip(name: &str) -> String {
    name.strip_prefix("AX").unwrap_or(name).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use touchcore::mapper::ScreenRect;

    /// A window pinned to the screen's top-left corner.
    fn window_at_origin() -> WindowInfo {
        WindowInfo {
            number: 1,
            pid: 42,
            owner_name: "Test".to_string(),
            layer: 0,
            bounds: ScreenRect { x: 0.0, y: 0.0, width: 800.0, height: 600.0 },
            alpha: 1.0,
        }
    }

    #[test]
    fn a_touch_outside_the_search_band_is_never_a_drag_area() {
        let window = window_at_origin();
        assert!(!WindowDragArea::is_drag_area(CGPoint::new(10.0, -1.0), &window, false));
        assert!(!WindowDragArea::is_drag_area(CGPoint::new(10.0, 120.0), &window, false));
        assert!(!WindowDragArea::is_drag_area(CGPoint::new(10.0, 700.0), &window, false));
    }

    #[test]
    fn with_no_accessibility_answer_the_fallback_rule_decides() {
        let window = window_at_origin();
        // 10 pt below the top edge: inside the original 28 pt title bar.
        assert!(WindowDragArea::is_drag_area(CGPoint::new(10.0, 10.0), &window, false));
        // 119 pt below it: inside the band, but no longer a title bar without an answer.
        assert!(!WindowDragArea::is_drag_area(CGPoint::new(10.0, 119.0), &window, false));
    }

    #[test]
    fn strip_removes_the_ax_prefix() {
        assert_eq!(strip("AXWindow"), "Window");
        assert_eq!(strip("Button"), "Button");
    }
}
