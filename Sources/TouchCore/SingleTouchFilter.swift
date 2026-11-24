import Foundation

/// Coordinate repair for the single-contact stream a WCH panel sends while in mouse
/// emulation — the only touch data available until the digitizer-mode switch takes hold.
/// Measured on the JN-MD-IPST105FP (probe 2026-11-24), a slow drag contains single-frame
/// jumps of up to 129 mm while the tip stays down (p95: 21 mm): the contact centroid
/// glitching, not the finger. Trusted, each one teleports the drag anchor — the "tabs
/// dancing while my finger is still" symptom.
///
/// The policy is libinput's touchpad jump detector (tp_detect_jumps), because that is the
/// maintained, shipped implementation of exactly this problem for exactly this feel:
/// normalize the frame's motion to a 12 ms reference interval; a jump is travel over
/// `jumpMaxMm` (20 mm) or growth over `jumpGrowthMm` (7 mm) versus the previous frame; a
/// jump is discarded — the held position is emitted instead, and the reference stays put,
/// so a wandering centroid reports nothing. Samples staler than `jumpStaleNs` (30 ms)
/// bypass the test (irregular frames are not evidence); after `jumpAcceptCap` consecutive
/// discards the position is accepted anyway (the contact genuinely relocated — a
/// two-finger handover — rather than glitching).
///
/// Identity is not this type's business: lifts, flaps and dropout bridging live in
/// ContactTracker. Digitizer frames pass through untouched and reset the repair state.
public struct SingleTouchFilter: Sendable {
    /// Reference frame length the jump thresholds are normalized to.
    public var jumpReferenceNs: UInt64
    /// Normalized travel beyond this is a jump (libinput: 20 mm).
    public var jumpMaxMm: Double
    /// Normalized growth beyond this over the previous frame is a jump (libinput: 7 mm).
    public var jumpGrowthMm: Double
    /// Samples staler than this skip the jump test (libinput: 30 ms).
    public var jumpStaleNs: UInt64
    /// Consecutive discards before a new position is accepted as real.
    public var jumpAcceptCap: Int

    private var last: (x: Double, y: Double, atNs: UInt64, deltaMm: Double)?
    private var discards = 0

    public init(jumpReferenceNs: UInt64 = 12_000_000, jumpMaxMm: Double = 20,
                jumpGrowthMm: Double = 7, jumpStaleNs: UInt64 = 30_000_000, jumpAcceptCap: Int = 6) {
        self.jumpReferenceNs = jumpReferenceNs
        self.jumpMaxMm = jumpMaxMm
        self.jumpGrowthMm = jumpGrowthMm
        self.jumpStaleNs = jumpStaleNs
        self.jumpAcceptCap = jumpAcceptCap
    }

    /// The frames to feed the tracker instead of `frame`. A discarded jump contributes the
    /// held position; everything else passes as reported.
    public mutating func filter(_ frame: TouchFrame, atNs now: UInt64,
                                panelWidthMm: Double, panelHeightMm: Double) -> [TouchFrame] {
        guard frame.source == .mouse else {
            last = nil
            discards = 0
            return [frame]
        }
        guard let contact = frame.contacts.first, contact.touching else {
            last = nil
            discards = 0
            return [frame]
        }
        guard let previous = last else {
            last = (contact.x, contact.y, now, 0)
            discards = 0
            return [frame]
        }

        let dt = now - previous.atNs
        if dt > jumpStaleNs || dt == 0 {
            // Irregular frame (after a dropout, or the same timestamp): not jump evidence.
            last = (contact.x, contact.y, now, 0)
            discards = 0
            return [frame]
        }
        let distMm = hypot((contact.x - previous.x) * panelWidthMm, (contact.y - previous.y) * panelHeightMm)
        let normalized = distMm * Double(jumpReferenceNs) / Double(dt)
        if normalized <= jumpMaxMm && abs(normalized - previous.deltaMm) <= jumpGrowthMm {
            last = (contact.x, contact.y, now, normalized)
            discards = 0
            return [frame]
        }

        // A jump. Discard it — unless it has persisted, when it is a real relocation. The
        // reference's clock advances even while held, so a run of discards is not mistaken
        // for one stale sample that would bypass the test.
        discards += 1
        if discards >= jumpAcceptCap {
            last = (contact.x, contact.y, now, 0)
            discards = 0
            return [frame]
        }
        last = (previous.x, previous.y, now, 0)
        return [TouchFrame(source: .mouse,
                           contacts: [Contact(id: contact.id, touching: true, x: previous.x, y: previous.y)],
                           scanTime: frame.scanTime)]
    }
}
