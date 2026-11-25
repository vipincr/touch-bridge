//! The displays the window server reports, with the names AppKit gives them.
//!
//! CoreGraphics lists every online display with its geometry and identity; AppKit knows the names
//! the user sees, but answers on the main thread only. [`DisplayLocator`] asks each side for its
//! half and joins the two.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSNumber, NSString};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayIsMain, CGDisplayModelNumber, CGDisplayRotation, CGDisplaySerialNumber,
    CGDisplayVendorNumber,
};

use crate::system_displays::SystemDisplays;

/// A connected display as CoreGraphics sees it. `bounds` are global points, top-left origin of the
/// main display.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    /// The display's `CGDirectDisplayID`.
    pub id: u32,
    /// AppKit's localized name; "display \<id\>" when AppKit has none.
    pub name: String,
    /// The display's vendor id, as the EDID reports it.
    pub vendor: u32,
    /// The display's model id.
    pub model: u32,
    /// The display's serial number.
    pub serial: u32,
    /// The frame in global points, top-left origin of the main display.
    pub bounds: CGRect,
    /// Degrees clockwise from upright.
    pub rotation: f64,
    /// Whether this is the display whose top-left corner anchors global coordinates.
    pub is_main: bool,
}

/// Locates the displays.
pub struct DisplayLocator;

impl DisplayLocator {
    /// AppKit's name for each online display, paired with its display number. Main thread only,
    /// because `NSScreen` answers there.
    pub fn screen_names(mtm: MainThreadMarker) -> Vec<(u32, String)> {
        let number_key = NSString::from_str("NSScreenNumber");
        let mut names = Vec::new();
        for screen in NSScreen::screens(mtm).iter() {
            let Some(number) = screen.deviceDescription().objectForKey(&number_key) else { continue };
            // The description holds an `NSNumber`; the cast checks the class, as a Swift `as?` does.
            let Some(number) = number.downcast_ref::<NSNumber>() else { continue };
            names.push((number.unsignedIntValue(), screen.localizedName().to_string()));
        }
        names
    }

    /// The online displays in CoreGraphics' order, each with its name from `names` ("display \<id\>"
    /// when `names` has none for it). Call [`DisplayLocator::screen_names`] first, on the main
    /// thread.
    pub fn online_displays(names: &[(u32, String)]) -> Vec<DisplayInfo> {
        SystemDisplays::online()
            .into_iter()
            .map(|id| {
                let name = names
                    .iter()
                    .find(|(number, _)| *number == id)
                    .map(|(_, name)| name.clone())
                    .unwrap_or_else(|| format!("display {id}"));
                DisplayInfo {
                    id,
                    name,
                    vendor: CGDisplayVendorNumber(id),
                    model: CGDisplayModelNumber(id),
                    serial: CGDisplaySerialNumber(id),
                    bounds: CGDisplayBounds(id),
                    rotation: CGDisplayRotation(id),
                    is_main: CGDisplayIsMain(id),
                }
            })
            .collect()
    }
}
