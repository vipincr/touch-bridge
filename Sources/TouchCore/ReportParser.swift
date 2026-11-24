/// One finger in a frame. Coordinates are normalized to 0...1 across the panel.
public struct Contact: Equatable, Sendable {
    public let id: UInt8
    public let touching: Bool
    public let x: Double
    public let y: Double

    public init(id: UInt8, touching: Bool, x: Double, y: Double) {
        self.id = id
        self.touching = touching
        self.x = x
        self.y = y
    }
}

/// All contacts reported by one HID input report.
public struct TouchFrame: Equatable, Sendable {
    public enum Source: Equatable, Sendable { case digitizer, mouse }

    public let source: Source
    public let contacts: [Contact]
    /// Device scan time (RID 4 only); units are confirmed by the M1 probe.
    public let scanTime: UInt32?
}

/// Decodes the multitouch digitizer input reports of the two supported controller families.
/// One decoder per wire format; the report ID says which panel is talking.
public enum ReportParser {
    /// WCH (wch.cn, USB 0x27C0:0x0859 — the JAPANNEXT JN-MD-IPST105FP), from its HID report
    /// descriptor: RID 0x0D, ten 5-byte slots (tip switch at bit 0, contact id at bits 4-7,
    /// X and Y as little-endian 16-bit up to 16383 / 9599), then scan time LE16 and contact
    /// count; 54 bytes in total.
    static let wchReportID: UInt8 = 0x0D
    static let wchReportLength = 54
    static let wchScanTimeOffset = 51
    static let wchContactCountOffset = 53
    static let wchSlotSize = 5
    static let wchMaxSlots = 10
    static let wchLogicalMaxX = 16383.0
    static let wchLogicalMaxY = 9599.0

    /// ILITEK (USB 0x222A:0x0001 — the upstream ARZOPA reference panel): RID 4, N slots of 5
    /// bytes (contact id at bits 0-5, tip switch at 0x40, X and Y LE16 up to 16384 / 9600), then
    /// scan time LE32 and contact count. N is the panel's contact limit and varies: the 10-contact
    /// ARZOPA pads its report to 64 bytes; shorter ILITEK-family panels send shorter reports, so
    /// the slot count is derived from each report's own length and capped at 10.
    static let ilitekReportID: UInt8 = 0x04
    static let ilitekReportMinLength = 11
    static let ilitekSlotSize = 5
    static let ilitekMaxSlots = 10
    static let ilitekLogicalMaxX = 16384.0
    static let ilitekLogicalMaxY = 9600.0

    /// WCH mouse interface (single touch on macOS, the digitizer interface being firmware-gated):
    /// RID 7, buttons @1, X LE16 @2 (logical 0-16383), Y LE16 @4 (logical 0-9599), wheel @6 —
    /// extents confirmed from the interface's own report descriptor.
    static let wchMouseReportID: UInt8 = 0x07
    static let wchMouseReportLength = 7

    /// ILITEK mouse fallback: RID 5, buttons @1, X LE16 @2, Y LE16 @4.
    static let mouseReportID: UInt8 = 0x05
    static let mouseReportLength = 6

    public static func parse(_ bytes: [UInt8]) -> TouchFrame? {
        guard let reportID = bytes.first else { return nil }
        switch reportID {
        case wchReportID where bytes.count >= wchReportLength:
            return parseWCH(bytes)
        case wchMouseReportID where bytes.count >= wchMouseReportLength:
            return parseWCHMouse(bytes)
        case ilitekReportID where bytes.count >= ilitekReportMinLength:
            return parseILITEK(bytes)
        case mouseReportID where bytes.count >= mouseReportLength:
            return parseMouse(bytes)
        default:
            return nil
        }
    }

    private static func parseWCHMouse(_ r: [UInt8]) -> TouchFrame {
        TouchFrame(
            source: .mouse,
            contacts: [Contact(
                id: 0,
                touching: r[1] & 0x01 != 0,
                x: normalize(le16(r, 2), wchLogicalMaxX),
                y: normalize(le16(r, 4), wchLogicalMaxY)
            )],
            scanTime: nil
        )
    }

    private static func parseWCH(_ r: [UInt8]) -> TouchFrame {
        let count = min(Int(r[wchContactCountOffset]), wchMaxSlots)
        let contacts = (0..<count).map { slot -> Contact in
            let base = 1 + slot * wchSlotSize
            return Contact(
                id: r[base] >> 4,
                touching: r[base] & 0x01 != 0,
                x: normalize(le16(r, base + 1), wchLogicalMaxX),
                y: normalize(le16(r, base + 3), wchLogicalMaxY)
            )
        }
        return TouchFrame(source: .digitizer, contacts: contacts, scanTime: UInt32(le16(r, wchScanTimeOffset)))
    }

    private static func parseILITEK(_ r: [UInt8]) -> TouchFrame {
        let slotCount = min((r.count - 6) / ilitekSlotSize, ilitekMaxSlots)
        let scanTimeOffset = 1 + slotCount * ilitekSlotSize
        let contactCountOffset = scanTimeOffset + 4
        let count = min(Int(r[contactCountOffset]), slotCount)
        let contacts = (0..<count).map { slot -> Contact in
            let base = 1 + slot * ilitekSlotSize
            return Contact(
                id: r[base] & 0x3F,
                touching: r[base] & 0x40 != 0,
                x: normalize(le16(r, base + 1), ilitekLogicalMaxX),
                y: normalize(le16(r, base + 3), ilitekLogicalMaxY)
            )
        }
        let scanTime = (0..<4).reduce(UInt32(0)) { $0 | UInt32(r[scanTimeOffset + $1]) << (8 * UInt32($1)) }
        return TouchFrame(source: .digitizer, contacts: contacts, scanTime: scanTime)
    }

    private static func parseMouse(_ r: [UInt8]) -> TouchFrame {
        let contact = Contact(
            id: 0,
            touching: r[1] & 0x01 != 0,
            x: normalize(le16(r, 2), ilitekLogicalMaxX),
            y: normalize(le16(r, 4), ilitekLogicalMaxY)
        )
        return TouchFrame(source: .mouse, contacts: [contact], scanTime: nil)
    }

    private static func le16(_ r: [UInt8], _ offset: Int) -> UInt16 {
        UInt16(r[offset]) | UInt16(r[offset + 1]) << 8
    }

    private static func normalize(_ value: UInt16, _ max: Double) -> Double {
        min(Double(value) / max, 1)
    }
}
