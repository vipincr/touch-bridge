//! Hides the cursor while TouchNext is in the background, via a private CoreGraphics call looked up
//! at runtime. If the call is missing, the cursor simply stays visible during Warp gestures.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use objc2_core_foundation::{CFBoolean, CFString, CFType};
use objc2_core_graphics::{CGDisplayHideCursor, CGDisplayShowCursor, CGMainDisplayID};

use crate::runtime_symbols::lookup;

/// `int _CGSDefaultConnection(void)`: the process's window server connection id.
type DefaultConnection = unsafe extern "C" fn() -> i32;
/// `int CGSSetConnectionProperty(int, int, CFStringRef, CFTypeRef)`: sets a connection property.
type SetProperty = unsafe extern "C" fn(i32, i32, *const CFString, *const CFType) -> i32;

static DEFAULT_CONNECTION: OnceLock<Option<DefaultConnection>> = OnceLock::new();
static SET_PROPERTY: OnceLock<Option<SetProperty>> = OnceLock::new();

/// Whether the window server accepted the background-hiding property.
static AVAILABLE: AtomicBool = AtomicBool::new(false);
/// The cursor is currently hidden by this process.
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// The window server connection, when this macOS exports the private getter.
fn default_connection() -> Option<DefaultConnection> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"_CGSDefaultConnection", &DEFAULT_CONNECTION) }
}

/// The private property setter, when this macOS exports it.
fn set_property() -> Option<SetProperty> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"CGSSetConnectionProperty", &SET_PROPERTY) }
}

/// The Swift namespace `CursorVisibility`: cursor hiding and showing, at most once each way.
pub struct CursorVisibility;

impl CursorVisibility {
    /// Asks the window server to let this process hide the cursor while none of its windows is
    /// frontmost. Without the property, `CGDisplayHideCursor` only works while the app is active.
    pub fn allow_hiding_in_background() {
        let (Some(connection_fn), Some(set)) = (default_connection(), set_property()) else {
            return;
        };
        // SAFETY: `_CGSDefaultConnection` takes no arguments and answers the session connection.
        let connection = unsafe { connection_fn() };
        let name = CFString::from_str("SetsCursorInBackground");
        let value = CFBoolean::new(true) as *const CFBoolean as *const CFType;
        // SAFETY: the property name is a live CFString and the value a static CFBoolean, which is
        // what the private call takes; zero means the property was set.
        let accepted = unsafe { set(connection, connection, &*name, value) } == 0;
        AVAILABLE.store(accepted, Ordering::Relaxed);
    }

    /// Hides the cursor on the main display, once, until [`show`](Self::show) is called.
    pub fn hide() {
        if !AVAILABLE.load(Ordering::Relaxed) || HIDDEN.load(Ordering::Relaxed) {
            return;
        }
        CGDisplayHideCursor(CGMainDisplayID());
        HIDDEN.store(true, Ordering::Relaxed);
    }

    /// Shows the cursor again, once, after a [`hide`](Self::hide).
    pub fn show() {
        if !HIDDEN.load(Ordering::Relaxed) {
            return;
        }
        CGDisplayShowCursor(CGMainDisplayID());
        HIDDEN.store(false, Ordering::Relaxed);
    }
}
