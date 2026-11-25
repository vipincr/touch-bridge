//! A window that receives events straight from TouchNext (Direct delivery), without the cursor
//! moving.
//!
//! [`CGEvent::post_to_pid`] alone delivers events with no window attached (AppKit sees windowNumber
//! 0 and drops them). The window server normally fills in two things when it routes an event, and
//! the same two are set here:
//!
//! - field 51: the window number (AppKit's NSEvent.windowNumber; fields 91/92 are not used for this)
//! - the window-relative location, top-left origin, via the private `CGEventSetWindowLocation`
//!
//! Found by diffing routed and posted events in the Event Sink (see docs/spec.md, ledger).

use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{CGEvent, CGEventField};
use touchcore::addressing::DirectAddressing;
use touchcore::mapper::ScreenRect;

use crate::geometry::screen_point;
use crate::runtime_symbols::lookup;

/// The window number, field 51: what AppKit reports as `NSEvent.windowNumber`.
const WINDOW_NUMBER_FIELD: CGEventField = CGEventField(51);

/// `void CGEventSetWindowLocation(CGEventRef event, CGPoint windowLocation)`, the private call that
/// writes the window-relative location into an event.
type SetWindowLocation = unsafe extern "C" fn(NonNull<CGEvent>, CGPoint);

static SET_WINDOW_LOCATION: OnceLock<Option<SetWindowLocation>> = OnceLock::new();

/// The private `CGEventSetWindowLocation`, when this macOS exports it.
fn set_window_location() -> Option<SetWindowLocation> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"CGEventSetWindowLocation", &SET_WINDOW_LOCATION) }
}

/// A window that events are posted to directly, by process id.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectTarget {
    /// The process owning the window.
    pub pid: i32,
    pub window: i64,
    /// The window's top-left corner in global display points.
    pub origin: CGPoint,
}

// CGPoint only derives `PartialEq` (a float cannot honestly promise `Eq`), but Swift's
// `DirectTarget: Equatable` compares the same way, so equality here means the same thing.
impl Eq for DirectTarget {}

impl DirectTarget {
    /// Whether this macOS still offers the private call Direct delivery depends on.
    pub fn is_available() -> bool {
        set_window_location().is_some()
    }

    /// Addresses `event` to this window, the way the window server addresses a routed one, and
    /// hands it straight to the app's process.
    pub fn post(&self, event: &CGEvent) {
        CGEvent::set_integer_value_field(Some(event), WINDOW_NUMBER_FIELD, self.window);
        let local = DirectAddressing::window_location(
            screen_point(CGEvent::location(Some(event))),
            ScreenRect { x: self.origin.x, y: self.origin.y, width: 0.0, height: 0.0 },
        );
        if let Some(set) = set_window_location() {
            // SAFETY: `event` is a live CGEvent, and `set` is `CGEventSetWindowLocation` itself.
            unsafe { set(NonNull::from(event), CGPoint::new(local.x, local.y)) };
        }
        CGEvent::post_to_pid(self.pid, Some(event));
    }
}
