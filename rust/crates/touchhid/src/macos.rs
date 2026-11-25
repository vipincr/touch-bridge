//! The macOS backend: IOKit HID (Swift: `HIDTouchSource`).
//!
//! Finds the touch panel, opens it exclusively (seize) and delivers raw input reports. Two matchers
//! run side by side: any USB touch-screen digitizer by HID class (Apple's own devices and WCH panels
//! excepted), and, because a WCH panel's touch data on macOS arrives on its mouse interface, every
//! HID interface of the exact WCH USB device, so that interface is seized too and macOS stops turning
//! touches into cursor movement itself.
//!
//! IOHIDManager is used only for matching. For each match the source creates its own IOHIDDevice,
//! because the manager's device objects are already activated and IOKit requires every "Register"
//! call to happen before activation. All device callbacks run on the source's serial queue.
//!
//! Lifetime rule: every allocation a callback can reach (each device's report buffer, the shared
//! state) is released only from IOKit's cancel handler for that object, which runs on the queue after
//! the last callback, and the manager objects outlive their cancel handler. This is the same
//! discipline the Swift original follows in its two cancel handlers.

use std::ffi::{c_void, CStr};
use std::mem::MaybeUninit;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2_core_foundation::{CFDictionary, CFIndex, CFNumber, CFRetained, CFString};
use objc2_io_kit::{
    kHIDPage_Digitizer, kHIDUsage_Dig_TouchScreen, kIOHIDOptionsTypeNone, kIOHIDOptionsTypeSeizeDevice,
    kIOHIDPrimaryUsageKey, kIOHIDPrimaryUsagePageKey, kIOHIDProductIDKey, kIOHIDProductKey, kIOHIDVendorIDKey,
    kIOReturnExclusiveAccess, kIOReturnNotPermitted, kIOReturnSuccess, IOHIDDevice, IOHIDManager, IOHIDReportType,
    IOReturn,
};

use crate::{PanelEvent, PanelSink, RawReport};

/// ILITEK panels (Swift: `ilitekVendorID`).
const ILITEK_VENDOR_ID: u16 = 0x222A;
/// WCH panels, vendor (Swift: `wchVendorID`).
const WCH_VENDOR_ID: u16 = 0x27C0;
/// WCH panels, product (Swift: `wchProductID`).
const WCH_PRODUCT_ID: u16 = 0x0859;
/// Apple's own HID devices, excluded from the class matcher (Swift: `appleVendorID`).
const APPLE_VENDOR_ID: u16 = 0x05AC;
/// Feature report that selects the input mode (Swift: `inputModeReportID`).
const INPUT_MODE_REPORT_ID: u8 = 0x21;
/// The digitizer value for the input mode (Swift: `inputModeDigitizer`).
const INPUT_MODE_DIGITIZER: u8 = 0x02;
/// Feature report 0x0A, contact count maximum. Windows reads it before setting the input mode
/// (Swift: `contactMaximumReportID`).
const CONTACT_MAXIMUM_REPORT_ID: u8 = 0x0A;
/// Length of the contact-count maximum report (Swift: `contactMaximumReportLength`).
const CONTACT_MAXIMUM_REPORT_LENGTH: usize = 2;
/// Preallocated buffer each input-report callback writes into (Swift: `reportBufferSize`).
const REPORT_BUFFER_SIZE: usize = 128;

/// The queue label, kept from the Swift app.
const QUEUE_LABEL: &str = "com.gramini.touchnext.hid";

/// Everything the callbacks and the consumer share. The callbacks reach it through a strong
/// reference whose release is tied to the IOKit object they were registered with (Swift passes
/// `self` unretained and relies on the source outliving its objects).
struct Shared {
    /// Panel state, guarded by the mutex. The IOKit calls that read or write it happen on `queue`,
    /// exactly where Swift touches them.
    inner: Mutex<Inner>,
    /// The serial queue every IOKit object is registered, activated and cancelled on, and the queue
    /// the callbacks arrive on.
    queue: DispatchRetained<DispatchQueue>,
}

impl Shared {
    /// Locks the panel state. A poisoned lock means a thread panicked while holding it; Swift has no
    /// poison and would have kept going, so recover the data.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `work` on the queue and waits for its result. This is the synchronous hop from a
    /// consumer thread onto the panel's queue, which replaces Swift's `queue.sync`.
    ///
    /// Calling it from the queue itself would deadlock; the callbacks never do, they only push into
    /// the sink.
    fn sync<R: Send>(shared: &Arc<Shared>, work: impl FnOnce() -> R + Send) -> R {
        let mut result = MaybeUninit::uninit();
        // SAFETY: the pointer targets the caller's stack slot above. `exec_sync` runs the closure on
        // the queue and does not return until the closure has finished, so the slot is initialized
        // before the `assume_init` below and the stack frame outlives the write.
        let slot = SendPtr(result.as_mut_ptr());
        shared.queue.exec_sync(move || {
            // SAFETY: the queue runs this closure exactly once, before `exec_sync` returns, and the
            // caller waits for it.
            unsafe { slot.write(work()) };
        });
        // SAFETY: the closure wrote the value before `exec_sync` returned.
        unsafe { result.assume_init() }
    }
}

/// A raw pointer a queued closure may write through.
struct SendPtr<T>(*mut T);
// SAFETY: only used by `Shared::sync`, which runs the closure synchronously on the queue while the
// pointer's stack frame is still alive.
unsafe impl<T> Send for SendPtr<T> {}

impl<T> SendPtr<T> {
    unsafe fn write(self, value: T) {
        // SAFETY: the caller proves the target is valid and unwritten.
        unsafe { self.0.write(value) };
    }
}

/// The panel state (Swift's `managers`, `devices` and `wchDigitizer`, held under one mutex instead
/// of relying on queue discipline).
struct Inner {
    /// Active managers. Fresh per `start`: a cancelled IOHIDManager cannot be registered or activated
    /// again (Swift: "Disable → Enable" crashed before this). Only touched by start and stop.
    managers: Vec<CFRetained<IOHIDManager>>,
    /// Seized devices, in attachment order. `read_feature_report` reads from the first.
    devices: Vec<CFRetained<IOHIDDevice>>,
    /// The WCH digitizer interface, for re-asking when the panel slips back to mouse mode.
    wch_digitizer: Option<CFRetained<IOHIDDevice>>,
    /// True once the panel has matched as a HID device (whether or not the seize succeeded).
    matched_any: bool,
    /// The current run's sink, replaced by each `start`.
    events: Option<PanelSink>,
}

impl Inner {
    /// Delivers one event to the sink (Swift: `onReport?` and `onStatus?`).
    fn send_event(&self, event: PanelEvent) {
        if let Some(sink) = &self.events {
            sink(event);
        }
    }

    /// Delivers one status line (Swift: `onStatus?`).
    fn send_status(&self, text: String) {
        self.send_event(PanelEvent::Status(text));
    }
}

// SAFETY: the CF device and manager references cross threads only inside the mutex, and after
// activation every IOKit call on them happens on the source's serial queue (Swift: devices are
// "only touched on `queue`" and managers "only touched by start/stop"). CoreFoundation refcounts are
// atomic. This is the assertion Swift makes with `@unchecked Sendable` on DeviceBox and ManagerBox.
unsafe impl Send for Inner {}

/// Tells a manager's matching callback which vendor filter applies. Owned by a `Box` whose pointer
/// is the callback context; the manager's cancel handler releases it (Swift: `MatchBox`, owned by
/// `boxes`).
struct MatchCtx {
    /// The shared state. This reference is what keeps it alive for as long as the manager can still
    /// call back.
    shared: Arc<Shared>,
    wch_only: bool,
}

/// Carries a CF device object across the hop onto the HID queue (Swift: `DeviceBox`).
struct DeviceBox(CFRetained<IOHIDDevice>);
// SAFETY: CoreFoundation refcounts are atomic, and after the hop the device is only touched on the
// source's queue, where `attach` runs.
unsafe impl Send for DeviceBox {}

/// Finds the touch panel, opens it exclusively (seize) and delivers raw input reports and status
/// lines to the sink given to [`start`](Self::start).
pub struct HidTouchSource {
    /// The state shared with the callbacks. Behind an `Arc` because the callbacks must reach it after
    /// `start` has returned, until each object's cancel handler runs.
    shared: Arc<Shared>,
}

impl Default for HidTouchSource {
    fn default() -> Self {
        Self::new()
    }
}

impl HidTouchSource {
    /// Creates the source with its serial queue, without touching the panel. Nothing happens until
    /// `start`.
    pub fn new() -> Self {
        // Swift: DispatchQueue(label: "com.gramini.touchnext.hid", qos: .userInteractive). The
        // attribute makes it serial; the QoS floor gives it the user-interactive priority.
        let queue = DispatchQueue::new(QUEUE_LABEL, DispatchQueueAttr::SERIAL);
        let _ = queue.set_qos_class_floor(DispatchQoS::UserInteractive, 0);
        Self {
            shared: Arc::new(Shared {
                inner: Mutex::new(Inner {
                    managers: Vec::new(),
                    devices: Vec::new(),
                    wch_digitizer: None,
                    matched_any: false,
                    events: None,
                }),
                queue,
            }),
        }
    }

    /// Finds the panel, seizes its interfaces and starts delivering events to `sink`.
    ///
    /// Every `start` builds fresh managers: a cancelled IOHIDManager cannot be registered or
    /// activated again. Called while already running, it is a no-op and `sink` is dropped (Swift
    /// returns early the same way); call `stop` first for the restart self-test.
    pub fn start(&mut self, sink: impl Fn(PanelEvent) + Send + 'static) {
        {
            let mut inner = self.shared.lock();
            if !inner.managers.is_empty() {
                return;
            }
            inner.events = Some(Box::new(sink));
        }
        let digitizer = digitizer_matching_dictionary();
        let wch = wch_matching_dictionary();
        // The class matcher runs the callback for devices already present on the activating thread,
        // so `make_manager` may report before this function returns; that is the Swift order too.
        let digitizer = make_manager(
            &self.shared,
            digitizer,
            false,
            "any USB touch-screen digitizer except Apple and WCH",
        );
        let wch = make_manager(
            &self.shared,
            wch,
            true,
            "WCH TouchScreen, all interfaces (touch data arrives on the mouse interface)",
        );
        let mut inner = self.shared.lock();
        inner.managers.push(digitizer);
        inner.managers.push(wch);
    }

    /// Closes every seized device, which hands the panel back to macOS, and cancels devices and
    /// managers, leaving the source ready for a new `start`. The cancel handlers then run on the
    /// queue and release the buffers and callback contexts.
    pub fn stop(&mut self) {
        Shared::sync(&self.shared, || {
            let mut inner = self.shared.lock();
            for device in inner.devices.drain(..) {
                device.close(kIOHIDOptionsTypeSeizeDevice);
                device.cancel();
            }
            inner.wch_digitizer = None;
            for manager in &inner.managers {
                manager.cancel();
            }
            inner.managers.clear();
            inner.send_status("stopped; panel released".into());
        });
    }

    /// True once the panel has matched as a HID device (whether or not the seize succeeded).
    pub fn has_matched_device(&self) -> bool {
        self.shared.lock().matched_any
    }

    /// Reads a feature report (e.g. RID 2 contact-count max, RID 6 vendor blob) from the first
    /// seized device. Read-only. Returns `None` when no device is seized; a failed read is reported
    /// through the status stream.
    ///
    /// Call from the consumer's thread: the call hops onto the panel's queue and back, which replaces
    /// Swift's `queue.sync`.
    pub fn read_feature_report(&self, id: u8, length: usize) -> Option<Vec<u8>> {
        Shared::sync(&self.shared, move || {
            let inner = self.shared.lock();
            let device = inner.devices.first()?;
            let mut bytes = vec![0u8; length];
            let mut size = length as CFIndex;
            // SAFETY: `bytes` is a valid buffer of `size` bytes and `size` doubles as the in/out
            // length pointer the binding requires.
            let result = unsafe {
                device.report(
                    IOHIDReportType::Feature,
                    id as CFIndex,
                    NonNull::from(&mut bytes).cast::<u8>(),
                    NonNull::from(&mut size),
                )
            };
            if result != kIOReturnSuccess {
                inner.send_status(format!("feature report {id} read failed: {}", describe(result)));
                return None;
            }
            let end = (size.max(0) as usize).min(bytes.len());
            bytes.truncate(end);
            Some(bytes)
        })
    }

    /// Reads feature reports 2 (contact-count maximum) and 6 (vendor blob) from every seized ILITEK
    /// panel, and re-asks a WCH panel that has been seen for digitizer mode. Linux's hid-multitouch
    /// reads the blob "to enable some devices": after a power cycle the panel sends single-touch
    /// mouse reports until it is read. Other controllers are left alone.
    ///
    /// Call from the consumer's thread: the call hops onto the panel's queue, which replaces Swift's
    /// `dispatchPrecondition`. The report callbacks only push events into the sink, so a consumer
    /// blocked in this hop cannot deadlock.
    pub fn request_multitouch_mode(&self) {
        Shared::sync(&self.shared, || {
            let inner = self.shared.lock();
            request_multitouch_mode_on_queue(&inner);
        });
    }
}

impl Drop for HidTouchSource {
    /// A source dropped while running would leave its interfaces seized and its IOKit objects
    /// unreleased for the rest of the process's life, so it is stopped first.
    fn drop(&mut self) {
        if !self.shared.lock().managers.is_empty() {
            self.stop();
        }
    }
}

/// The HID-class matcher: any USB touch-screen digitizer (Swift: `start`'s first matching
/// dictionary).
fn digitizer_matching_dictionary() -> CFRetained<CFDictionary<CFString, CFNumber>> {
    let page_key = key_string(kIOHIDPrimaryUsagePageKey);
    let usage_key = key_string(kIOHIDPrimaryUsageKey);
    let page = CFNumber::new_i32(kHIDPage_Digitizer as i32);
    let usage = CFNumber::new_i32(kHIDUsage_Dig_TouchScreen as i32);
    CFDictionary::<CFString, CFNumber>::from_slices(&[&page_key, &usage_key], &[&page, &usage])
}

/// The exact-device matcher: every HID interface of the WCH panel, so the mouse-emulation interface
/// is seized too (Swift: `start`'s second matching dictionary).
fn wch_matching_dictionary() -> CFRetained<CFDictionary<CFString, CFNumber>> {
    let vendor_key = key_string(kIOHIDVendorIDKey);
    let product_key = key_string(kIOHIDProductIDKey);
    let vendor = CFNumber::new_i32(i32::from(WCH_VENDOR_ID));
    let product = CFNumber::new_i32(i32::from(WCH_PRODUCT_ID));
    CFDictionary::<CFString, CFNumber>::from_slices(&[&vendor_key, &product_key], &[&vendor, &product])
}

/// Builds one manager with its matching dictionary, callback, queue, cancel handler, and activates it
/// (Swift: `makeManager`).
fn make_manager(
    shared: &Arc<Shared>,
    matching: CFRetained<CFDictionary<CFString, CFNumber>>,
    wch_only: bool,
    note: &str,
) -> CFRetained<IOHIDManager> {
    let manager = IOHIDManager::new(None, kIOHIDOptionsTypeNone);
    // The binding takes the untyped dictionary view; the cast only forgets the key and value types,
    // which are CFString and CFNumber as IOKit requires.
    let matching: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(matching) };
    // SAFETY: the dictionary holds CFType keys and values, as the binding requires.
    unsafe { manager.set_device_matching(Some(&matching)) };
    // The matching callback and the manager's cancel handler share the context box; each holds the
    // pointer, and the cancel handler is the one that releases it.
    let match_ctx: *mut MatchCtx = Box::into_raw(Box::new(MatchCtx {
        shared: Arc::clone(shared),
        wch_only,
    }));
    // SAFETY: the context stays valid until the manager's cancel handler releases it, and the
    // callback only reads it.
    unsafe { manager.register_device_matching_callback(Some(device_matched), match_ctx.cast()) };
    // SAFETY: dispatch queues are thread-safe, and the queue outlives the manager (the cancel
    // handler below holds the shared state it belongs to).
    unsafe { manager.set_dispatch_queue(&shared.queue) };
    // IOKit releases the manager only after it is cancelled and this handler has run, so the handler
    // is where the context box and the manager's own strong reference are dropped (Swift:
    // ManagerBox).
    let keep_alive = CFRetained::into_raw(manager.clone());
    let block = RcBlock::new(move || {
        // SAFETY: this block runs once, after the manager's last callback, so the context box has no
        // other reader; its +1 comes from `Box::into_raw` above.
        drop(unsafe { Box::from_raw(match_ctx) });
        // SAFETY: takes over the +1 that `into_raw` left on the manager.
        drop(unsafe { CFRetained::from_raw(keep_alive) });
    });
    // SAFETY: the block's +1 transfers to IOKit, which releases it after the handler has run (the
    // ownership pattern from IOHIDManager.h).
    unsafe { manager.set_cancel_handler(RcBlock::into_raw(block)) };
    manager.activate();
    shared.lock().send_status(format!("matching started ({note})"));
    manager
}

/// The manager's matching callback. Runs on the thread that activated the manager (for devices
/// already present) or on the queue (hotplug), so all further work hops onto the queue (Swift:
/// `queue.async { attach }`).
unsafe extern "C-unwind" fn device_matched(
    ctx: *mut c_void,
    _result: IOReturn,
    _sender: *mut c_void,
    matched: NonNull<IOHIDDevice>,
) {
    // SAFETY: borrow only; the context is owned by the manager's cancel handler, which is alive for
    // as long as this callback can fire.
    let ctx: &MatchCtx = unsafe { &*ctx.cast::<MatchCtx>() };
    // The manager's device objects are already activated, and IOKit requires every register call
    // before activation, so the source creates its own device for the same service.
    // SAFETY: IOKit passes a valid device for the duration of the callback.
    let service = unsafe { matched.as_ref() }.service();
    let Some(device) = IOHIDDevice::new(None, service) else {
        return;
    };
    let Some(vendor) = vendor_of(&device) else {
        return;
    };
    if ctx.wch_only {
        if vendor != WCH_VENDOR_ID {
            return;
        }
    } else if vendor == APPLE_VENDOR_ID || vendor == WCH_VENDOR_ID {
        return;
    }
    // The callback only borrows the context, and the manager's cancel handler may release it once
    // this returns; the queued work needs its own strong reference.
    let shared = Arc::clone(&ctx.shared);
    let queue = shared.queue.clone();
    let boxed = DeviceBox(device);
    queue.exec_async(move || attach(&shared, boxed));
}

/// Opens and activates one matched device on the queue (Swift: `attach`).
fn attach(shared: &Arc<Shared>, boxed: DeviceBox) {
    let device = boxed.0;
    let mut inner = shared.lock();
    inner.matched_any = true;
    let vendor = vendor_of(&device).unwrap_or(0);
    let pid = number_u16(&device, kIOHIDProductIDKey).unwrap_or(0);
    let product = product_of(&device).unwrap_or_else(|| "unknown product".into());
    let page = number_u32(&device, kIOHIDPrimaryUsagePageKey).unwrap_or(0);
    inner.send_status(format!(
        "panel matched: {product} (VID 0x{vendor:04X} PID 0x{pid:04X}, usage page {page})"
    ));
    let result = device.open(kIOHIDOptionsTypeSeizeDevice);
    if result != kIOReturnSuccess {
        inner.send_status(format!("seize failed: {}", describe(result)));
        return;
    }
    let digitizer = page == u32::from(kHIDPage_Digitizer);
    if digitizer && vendor == WCH_VENDOR_ID {
        inner.wch_digitizer = Some(device.clone());
    }
    // The input-report callback writes this buffer on the queue for as long as the device is active.
    // Its +1 transfers to IOKit and the device's cancel handler releases it after the last callback
    // (Swift: `IOHIDDeviceSetCancelHandler(device) { buffer.deallocate() }`).
    let buffer: *mut [u8; REPORT_BUFFER_SIZE] = Box::into_raw(Box::new([0u8; REPORT_BUFFER_SIZE]));
    // SAFETY: `buffer` came from `Box::into_raw` above and is non-null.
    let report_ptr = NonNull::from(unsafe { &mut *buffer }).cast::<u8>();
    // The device's callbacks borrow the shared state through this pointer; each carries its own
    // strong reference, which the device's cancel handler releases after the last callback.
    let ctx: *mut Shared = Arc::into_raw(Arc::clone(shared)).cast_mut();
    // SAFETY: the buffer stays allocated until the cancel handler runs, the callbacks only touch it
    // on the queue, and `ctx` is valid for the same span. Registers must precede activation.
    unsafe {
        device.register_input_report_with_time_stamp_callback(
            report_ptr,
            REPORT_BUFFER_SIZE as CFIndex,
            Some(input_report),
            ctx.cast(),
        );
        device.register_removal_callback(Some(device_removed), ctx.cast());
        device.set_dispatch_queue(&shared.queue);
    }
    let block = RcBlock::new(move || {
        // SAFETY: IOKit runs this handler on the queue after the device's last callback, so the buffer
        // and the context have no other reader; both carry their own +1 from registration.
        drop(unsafe { Box::from_raw(buffer) });
        drop(unsafe { Arc::from_raw(ctx) });
    });
    // SAFETY: the block's +1 transfers to IOKit (the pattern from IOHIDDevice.h).
    unsafe { device.set_cancel_handler(RcBlock::into_raw(block)) };
    device.activate();
    inner.devices.push(device.clone());
    inner.send_status("panel seized".into());
    if digitizer && vendor == WCH_VENDOR_ID {
        // Now that the interrupt pipe is armed (activation), ask for multitouch the way Windows does
        // on this controller (USBPcap, Xeneon Edge, same VID:PID): read the contact-count maximum,
        // then set the input mode.
        request_digitizer_mode(&device, &inner);
    }
    if digitizer && vendor == ILITEK_VENDOR_ID {
        // After a power cycle (unplug, sleep) the panel starts in single-touch mouse mode.
        request_multitouch_mode_on_queue(&inner);
    }
}

/// The timestamped input-report callback. Runs on the queue; pushes the raw report into the sink
/// and never calls back into the source, so a consumer blocked in a synchronous hop cannot deadlock.
unsafe extern "C-unwind" fn input_report(
    ctx: *mut c_void,
    result: IOReturn,
    _sender: *mut c_void,
    _report_type: IOHIDReportType,
    _report_id: u32,
    report: NonNull<u8>,
    report_length: CFIndex,
    timestamp: u64,
) {
    if result != kIOReturnSuccess {
        return;
    }
    // SAFETY: borrow only; the +1 stays with the device's cancel handler.
    let shared: &Shared = unsafe { &*ctx.cast::<Shared>() };
    let length = (report_length.max(0) as usize).min(REPORT_BUFFER_SIZE);
    // SAFETY: IOKit fills at most `report_length` bytes of the buffer that was registered with this
    // callback.
    let bytes = unsafe { std::slice::from_raw_parts(report.as_ptr(), length) }.to_vec();
    shared.lock().send_event(PanelEvent::Report(RawReport {
        timestamp_ns: crate::clock::nanoseconds(timestamp),
        bytes,
    }));
}

/// The device removal callback. Runs on the queue; IOKit passes the device being removed as the
/// `sender` (Swift: `detach(Unmanaged<IOHIDDevice>.fromOpaque(sender)...)`).
unsafe extern "C-unwind" fn device_removed(ctx: *mut c_void, _result: IOReturn, sender: *mut c_void) {
    // SAFETY: borrow only; the +1 stays with the device's cancel handler.
    let shared: &Shared = unsafe { &*ctx.cast::<Shared>() };
    // SAFETY: the sender is the device being removed, valid for the call.
    detach(shared, unsafe { &*sender.cast::<IOHIDDevice>() });
}

/// Forgets a removed device: drops the WCH digitizer if it was one, cancels it, and reports (Swift:
/// `detach`).
fn detach(shared: &Shared, device: &IOHIDDevice) {
    let mut inner = shared.lock();
    // The removal callback hands back the very object that was registered, so pointer identity is
    // the equality Swift's `CFEqual` computes here.
    let same = |stored: &CFRetained<IOHIDDevice>| std::ptr::eq(&**stored, device);
    if inner.wch_digitizer.as_ref().is_some_and(same) {
        inner.wch_digitizer = None;
    }
    let Some(index) = inner.devices.iter().position(same) else {
        return;
    };
    let removed = inner.devices.remove(index);
    removed.cancel();
    inner.send_status("panel removed".into());
}

/// Writes Input Mode = digitizer (feature report 0x21), after reading the contact-count maximum
/// (feature report 0x0A) as Windows does. Each step is reported, because the write returning success
/// says nothing about whether the panel obeyed it (the firmware accepts the request either way); the
/// report census in the pipeline is what shows the result. Call on the queue with the state locked
/// (Swift: private, called on `queue`).
fn request_digitizer_mode(device: &IOHIDDevice, inner: &Inner) {
    let mut maximum = [0u8; CONTACT_MAXIMUM_REPORT_LENGTH];
    let mut size = CONTACT_MAXIMUM_REPORT_LENGTH as CFIndex;
    // SAFETY: valid buffer and in/out length pointer, as the binding requires.
    let read = unsafe {
        device.report(
            IOHIDReportType::Feature,
            CONTACT_MAXIMUM_REPORT_ID as CFIndex,
            NonNull::from(&mut maximum).cast::<u8>(),
            NonNull::from(&mut size),
        )
    };
    let read_text = if read == kIOReturnSuccess {
        let end = (size.max(0) as usize).min(maximum.len());
        hex(&maximum[..end])
    } else {
        format!("read failed: {}", describe(read))
    };
    inner.send_status(format!(
        "contact-count maximum (feature 0x{0:X}): {read_text}",
        CONTACT_MAXIMUM_REPORT_ID
    ));
    let mode = [INPUT_MODE_DIGITIZER, 0];
    // SAFETY: a valid two-byte report; the report ID travels in its own parameter, as the binding
    // documents.
    let result = unsafe {
        device.set_report(
            IOHIDReportType::Feature,
            INPUT_MODE_REPORT_ID as CFIndex,
            NonNull::from(&mode).cast::<u8>(),
            mode.len() as CFIndex,
        )
    };
    inner.send_status(format!(
        "input mode → digitizer (feature 0x{0:X}: {1})",
        INPUT_MODE_REPORT_ID,
        if result == kIOReturnSuccess {
            "accepted".to_owned()
        } else {
            describe(result)
        },
    ));
}

/// The on-queue body of `request_multitouch_mode` (Swift's version asserts the queue with
/// `dispatchPrecondition`; here the caller either runs on the queue already or came through
/// `Shared::sync`).
fn request_multitouch_mode_on_queue(inner: &Inner) {
    // A WCH panel seen sending mouse frames is in single-touch emulation: ask for multitouch again.
    // The pipeline rate-limits this through PanelModeWatch.
    if let Some(digitizer) = inner.wch_digitizer.clone() {
        request_digitizer_mode(&digitizer, inner);
    }
    for device in &inner.devices {
        if vendor_of(device) != Some(ILITEK_VENDOR_ID) {
            continue;
        }
        for (id, length) in [(2usize, 2usize), (6usize, 257usize)] {
            let mut bytes = vec![0u8; length];
            let mut size = length as CFIndex;
            // SAFETY: valid buffer and in/out length pointer, as the binding requires.
            let result = unsafe {
                device.report(
                    IOHIDReportType::Feature,
                    id as CFIndex,
                    NonNull::from(&mut bytes).cast::<u8>(),
                    NonNull::from(&mut size),
                )
            };
            let outcome = if result == kIOReturnSuccess {
                format!("{size} bytes read")
            } else {
                describe(result)
            };
            inner.send_status(format!("feature report {id}: {outcome}"));
        }
    }
}

/// The USB vendor id from a device's properties, or `None` when the property is missing or not a
/// number (Swift: `vendor(of:)`).
fn vendor_of(device: &IOHIDDevice) -> Option<u16> {
    number_u16(device, kIOHIDVendorIDKey)
}

/// An unsigned integer property (Swift: `(property as? NSNumber)?.intValue`).
fn number_u16(device: &IOHIDDevice, key: &'static CStr) -> Option<u16> {
    u16::try_from(number_i32(device, key)?).ok()
}

/// An unsigned integer property (Swift: `(property as? NSNumber)?.intValue`).
fn number_u32(device: &IOHIDDevice, key: &'static CStr) -> Option<u32> {
    u32::try_from(number_i32(device, key)?).ok()
}

fn number_i32(device: &IOHIDDevice, key: &'static CStr) -> Option<i32> {
    let value = device.property(&key_string(key))?;
    let number = value.downcast::<CFNumber>().ok()?;
    number.as_i32()
}

/// The product-name property (Swift: `(property as? String)`).
fn product_of(device: &IOHIDDevice) -> Option<String> {
    let value = device.property(&key_string(kIOHIDProductKey))?;
    let string = value.downcast::<CFString>().ok()?;
    Some(string.to_string())
}

/// The property-key constants are `&CStr`; the HID API wants a CFString.
fn key_string(key: &'static CStr) -> CFRetained<CFString> {
    // The constants are ASCII literals generated from the IOKit headers.
    CFString::from_str(key.to_str().expect("IOKit property key constants are ASCII"))
}

/// Lowercase, space-separated bytes (Swift: `hex`).
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0xF) as usize] as char);
    }
    text
}

/// The text the app's log readers depend on for the two common seize failures, hex for the rest
/// (Swift: `describe`).
fn describe(result: IOReturn) -> String {
    // Opening a mouse or keyboard interface is what Input Monitoring governs; a digitizer interface
    // opens without it.
    if result == kIOReturnNotPermitted as IOReturn {
        "kIOReturnNotPermitted (grant Input Monitoring, then relaunch)".into()
    } else if result == kIOReturnExclusiveAccess as IOReturn {
        "kIOReturnExclusiveAccess (another app holds the panel)".into()
    } else {
        format!("0x{0:08X}", result as u32)
    }
}
