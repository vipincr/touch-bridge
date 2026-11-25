//! Displays, plus the window server's per-display Space state.
//!
//! The Space state lives behind private SkyLight calls, looked up by name at run time; they are
//! read-only, and each answers `None` (or an empty vector) when its symbol is missing on this
//! macOS. Called from the pipeline's thread, exactly where the Swift called them from its queue.

use std::sync::OnceLock;

use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CFUUID,
};
use objc2_core_graphics::{CGDisplayBounds, CGGetOnlineDisplayList};
use touchcore::mapper::ScreenRect;

use crate::geometry::screen_rect;
use crate::runtime_symbols::lookup;

/// `int _CGSDefaultConnection(void)`: the process's window server connection id.
type DefaultConnection = unsafe extern "C" fn() -> i32;
/// `CFStringRef SLSCopyActiveMenuBarDisplayIdentifier(int connection)`: the UUID of the display
/// whose menu bar is active.
type CopyActiveMenuBarDisplayIdentifier = unsafe extern "C" fn(i32) -> *mut CFString;
/// `CFArrayRef SLSCopyManagedDisplaySpaces(int connection)`: one dictionary per display that has
/// its own Spaces.
type CopyManagedDisplaySpaces = unsafe extern "C" fn(i32) -> *mut CFArray;
/// `CFUUIDRef CGDisplayCreateUUIDFromDisplayID(CGDirectDisplayID)`: public, but missing from the
/// generated bindings.
type CreateUUIDFromDisplayID = unsafe extern "C" fn(u32) -> *mut CFUUID;

/// One entry of `SLSCopyManagedDisplaySpaces`: a dictionary keyed by CFString.
type SpaceEntry = CFDictionary<CFString, CFType>;

static DEFAULT_CONNECTION: OnceLock<Option<DefaultConnection>> = OnceLock::new();
static COPY_ACTIVE_MENU_BAR: OnceLock<Option<CopyActiveMenuBarDisplayIdentifier>> = OnceLock::new();
static COPY_MANAGED_DISPLAY_SPACES: OnceLock<Option<CopyManagedDisplaySpaces>> = OnceLock::new();
static CREATE_UUID: OnceLock<Option<CreateUUIDFromDisplayID>> = OnceLock::new();

/// The window server connection, computed afresh on every call, as the Swift computed it.
fn connection() -> Option<i32> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    let connection = unsafe { lookup(c"_CGSDefaultConnection", &DEFAULT_CONNECTION) }?;
    // SAFETY: the call takes nothing and answers the session's connection id.
    Some(unsafe { connection() })
}

/// The managed per-display Space dictionaries, or an empty vector when the private call is
/// missing or answers nothing.
fn managed_display_spaces() -> Vec<CFRetained<SpaceEntry>> {
    let Some(connection) = connection() else { return Vec::new() };
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    let Some(copy) = (unsafe { lookup(c"SLSCopyManagedDisplaySpaces", &COPY_MANAGED_DISPLAY_SPACES) })
    else {
        return Vec::new();
    };
    // SAFETY: the call answers a +1 CFArray, or null.
    let Some(raw) = std::ptr::NonNull::new(unsafe { copy(connection) }) else {
        return Vec::new();
    };
    // SAFETY: the call handed us the array, and SLSCopyManagedDisplaySpaces fills it with
    // dictionaries whose keys are CFStrings, which is what `SpaceEntry` says.
    let list: CFRetained<CFArray<SpaceEntry>> =
        unsafe { CFRetained::cast_unchecked(CFRetained::from_raw(raw)) };
    list.to_vec()
}

/// A dictionary entry's string value.
fn entry_string(entry: &SpaceEntry, key: &CFString) -> Option<String> {
    entry.get(key)?.downcast_ref::<CFString>().map(|value| value.to_string())
}

/// A dictionary entry's integer value, read as a 64-bit signed number (Space ids are small
/// enough that the signed read carries every id the window server hands out).
fn entry_u64(entry: &SpaceEntry, key: &CFString) -> Option<u64> {
    entry.get(key)?.downcast_ref::<CFNumber>()?.as_i64().map(|value| value as u64)
}

/// A display entry's "Current Space" dictionary, present only when the display has Spaces.
fn current_space(entry: &SpaceEntry) -> Option<CFRetained<SpaceEntry>> {
    let key = CFString::from_str("Current Space");
    let value = entry.get(&key)?;
    // The Swift read this with `as?`, so anything that is not a dictionary answers nothing here
    // either; the checked downcast is that check.
    value.downcast_ref::<CFDictionary>()?;
    // SAFETY: the value is a CFDictionary (checked just above), and the window server keys it by
    // CFString, which is what `SpaceEntry` says.
    Some(unsafe { CFRetained::cast_unchecked(value) })
}

/// Displays, plus the window server's per-display Space state.
pub struct SystemDisplays;

impl SystemDisplays {
    /// Every online display's `CGDirectDisplayID`, in CoreGraphics' order (the count is asked
    /// first, then the list, exactly as the Swift asked).
    pub fn online() -> Vec<u32> {
        let mut count: u32 = 0;
        // SAFETY: a null list asks for the count only, and `count` is a valid out slot.
        unsafe { CGGetOnlineDisplayList(0, std::ptr::null_mut(), &mut count) };
        let mut ids = vec![0u32; count as usize];
        // SAFETY: `ids` holds `count` slots, and `count` is a valid out slot.
        unsafe { CGGetOnlineDisplayList(count, ids.as_mut_ptr(), &mut count) };
        ids.truncate(count as usize);
        ids
    }

    /// A display's bounds in global points.
    pub fn rect(id: u32) -> ScreenRect {
        screen_rect(CGDisplayBounds(id))
    }

    /// A display's UUID as the window server names it, or `None` when the public call the
    /// bindings lack is missing here.
    pub fn uuid(id: u32) -> Option<String> {
        // SAFETY: the cell stores exactly the C signature of the symbol it names.
        let create = unsafe { lookup(c"CGDisplayCreateUUIDFromDisplayID", &CREATE_UUID) }?;
        // SAFETY: the call answers a +1 CFUUID, or null.
        let raw = std::ptr::NonNull::new(unsafe { create(id) })?;
        let uuid = unsafe { CFRetained::from_raw(raw) };
        CFUUID::new_string(None, Some(&uuid)).map(|string| string.to_string())
    }

    /// The display whose menu bar is active (the one with the focused window), as a display UUID.
    pub fn active_menu_bar_display_uuid() -> Option<String> {
        let connection = connection()?;
        // SAFETY: the cell stores exactly the C signature of the symbol it names.
        let copy = unsafe { lookup(c"SLSCopyActiveMenuBarDisplayIdentifier", &COPY_ACTIVE_MENU_BAR) }?;
        // SAFETY: the call answers a +1 CFString, or null.
        let raw = std::ptr::NonNull::new(unsafe { copy(connection) })?;
        let identifier = unsafe { CFRetained::from_raw(raw) };
        Some(identifier.to_string())
    }

    /// Every display with its own Spaces: its UUID, its Spaces in order, and the current one.
    pub fn space_lists() -> Vec<SpaceList> {
        let id64 = CFString::from_str("id64");
        let mut lists = Vec::new();
        for entry in managed_display_spaces() {
            let Some(uuid) = entry_string(&entry, &CFString::from_str("Display Identifier")) else {
                continue;
            };
            let Some(current) = current_space(&entry).and_then(|space| entry_u64(&space, &id64))
            else {
                continue;
            };
            let spaces = entry
                .get(&CFString::from_str("Spaces"))
                .and_then(|value| {
                    // The Swift read the list with `as?`, so a value that is not an array
                    // contributes none.
                    value.downcast_ref::<CFArray>()?;
                    // SAFETY: the value is a CFArray (checked just above), and the window server
                    // fills it with dictionaries keyed by CFString, which is what `SpaceEntry`
                    // says.
                    let array: CFRetained<CFArray<SpaceEntry>> =
                        unsafe { CFRetained::cast_unchecked(value) };
                    Some(array)
                })
                .map(|array| {
                    array.to_vec().iter().filter_map(|space| entry_u64(space, &id64)).collect()
                })
                .unwrap_or_default();
            lists.push(SpaceList { uuid, spaces, current });
        }
        lists
    }

    /// Display UUID → its current Space id, for every display with its own Spaces. "id64" is
    /// read first, "ManagedSpaceID" as the fallback, as the Swift read them.
    pub fn current_spaces() -> Vec<(String, u64)> {
        let id64 = CFString::from_str("id64");
        let managed = CFString::from_str("ManagedSpaceID");
        let mut spaces = Vec::new();
        for entry in managed_display_spaces() {
            let Some(uuid) = entry_string(&entry, &CFString::from_str("Display Identifier")) else {
                continue;
            };
            let Some(current) = current_space(&entry) else { continue };
            let Some(id) = entry_u64(&current, &id64).or_else(|| entry_u64(&current, &managed))
            else {
                continue;
            };
            spaces.push((uuid, id));
        }
        spaces
    }
}

/// One display's Spaces in order, and the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceList {
    /// The display's UUID, as [`SystemDisplays::uuid`] reports it.
    pub uuid: String,
    /// Every Space's id, in the window server's order.
    pub spaces: Vec<u64>,
    /// The id of the Space the display shows now.
    pub current: u64,
}
