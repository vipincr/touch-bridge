//! The low-level HID event a real trackpad attaches to its CGEvents. AppKit's swipe tracking (Safari
//! back/forward) seems to read swipe progress from it, and synthetic scroll events carry none
//! (backlog 5). Private IOKit and SkyLight calls, looked up at runtime; every function is a no-op
//! when they are missing.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2_core_foundation::{CFRetained, CFType};
use objc2_core_graphics::CGEvent;
use touchhid::clock;

use crate::runtime_symbols::lookup;

/// The C signature of `IOHIDEventCreateScrollEvent`: allocator, timestamp, x, y, z, options.
type CreateScrollFn = unsafe extern "C" fn(*const c_void, u64, f64, f64, f64, u32) -> *mut c_void;
/// The C signature of `IOHIDEventSetPhase`: event, phase bits.
type SetPhaseFn = unsafe extern "C" fn(*const c_void, u16);
/// The C signature of `IOHIDEventSetSenderID`: event, sender id.
type SetSenderFn = unsafe extern "C" fn(*const c_void, u64);
/// The C signature of `SLEventSetIOHIDEvent`: the CGEvent, the HID event.
type AttachFn = unsafe extern "C" fn(*const c_void, *const c_void);
/// The C signature of `IOHIDEventAppendEvent`: parent, child, options.
type AppendFn = unsafe extern "C" fn(*const c_void, *const c_void, u32);
/// The C signature of `IOHIDEventCreateVendorDefinedEvent`: allocator, timestamp, usage page, usage,
/// version, bytes, length, options.
type CreateVendorFn = unsafe extern "C" fn(*const c_void, u64, u32, u32, u32, *const u8, u32, u32) -> *mut c_void;
/// The C signature of `CGEventCopyIOHIDEvent`: the CGEvent, the +1 attached HID event.
type CopyAttachedFn = unsafe extern "C" fn(*const c_void) -> *mut c_void;

static CREATE_SCROLL: OnceLock<Option<CreateScrollFn>> = OnceLock::new();
static SET_PHASE: OnceLock<Option<SetPhaseFn>> = OnceLock::new();
static SET_SENDER: OnceLock<Option<SetSenderFn>> = OnceLock::new();
static ATTACH: OnceLock<Option<AttachFn>> = OnceLock::new();
static APPEND: OnceLock<Option<AppendFn>> = OnceLock::new();
static CREATE_VENDOR: OnceLock<Option<CreateVendorFn>> = OnceLock::new();
static COPY_ATTACHED: OnceLock<Option<CopyAttachedFn>> = OnceLock::new();

/// `IOHIDEventCreateScrollEvent`, resolved once.
fn create_scroll() -> Option<CreateScrollFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"IOHIDEventCreateScrollEvent", &CREATE_SCROLL) }
}

/// `IOHIDEventSetPhase`, resolved once.
fn set_phase() -> Option<SetPhaseFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"IOHIDEventSetPhase", &SET_PHASE) }
}

/// `IOHIDEventSetSenderID`, resolved once.
fn set_sender() -> Option<SetSenderFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"IOHIDEventSetSenderID", &SET_SENDER) }
}

/// `SLEventSetIOHIDEvent`, resolved once.
fn attach() -> Option<AttachFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"SLEventSetIOHIDEvent", &ATTACH) }
}

/// `IOHIDEventAppendEvent`, resolved once.
fn append() -> Option<AppendFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"IOHIDEventAppendEvent", &APPEND) }
}

/// `IOHIDEventCreateVendorDefinedEvent`, resolved once.
fn create_vendor() -> Option<CreateVendorFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"IOHIDEventCreateVendorDefinedEvent", &CREATE_VENDOR) }
}

/// `CGEventCopyIOHIDEvent`, resolved once.
fn copy_attached() -> Option<CopyAttachedFn> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"CGEventCopyIOHIDEvent", &COPY_ATTACHED) }
}

/// IOHIDEventPhaseBits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Phase {
    None = 0,
    Began = 1,
    Changed = 2,
    Ended = 4,
    Cancelled = 8,
    MayBegin = 0x80,
}

/// Attaches the HID events a real trackpad would, to the CGEvents this driver posts.
pub struct HIDEventBridge;

impl HIDEventBridge {
    /// Whether the calls a real trackpad's events need are all here. The private set is missing on
    /// some macOS versions, and everything else here is a no-op then.
    pub fn is_available() -> bool {
        create_scroll().is_some() && set_phase().is_some() && attach().is_some()
    }

    /// The attached HID event, as IOKit describes it (type, phase, sender, fields, children); `None`
    /// if the event carries none.
    pub fn describe_attached(event: &CGEvent) -> Option<String> {
        let copy_attached = copy_attached()?;
        // SAFETY: the call follows the signature of `CGEventCopyIOHIDEvent`; a non-null result is
        // the +1 reference `owned` takes over and releases.
        let hid = owned(unsafe { copy_attached(raw_event(event)) })?;
        // `CFType`'s Debug formatting prints what `CFCopyDescription` prints.
        Some(format!("{:?}", *hid))
    }

    /// Attaches what the built-in trackpad attaches to a scroll event (seen in the Event Sink,
    /// 2026-10-11): a built-in scroll event (accelerated delta) with a child scroll event carrying
    /// the finger's travel (about a tenth of the pixel delta, flags 0x10010) and, optionally, the
    /// multitouch driver's vendor event (usage page 0xFF00, usage 0x18, 124 bytes). `dx`, `dy` are
    /// the CGEvent's pixel deltas.
    pub fn attach_trackpad_scroll(
        event: &CGEvent,
        dx: f64,
        dy: f64,
        phase: Phase,
        sender_id: u64,
        vendor_data: bool,
        gesture_underway: bool,
    ) -> bool {
        let (Some(create_scroll), Some(set_phase), Some(attach), Some(append)) =
            (create_scroll(), set_phase(), attach(), append())
        else {
            return false;
        };
        let ts = clock::now_ticks();
        let travel_x = -dx * 0.099;
        let travel_y = -dy * 0.099;
        let Some(parent) = scroll_event(create_scroll, ts, -dx * 1.4, -dy * 1.4, 0x10) else {
            return false;
        };
        let Some(child) = scroll_event(create_scroll, ts, travel_x, travel_y, 0x10010) else {
            return false;
        };
        // SAFETY: both calls follow the signature of `IOHIDEventSetPhase`.
        unsafe {
            set_phase(cf_ptr(&parent), phase as u16);
            set_phase(cf_ptr(&child), phase as u16);
        }
        if let Some(set_sender) = set_sender() {
            // SAFETY: the call follows the signature of `IOHIDEventSetSenderID`.
            unsafe { set_sender(cf_ptr(&parent), sender_id) };
        }
        // SAFETY: the call follows the signature of `IOHIDEventAppendEvent`.
        unsafe { append(cf_ptr(&parent), cf_ptr(&child), 0) };
        if vendor_data
            && let Some(create_vendor) = create_vendor() {
                let blob = vendor_blob(ts, sender_id, travel_x, travel_y, dx, dy, phase, gesture_underway);
                if let Some(vendor) = vendor_event(create_vendor, ts, &blob) {
                    // SAFETY: the call follows the signature of `IOHIDEventAppendEvent`.
                    unsafe { append(cf_ptr(&parent), cf_ptr(&vendor), 0) };
                }
            }
        // SAFETY: the call follows the signature of `SLEventSetIOHIDEvent`.
        unsafe { attach(raw_event(event), cf_ptr(&parent)) };
        true
    }

    /// Attaches a HID scroll event (`x`, `y` in the device's units) with `phase` to `event`.
    pub fn attach_scroll(event: &CGEvent, x: f64, y: f64, phase: Phase, sender_id: Option<u64>) -> bool {
        let (Some(create_scroll), Some(set_phase), Some(attach)) = (create_scroll(), set_phase(), attach()) else {
            return false;
        };
        let Some(hid) = scroll_event(create_scroll, clock::now_ticks(), x, y, 0) else {
            return false;
        };
        // SAFETY: the call follows the signature of `IOHIDEventSetPhase`.
        unsafe { set_phase(cf_ptr(&hid), phase as u16) };
        if let (Some(set_sender), Some(sender_id)) = (set_sender(), sender_id) {
            // SAFETY: the call follows the signature of `IOHIDEventSetSenderID`.
            unsafe { set_sender(cf_ptr(&hid), sender_id) };
        }
        // SAFETY: the call follows the signature of `SLEventSetIOHIDEvent`.
        unsafe { attach(raw_event(event), cf_ptr(&hid)) };
        true
    }
}

/// The CGEventRef behind a bound event, for the calls that take it as a plain pointer.
fn raw_event(event: &CGEvent) -> *const c_void {
    std::ptr::from_ref(event).cast::<c_void>()
}

/// The CFTypeRef behind an owned Core Foundation value.
fn cf_ptr(value: &CFRetained<CFType>) -> *mut c_void {
    CFRetained::as_ptr(value).cast::<c_void>().as_ptr()
}

/// Takes over the +1 reference a Create call handed back; `None` when the call failed.
fn owned(pointer: *mut c_void) -> Option<CFRetained<CFType>> {
    let pointer = NonNull::new(pointer)?;
    // SAFETY: the caller passes a pointer a Create call returned, whose +1 reference `from_raw`
    // takes over and releases when the value drops.
    Some(unsafe { CFRetained::from_raw(pointer.cast::<CFType>()) })
}

/// A scroll HID event stamped `ts`, with `x`, `y` travel and `options` flags; the +1 result.
fn scroll_event(create: CreateScrollFn, ts: u64, x: f64, y: f64, options: u32) -> Option<CFRetained<CFType>> {
    // SAFETY: the call follows the signature of `IOHIDEventCreateScrollEvent`; a non-null result is
    // the +1 reference `owned` takes over.
    unsafe { owned(create(std::ptr::null(), ts, x, y, 0.0, options)) }
}

/// The vendor-defined event carrying the multitouch driver's 124-byte blob; the +1 result.
fn vendor_event(create: CreateVendorFn, ts: u64, blob: &[u8; 124]) -> Option<CFRetained<CFType>> {
    // SAFETY: the call follows the signature of `IOHIDEventCreateVendorDefinedEvent`; `blob` lives
    // across the call, and a non-null result is the +1 reference `owned` takes over.
    unsafe { owned(create(std::ptr::null(), ts, 0xFF00, 0x18, 0, blob.as_ptr(), blob.len() as u32, 0x1)) }
}

/// The vendor blob's fixed-point travel value: hundredths of a unit scaled into 16.16 bits, as the
/// Swift lays it down.
fn fixed(value: f64) -> i32 {
    (((value * 10.0).round() / 10.0) * 65536.0) as i32
}

/// Writes `value` little-endian at `offset`, as the Swift blob builder does.
fn put<const N: usize>(blob: &mut [u8; 124], offset: usize, value: [u8; N]) {
    blob[offset..offset + N].copy_from_slice(&value);
}

/// The 124 bytes the multitouch driver's vendor event carries, laid out exactly as the Swift lays
/// them down (little endian, the same offsets).
#[allow(clippy::too_many_arguments)]
fn vendor_blob(
    ts: u64,
    sender_id: u64,
    travel_x: f64,
    travel_y: f64,
    dx: f64,
    dy: f64,
    phase: Phase,
    gesture_underway: bool,
) -> [u8; 124] {
    let mut blob = [0u8; 124];
    put(&mut blob, 0, 22i32.to_le_bytes());
    put(&mut blob, 12, ts.to_le_bytes());
    put(&mut blob, 28, sender_id.to_le_bytes());
    put(&mut blob, 40, (travel_y.round() as i16).to_le_bytes());
    put(&mut blob, 42, (travel_x.round() as i16).to_le_bytes());
    blob[46] = 0x09;
    blob[47] = phase as u8;
    put(&mut blob, 48, fixed(travel_y).to_le_bytes());
    put(&mut blob, 52, fixed(travel_x).to_le_bytes());
    put(&mut blob, 60, ((-(dy.round())) as i32).to_le_bytes());
    put(&mut blob, 64, ((-(dx.round())) as i32).to_le_bytes());
    if gesture_underway {
        put(&mut blob, 76, 1i32.to_le_bytes());
    }
    put(&mut blob, 80, (sender_id as u32).to_le_bytes());
    put(&mut blob, 88, 1i32.to_le_bytes());
    blob
}
