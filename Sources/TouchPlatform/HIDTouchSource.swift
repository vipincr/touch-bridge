import Foundation
import IOKit.hid

/// How the seized panel reports touch.
public enum PanelFamily: Equatable, Sendable {
    /// WCH (wch.cn, 0x27C0:0x0859 — the JAPANNEXT JN-MD-IPST105FP): on macOS the digitizer
    /// interface is firmware-gated to non-Apple hosts (the mode-switch feature is accepted and
    /// ignored), so touch arrives as absolute single touch on the panel's mouse interface.
    /// On Windows and Linux hosts the digitizer interface streams full multitouch.
    case wch
    /// ILITEK (0x222A:0x0001): multitouch digitizer that reverts to single-touch mouse reports
    /// after losing power until feature reports 2 and 6 are read.
    case ilitek
    /// Any other touch-screen digitizer matched by class.
    case other
    case unknown
}

/// Finds the touch panel, opens it exclusively (seize) and delivers raw input reports.
/// Two matchers run side by side: any USB touch-screen digitizer by HID class (Apple's own
/// devices and WCH panels excepted), and — because a WCH panel's touch data on macOS arrives on
/// its mouse interface — every HID interface of the exact WCH USB device, so that interface is
/// seized too and macOS stops turning touches into cursor movement itself.
/// IOHIDManager is used only for matching. For each match we create our own IOHIDDevice,
/// because the manager's device objects are already activated and IOKit requires every
/// "Register" call to happen before activation. All device callbacks run on `queue`.
public final class HIDTouchSource: @unchecked Sendable {
    static let ilitekVendorID = 0x222A
    static let wchVendorID = 0x27C0
    static let wchProductID = 0x0859
    static let appleVendorID = 0x05AC
    static let inputModeReportID: UInt8 = 0x21
    static let inputModeDigitizer: UInt8 = 0x02
    static let reportBufferSize = 128

    public let queue = DispatchQueue(label: "com.gramini.touchnext.hid", qos: .userInteractive)

    /// (mach absolute timestamp, report bytes including the report ID at index 0)
    public var onReport: ((UInt64, [UInt8]) -> Void)?
    public var onStatus: ((String) -> Void)?

    /// The family of the most recently attached panel. Touched on `queue` only.
    public private(set) var family: PanelFamily = .unknown

    /// Fresh managers per start: a cancelled IOHIDManager can't be registered or activated again
    /// (IOKit asserts; "Disable → Enable" crashed before this). Only touched by start/stop.
    private var managers: [IOHIDManager] = []
    /// Seized devices; only touched on `queue`.
    private var devices: [IOHIDDevice] = []
    /// Keeps the match boxes alive for the lifetime of their managers.
    private var boxes: [MatchBox] = []
    private var matchedAny = false

    public init() {}

    public func start() {
        guard managers.isEmpty else { return }
        managers.append(makeManager(matching: [
            kIOHIDPrimaryUsagePageKey: kHIDPage_Digitizer,
            kIOHIDPrimaryUsageKey: kHIDUsage_Dig_TouchScreen,
        ], wchOnly: false, note: "any USB touch-screen digitizer except Apple and WCH"))
        managers.append(makeManager(matching: [
            kIOHIDVendorIDKey: Self.wchVendorID,
            kIOHIDProductIDKey: Self.wchProductID,
        ], wchOnly: true, note: "WCH TouchScreen, all interfaces (touch data arrives on the mouse interface)"))
    }

    private func makeManager(matching: [String: Any], wchOnly: Bool, note: String) -> IOHIDManager {
        let manager = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
        IOHIDManagerSetDeviceMatching(manager, matching as CFDictionary)
        let box = MatchBox(source: self, wchOnly: wchOnly)
        boxes.append(box)
        IOHIDManagerRegisterDeviceMatchingCallback(manager, { ctx, _, _, matched in
            // May run on the thread calling IOHIDManagerActivate (devices already present) or on the queue (hotplug).
            guard let ctx, let device = IOHIDDeviceCreate(kCFAllocatorDefault, IOHIDDeviceGetService(matched)) else { return }
            let box = Unmanaged<MatchBox>.fromOpaque(ctx).takeUnretainedValue()
            guard let vendor = HIDTouchSource.vendor(of: matched) else { return }
            if box.wchOnly {
                guard vendor == HIDTouchSource.wchVendorID else { return }
            } else {
                guard vendor != HIDTouchSource.appleVendorID, vendor != HIDTouchSource.wchVendorID else { return }
            }
            let boxed = DeviceBox(device: device)
            box.source.queue.async { box.source.attach(boxed.device) }
        }, Unmanaged.passUnretained(box).toOpaque())
        IOHIDManagerSetDispatchQueue(manager, queue)
        // IOHIDManager.h: release the manager only after it is cancelled and this handler has run.
        let keepAlive = ManagerBox(manager: manager)
        IOHIDManagerSetCancelHandler(manager) { _ = keepAlive }
        IOHIDManagerActivate(manager)
        onStatus?("matching started (\(note))")
        return manager
    }

    /// Closes every seized device, which hands the panel back to macOS.
    public func stop() {
        queue.sync {
            for device in devices {
                IOHIDDeviceClose(device, IOOptionBits(kIOHIDOptionsTypeSeizeDevice))
                IOHIDDeviceCancel(device)
            }
            devices.removeAll()
            for manager in managers { IOHIDManagerCancel(manager) }
            managers.removeAll()
            boxes.removeAll()
            family = .unknown
        }
        onStatus?("stopped; panel released")
    }

    /// True once the panel has matched as a HID device (whether or not the seize succeeded).
    public var hasMatchedDevice: Bool { queue.sync { matchedAny } }

    /// Reads a feature report (e.g. RID 2 contact-count max, RID 6 vendor blob). Read-only.
    public func readFeatureReport(id: UInt8, length: Int) -> [UInt8]? {
        queue.sync {
            guard let device = devices.first else { return nil }
            var bytes = [UInt8](repeating: 0, count: length)
            var size = CFIndex(length)
            let result = IOHIDDeviceGetReport(device, kIOHIDReportTypeFeature, CFIndex(id), &bytes, &size)
            guard result == kIOReturnSuccess else {
                onStatus?("feature report \(id) read failed: \(Self.describe(result))")
                return nil
            }
            return Array(bytes.prefix(size))
        }
    }

    /// Reads feature reports 2 (contact-count maximum) and 6 (vendor blob) from every seized
    /// ILITEK panel. Linux's hid-multitouch reads the blob "to enable some devices": after a power
    /// cycle the panel sends single-touch mouse reports until it is read. Other controllers are
    /// left alone. Call on `queue` (from a report callback).
    public func requestMultitouchMode() {
        dispatchPrecondition(condition: .onQueue(queue))
        for device in devices where Self.vendor(of: device) == Self.ilitekVendorID {
            for (id, length) in [(2, 2), (6, 257)] {
                var bytes = [UInt8](repeating: 0, count: length)
                var size = CFIndex(length)
                let result = IOHIDDeviceGetReport(device, kIOHIDReportTypeFeature, CFIndex(id), &bytes, &size)
                onStatus?("feature report \(id): \(result == kIOReturnSuccess ? "\(size) bytes read" : Self.describe(result))")
            }
        }
    }

    private func attach(_ device: IOHIDDevice) {
        matchedAny = true
        let vendor = Self.vendor(of: device) ?? 0
        let pid = (IOHIDDeviceGetProperty(device, kIOHIDProductIDKey as CFString) as? NSNumber)?.intValue ?? 0
        let product = (IOHIDDeviceGetProperty(device, kIOHIDProductKey as CFString) as? String) ?? "unknown product"
        let page = (IOHIDDeviceGetProperty(device, kIOHIDPrimaryUsagePageKey as CFString) as? NSNumber)?.intValue ?? 0
        family = vendor == Self.wchVendorID ? .wch : (vendor == Self.ilitekVendorID ? .ilitek : .other)
        onStatus?(String(format: "panel matched: %@ (VID 0x%04X PID 0x%04X, usage page %d)", product, vendor, pid, page))
        let result = IOHIDDeviceOpen(device, IOOptionBits(kIOHIDOptionsTypeSeizeDevice))
        guard result == kIOReturnSuccess else {
            onStatus?("seize failed: \(Self.describe(result))")
            return
        }
        if page == kHIDPage_Digitizer, vendor == Self.wchVendorID {
            // WCH panels boot in single-touch mouse emulation and only stream digitizer reports
            // once the host asks for digitizer mode (as Windows does during enumeration). The
            // write must happen BEFORE input reports are registered — done afterwards, the
            // interface stays silent — and the buffer is the feature's payload only, without
            // the report ID (Touch-Up issue #40, verified on this controller family).
            requestDigitizerMode(device)
        }
        let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: Self.reportBufferSize)
        let context = Unmanaged.passUnretained(self).toOpaque()
        IOHIDDeviceRegisterInputReportWithTimeStampCallback(device, buffer, Self.reportBufferSize, { ctx, result, _, _, _, report, length, timestamp in
            guard result == kIOReturnSuccess, let ctx else { return }
            let bytes = Array(UnsafeBufferPointer(start: report, count: length))
            Unmanaged<HIDTouchSource>.fromOpaque(ctx).takeUnretainedValue().onReport?(timestamp, bytes)
        }, context)
        IOHIDDeviceRegisterRemovalCallback(device, { ctx, _, sender in
            guard let ctx, let sender else { return }
            Unmanaged<HIDTouchSource>.fromOpaque(ctx).takeUnretainedValue().detach(Unmanaged<IOHIDDevice>.fromOpaque(sender).takeUnretainedValue())
        }, context)
        IOHIDDeviceSetDispatchQueue(device, queue)
        IOHIDDeviceSetCancelHandler(device) { buffer.deallocate() }
        IOHIDDeviceActivate(device)
        devices.append(device)
        onStatus?("panel seized")
        if page == kHIDPage_Digitizer, vendor == Self.ilitekVendorID {
            // After a power cycle (unplug, sleep) the panel starts in single-touch mouse mode.
            requestMultitouchMode()
        }
    }

    /// Writes Input Mode = digitizer (feature report 0x21). The buffer is the feature's payload
    /// only — [input mode, device index], without the report ID, which travels in its own
    /// parameter — and the call must precede input-report registration.
    private func requestDigitizerMode(_ device: IOHIDDevice) {
        var mode: [UInt8] = [Self.inputModeDigitizer, 0]
        let result = IOHIDDeviceSetReport(device, kIOHIDReportTypeFeature, CFIndex(Self.inputModeReportID), &mode, mode.count)
        onStatus?("input mode → digitizer: \(result == kIOReturnSuccess ? "set" : Self.describe(result))")
    }

    private func detach(_ device: IOHIDDevice) {
        guard let index = devices.firstIndex(where: { CFEqual($0, device) }) else { return }
        IOHIDDeviceCancel(devices.remove(at: index))
        onStatus?("panel removed")
    }

    static func vendor(of device: IOHIDDevice) -> Int? {
        (IOHIDDeviceGetProperty(device, kIOHIDVendorIDKey as CFString) as? NSNumber)?.intValue
    }

    static func describe(_ result: IOReturn) -> String {
        switch result {
        case kIOReturnNotPermitted: "kIOReturnNotPermitted (grant Accessibility, then relaunch)"
        case kIOReturnExclusiveAccess: "kIOReturnExclusiveAccess (another app holds the panel)"
        default: String(format: "0x%08X", UInt32(bitPattern: result))
        }
    }
}

/// Carries a CF device object across the hop onto the HID queue.
private struct DeviceBox: @unchecked Sendable {
    let device: IOHIDDevice
}

/// Tells a manager's matching callback which vendor filter applies. Owned by `boxes`.
private final class MatchBox: @unchecked Sendable {
    let source: HIDTouchSource
    let wchOnly: Bool

    init(source: HIDTouchSource, wchOnly: Bool) {
        self.source = source
        self.wchOnly = wchOnly
    }
}

/// Keeps a cancelled manager alive until its cancel handler has run.
private struct ManagerBox: @unchecked Sendable {
    let manager: IOHIDManager
}
