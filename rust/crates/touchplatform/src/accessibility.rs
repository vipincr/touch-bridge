//! Reading the user interface through the Accessibility API.
//!
//! The API is a set of C calls with out parameters, each a synchronous message to another process.
//! They are wrapped here once, so the modules that ask what is under a point or where the Dock sits
//! deal in `Option`s. An app that does not answer within its element's timeout yields `None`.
//!
//! The attribute names are not generated as constants by the bindings, so they are spelled here, in
//! one place.

use std::ptr::NonNull;

use objc2_application_services::{AXError, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFArray, CFRetained, CFString, CFType, CGPoint, CGSize, CGRect, Type};

/// The element's role, as "AXWindow" or "AXButton".
pub const ROLE: &str = "AXRole";
/// The element's subrole, when it has one (a standard window is "AXStandardWindow").
pub const SUBROLE: &str = "AXSubrole";
/// The element's children, front to back.
pub const CHILDREN: &str = "AXChildren";
/// The element's parent.
pub const PARENT: &str = "AXParent";
/// The element's top-left corner in screen points, an `AXValue` holding a `CGPoint`.
pub const POSITION: &str = "AXPosition";
/// The element's size in screen points, an `AXValue` holding a `CGSize`.
pub const SIZE: &str = "AXSize";
/// The element's identifier, where the app sets one (the clock's is "com.apple.menuextra.clock").
pub const IDENTIFIER: &str = "AXIdentifier";
/// The menu bar extras of an app that is not frontmost (the clock on every display).
pub const EXTRAS_MENU_BAR: &str = "AXExtrasMenuBar";
/// The action of clicking the element.
pub const PRESS: &str = "AXPress";

/// The system-wide element: the one that answers "what is at this point".
pub fn system_wide(timeout: f32) -> CFRetained<AXUIElement> {
    // SAFETY: creating the element has no preconditions, and the timeout is a plain number of seconds.
    unsafe {
        let element = AXUIElement::new_system_wide();
        element.set_messaging_timeout(timeout);
        element
    }
}

/// The root element of the app that runs as `pid`.
pub fn application(pid: i32, timeout: f32) -> CFRetained<AXUIElement> {
    // SAFETY: any pid is accepted (an unknown one simply fails later), and the timeout is a plain
    // number of seconds.
    unsafe {
        let element = AXUIElement::new_application(pid);
        element.set_messaging_timeout(timeout);
        element
    }
}

/// One attribute of an element: its value, retained. `None` when the app refuses, has no such
/// attribute, or does not answer in time.
pub fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = std::ptr::null();
    // SAFETY: `value` is a valid out slot and the attribute name is a valid string.
    let error = unsafe { element.copy_attribute_value(&CFString::from_str(name), NonNull::from(&mut value)) };
    if error != AXError::Success {
        return None;
    }
    // SAFETY: on success the slot holds a +1 reference, which `from_raw` takes over.
    Some(unsafe { CFRetained::from_raw(NonNull::new(value.cast_mut())?) })
}

/// A string attribute, as text.
pub fn string(element: &AXUIElement, name: &str) -> Option<String> {
    let value = attribute(element, name)?;
    let text: &CFString = value.downcast_ref()?;
    Some(text.to_string())
}

/// An attribute whose value is itself an element.
pub fn element(element: &AXUIElement, name: &str) -> Option<CFRetained<AXUIElement>> {
    let value = attribute(element, name)?;
    // The type-id check is what Swift's `CFGetTypeID(v) == AXUIElementGetTypeID()` does.
    Some(value.downcast_ref::<AXUIElement>()?.retain())
}

/// The element's children, front to back; empty when it has none or does not answer.
pub fn children(element: &AXUIElement) -> Vec<CFRetained<AXUIElement>> {
    let Some(value) = attribute(element, CHILDREN) else { return Vec::new() };
    let Some(list) = value.downcast_ref::<CFArray>() else { return Vec::new() };
    // An AXUIElement has no CF type id of its own to check against, so the array is taken as what
    // the Accessibility API documents it to be.
    let list: CFRetained<CFArray<AXUIElement>> = unsafe { CFRetained::cast_unchecked(list.retain()) };
    list.to_vec()
}

/// The element's parent.
pub fn parent(of: &AXUIElement) -> Option<CFRetained<AXUIElement>> {
    element(of, PARENT)
}

/// The element's frame in screen points; `None` when the app does not report a position and a size.
pub fn frame(element: &AXUIElement) -> Option<CGRect> {
    let mut origin = CGPoint::new(0.0, 0.0);
    let mut size = CGSize::new(0.0, 0.0);
    read_value(&attribute(element, POSITION)?, AXValueType::CGPoint, NonNull::from(&mut origin).cast())?;
    read_value(&attribute(element, SIZE)?, AXValueType::CGSize, NonNull::from(&mut size).cast())?;
    Some(CGRect::new(origin, size))
}

/// The process the element belongs to.
pub fn pid(element: &AXUIElement) -> Option<i32> {
    let mut pid: i32 = 0;
    // SAFETY: `pid` is a valid out slot.
    let error = unsafe { element.pid(NonNull::from(&mut pid)) };
    (error == AXError::Success).then_some(pid)
}

/// The element at a screen position, as the system-wide element sees it.
pub fn element_at(system: &AXUIElement, point: CGPoint) -> Option<CFRetained<AXUIElement>> {
    let mut hit: *const AXUIElement = std::ptr::null();
    // SAFETY: `hit` is a valid out slot and the coordinates are plain numbers.
    let error = unsafe { system.copy_element_at_position(point.x as f32, point.y as f32, NonNull::from(&mut hit)) };
    if error != AXError::Success {
        return None;
    }
    // SAFETY: on success the slot holds a +1 reference, which `from_raw` takes over.
    Some(unsafe { CFRetained::from_raw(NonNull::new(hit.cast_mut())?) })
}

/// Clicks the element through Accessibility (no cursor movement). True when the action was accepted.
pub fn press(element: &AXUIElement) -> bool {
    // SAFETY: the action name is a valid string; a refusal is reported in the return value.
    let error = unsafe { element.perform_action(&CFString::from_str(PRESS)) };
    error == AXError::Success
}

/// Reads a point or size out of an `AXValue`, through `target`. `None` when the value is not an
/// `AXValue`; a value of the wrong type leaves the target at zero, as it does in the Swift original.
fn read_value(value: &CFRetained<CFType>, kind: AXValueType, target: NonNull<std::ffi::c_void>) -> Option<()> {
    let value: &AXValue = value.downcast_ref()?;
    // SAFETY: the caller passes a valid, writable `CGPoint` or `CGSize` to match `kind`.
    unsafe { value.value(kind, target) };
    Some(())
}
