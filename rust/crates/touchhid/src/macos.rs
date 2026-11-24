//! macOS backend: IOKit HID.
//!
//! Mirrors the proven sequence from the Swift app's `HIDTouchSource`: match the
//! panel's USB device by exact vendor/product id (every HID interface, so the
//! mouse-emulation interface is seized too), open each interface with
//! `kIOHIDOptionsTypeSeizeDevice`, write the Input Mode unlock on the digitizer
//! interface **before** registering input reports (afterwards the interface
//! stays silent), then register, schedule and activate — all on a worker
//! thread, not the manager's enumeration thread.
//!
//! Ownership is process-lifetime by design: the driver, the shared state and
//! the per-interface report buffers live until the process exits, and the
//! kernel releases the seize then. A daemon owns exactly one driver.

use std::ffi::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

use crate::RawReport;

type Ref = *mut c_void;
type KernReturn = i32;

const KCF_ALLOCATOR_DEFAULT: Ref = std::ptr::null_mut();
const KCF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const KCF_NUMBER_SINT32_TYPE: isize = 3;
const KIO_HID_OPTIONS_TYPE_SEIZE_DEVICE: u32 = 0x01;
const KIO_HID_REPORT_TYPE_FEATURE: u32 = 0x02;
const KIO_RETURN_SUCCESS: KernReturn = 0;

#[repr(C)]
struct CFDictionaryKeyCallBacks {
    version: isize,
    retain: Option<extern "C" fn(Ref, Ref) -> Ref>,
    release: Option<extern "C" fn(Ref, Ref)>,
    copy_description: Option<extern "C" fn(Ref) -> Ref>,
    equal: Option<extern "C" fn(Ref, Ref) -> bool>,
    hash: Option<extern "C" fn(Ref) -> usize>,
}

#[repr(C)]
struct CFDictionaryValueCallBacks {
    version: isize,
    retain: Option<extern "C" fn(Ref, Ref) -> Ref>,
    release: Option<extern "C" fn(Ref, Ref)>,
    copy_description: Option<extern "C" fn(Ref) -> Ref>,
    equal: Option<extern "C" fn(Ref, Ref) -> bool>,
}

#[link(name = "CoreFoundation", kind = "framework")]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;

    fn CFStringCreateWithCString(allocator: Ref, c_string: *const u8, encoding: u32) -> Ref;
    fn CFNumberCreate(allocator: Ref, number_type: isize, value: *const c_void) -> Ref;
    fn CFDictionaryCreateMutable(allocator: Ref, capacity: isize, key_callbacks: *const CFDictionaryKeyCallBacks, value_callbacks: *const CFDictionaryValueCallBacks) -> Ref;
    fn CFDictionarySetValue(dictionary: Ref, key: Ref, value: Ref);
    fn CFNumberGetValue(number: Ref, number_type: isize, value: *mut c_void) -> bool;
    fn CFRelease(cf: Ref);

    fn dispatch_queue_create(label: *const u8, attr: Ref) -> Ref;

    fn IOHIDManagerCreate(allocator: Ref, options: u32) -> Ref;
    fn IOHIDManagerSetDeviceMatching(manager: Ref, matching: Ref);
    fn IOHIDManagerRegisterDeviceMatchingCallback(manager: Ref, callback: extern "C" fn(*mut c_void, KernReturn, *mut c_void, Ref), context: *mut c_void);
    fn IOHIDManagerSetDispatchQueue(manager: Ref, queue: Ref);
    fn IOHIDManagerActivate(manager: Ref);
    fn IOHIDDeviceCreate(allocator: Ref, service: u32) -> Ref;
    fn IOHIDDeviceGetService(device: Ref) -> u32;
    fn IOHIDDeviceGetProperty(device: Ref, key: Ref) -> Ref;
    fn IOHIDDeviceOpen(device: Ref, options: u32) -> KernReturn;
    fn IOHIDDeviceSetReport(device: Ref, report_type: u32, report_id: isize, report: *const u8, report_length: isize) -> KernReturn;
    fn IOHIDDeviceRegisterInputReportCallback(device: Ref, report: *mut u8, report_length: isize, callback: extern "C" fn(*mut c_void, KernReturn, *mut c_void, u32, u32, *mut u8, isize), context: *mut c_void);
    fn IOHIDDeviceSetDispatchQueue(device: Ref, queue: Ref);
    fn IOHIDDeviceActivate(device: Ref);
}

/// The state the C callbacks reach through their context pointer. Allocated
/// once per driver and alive for the process lifetime.
struct Shared {
    sender: Mutex<Sender<RawReport>>,
    attaches: Mutex<Sender<(u32, SharedPtr)>>,
    queue: Ref,
}

/// The shared-state pointer, made sendable for the attach channel: the pointee
/// lives for the process lifetime and every field is behind a mutex.
#[derive(Clone, Copy)]
struct SharedPtr(*mut Shared);

unsafe impl Send for SharedPtr {}

/// A running panel capture. Raw input reports arrive on the receiver; the
/// digitizer interface stays silent until the unlock lands, and the
/// mouse-emulation interface reports in the meantime. Dropping the driver is
/// not supported: stop by exiting the process.
pub struct Driver {
    _manager: Ref,
    _shared: *mut Shared,
}

// The IOKit objects are thread-safe once scheduled; `Shared` is behind a mutex.
unsafe impl Send for Driver {}
unsafe impl Sync for Driver {}

impl Driver {
    /// Seizes every HID interface of the given USB device, performs the mode
    /// unlock, and returns the driver with its report receiver.
    pub fn start(vendor_id: u16, product_id: u16) -> Result<(Driver, Receiver<RawReport>), String> {
        let queue = unsafe { dispatch_queue_create(c"touchhid".as_ptr().cast(), std::ptr::null_mut()) };
        if queue.is_null() {
            return Err(String::from("dispatch_queue_create failed"));
        }

        let (sender, receiver) = channel();
        let (attach_sender, attach_receiver) = channel::<(u32, SharedPtr)>();
        let shared = Box::into_raw(Box::new(Shared {
            sender: Mutex::new(sender),
            attaches: Mutex::new(attach_sender),
            queue,
        }));

        // Attachment happens on its own thread, mirroring the Swift app, which
        // hops off the manager's enumeration thread before opening devices and
        // registering reports.
        std::thread::spawn(move || {
            for (service, shared) in attach_receiver {
                unsafe { attach(service, shared) };
            }
        });

        let manager = unsafe { IOHIDManagerCreate(KCF_ALLOCATOR_DEFAULT, 0) };
        if manager.is_null() {
            return Err(String::from("IOHIDManagerCreate failed"));
        }
        unsafe {
            let vendor_key = cf_string("VendorID");
            let product_key = cf_string("ProductID");
            let vendor_value = cf_number_i32(i32::from(vendor_id));
            let product_value = cf_number_i32(i32::from(product_id));
            let matching = CFDictionaryCreateMutable(
                KCF_ALLOCATOR_DEFAULT,
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            CFDictionarySetValue(matching, vendor_key, vendor_value);
            CFDictionarySetValue(matching, product_key, product_value);
            IOHIDManagerSetDeviceMatching(manager, matching);
            IOHIDManagerRegisterDeviceMatchingCallback(manager, device_matched, shared.cast::<c_void>());
            IOHIDManagerSetDispatchQueue(manager, queue);
            IOHIDManagerActivate(manager);
            for cf in [vendor_key, product_key, vendor_value, product_value, matching] {
                CFRelease(cf);
            }
        }

        let driver = Driver { _manager: manager, _shared: shared };
        Ok((driver, receiver))
    }
}

extern "C" fn device_matched(context: *mut c_void, _result: KernReturn, _sender: *mut c_void, matched: Ref) {
    let service = unsafe { IOHIDDeviceGetService(matched) };
    let shared = SharedPtr(context.cast::<Shared>());
    let _ = unsafe { (*shared.0).attaches.lock().unwrap().send((service, shared)) };
}

unsafe fn attach(service: u32, shared: SharedPtr) {
    let shared = shared.0;
    let device = IOHIDDeviceCreate(KCF_ALLOCATOR_DEFAULT, service);
    if device.is_null() {
        return;
    }
    let usage_page = device_property_i32(device, "PrimaryUsagePage");
    let opened = IOHIDDeviceOpen(device, KIO_HID_OPTIONS_TYPE_SEIZE_DEVICE);
    if opened != KIO_RETURN_SUCCESS {
        eprintln!("touchhid: seize failed on usage page {usage_page:?}: 0x{opened:08X}");
        return;
    }
    // The Input Mode unlock must precede input-report registration, and its
    // buffer is the payload only ([mode, index]) with the report ID in its
    // own parameter. Usage page 0x0D is the digitizer; 2 = digitizer mode.
    if usage_page == Some(0x0D) {
        let mode = [2u8, 0u8];
        let written = IOHIDDeviceSetReport(device, KIO_HID_REPORT_TYPE_FEATURE, 0x21, mode.as_ptr(), 2);
        eprintln!(
            "touchhid: input mode → digitizer: {}",
            if written == KIO_RETURN_SUCCESS { "set" } else { "write failed" }
        );
    }
    // The buffer is written by IOKit for the process lifetime; leaking it
    // intentionally keeps the pointer valid.
    let buffer = Box::into_raw(Box::new([0u8; 128]));
    IOHIDDeviceRegisterInputReportCallback(device, buffer.cast(), 128, input_report, shared.cast::<c_void>());
    IOHIDDeviceSetDispatchQueue(device, (*shared).queue);
    IOHIDDeviceActivate(device);
    eprintln!("touchhid: interface seized (usage page {usage_page:?})");
}

extern "C" fn input_report(context: *mut c_void, result: KernReturn, _sender: *mut c_void, _report_type: u32, _report_id: u32, report: *mut u8, length: isize) {
    if result != KIO_RETURN_SUCCESS {
        return;
    }
    let shared = unsafe { &*context.cast::<Shared>() };
    let len = (length.max(0) as usize).min(128);
    let mut bytes = [0u8; 128];
    bytes[..len].copy_from_slice(unsafe { std::slice::from_raw_parts(report, len) });
    let _ = shared.sender.lock().unwrap().send(RawReport { bytes, len });
}

unsafe fn cf_string(s: &str) -> Ref {
    let mut nul = s.as_bytes().to_vec();
    nul.push(0);
    CFStringCreateWithCString(KCF_ALLOCATOR_DEFAULT, nul.as_ptr(), KCF_STRING_ENCODING_UTF8)
}

unsafe fn cf_number_i32(value: i32) -> Ref {
    CFNumberCreate(KCF_ALLOCATOR_DEFAULT, KCF_NUMBER_SINT32_TYPE, &value as *const i32 as *const c_void)
}

unsafe fn device_property_i32(device: Ref, key: &str) -> Option<i32> {
    let key = cf_string(key);
    let value = IOHIDDeviceGetProperty(device, key);
    CFRelease(key);
    if value.is_null() {
        return None;
    }
    let mut out = 0i32;
    let ok = CFNumberGetValue(value, KCF_NUMBER_SINT32_TYPE, &mut out as *mut i32 as *mut c_void);
    ok.then_some(out)
}
