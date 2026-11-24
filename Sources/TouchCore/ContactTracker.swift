import Foundation

public enum ContactPhase: Equatable, Sendable { case began, moved, ended }

/// A per-finger lifecycle change. Coordinates are normalized to 0...1 across the panel.
public struct ContactEvent: Equatable, Sendable {
    public let id: UInt8
    public let phase: ContactPhase
    public let x: Double
    public let y: Double

    public init(id: UInt8, phase: ContactPhase, x: Double, y: Double) {
        self.id = id
        self.phase = phase
        self.x = x
        self.y = y
    }
}

/// One finger on the panel, with what the lift policy needs to know about it.
private struct Press {
    var x: Double
    var y: Double
    /// Frame time of the last report for this press, ns.
    var lastAtNs: UInt64
    /// Exponential average of speed, mm/s.
    var speed: Double

    init(x: Double, y: Double, lastAtNs: UInt64 = 0, speed: Double = 0) {
        self.x = x
        self.y = y
        self.lastAtNs = lastAtNs
        self.speed = speed
    }
}

/// Turns successive frames into began/moved/ended events per contact id — the identity layer,
/// playing the role the kernel's input-mt plays under Linux. A contact ends when its tip lifts,
/// when it disappears from a frame, or when no frame arrives for `liftTimeoutNs` (call `expire`
/// from a timer to catch a lost lift report). The kernel's own timeout for "device forgot the
/// liftoff" is 100 ms (hid-multitouch release_timer); that is this type's default. Liftoff is
/// never delayed beyond `debounceNs` (60 ms): every canonical stack ends a touch when its
/// tracking id goes, and a delayed end would hold gestures open after the finger left.
///
/// Within that 60 ms window a returning contact continues the press instead of ending and
/// beginning it — a WCH tip flap during a press or lift. The return may even carry a different
/// contact id, provided it lands within a velocity-scaled radius of the finger: controllers
/// that renumber a finger mid-press (digitizer id churn, or the mouse stream's contact 0 after
/// a flap) would otherwise end and restart the gesture. The kernel's input_mt_assign_slots
/// solves the same problem with a bipartite match; one finger at a time, a radius gate is that
/// match. Longer dropouts (250 ms–1.7 s on this panel, mid-drag) are bridged one layer up, by
/// the recognizer's drag grace, which knows what gesture is under way and gates the resume on
/// position as well.
public struct ContactTracker: Sendable {
    private var active: [UInt8: Press] = [:]
    /// Lifts held back, with the frame time they happened at.
    private var pending: [UInt8: (press: Press, atNs: UInt64)] = [:]
    /// Frame contact id → the id this finger continues as (cross-id adoption).
    private var alias: [UInt8: UInt8] = [:]
    private var lastFrameNs: UInt64 = 0
    private let liftTimeoutNs: UInt64
    private let debounceNs: UInt64
    private let adoptionBaseMm: Double
    private let adoptionSpeedGain: Double
    private let panelWidthMm: Double
    private let panelHeightMm: Double

    public init(liftTimeoutNs: UInt64 = 100_000_000, debounceNs: UInt64 = 60_000_000,
                adoptionBaseMm: Double = 15, adoptionSpeedGain: Double = 0.75,
                panelWidthMm: Double = 221.9, panelHeightMm: Double = 147.9) {
        self.liftTimeoutNs = liftTimeoutNs
        self.debounceNs = debounceNs
        self.adoptionBaseMm = adoptionBaseMm
        self.adoptionSpeedGain = adoptionSpeedGain
        self.panelWidthMm = panelWidthMm
        self.panelHeightMm = panelHeightMm
    }

    public var activeCount: Int { active.count }

    public mutating func update(_ frame: TouchFrame, atNs now: UInt64) -> [ContactEvent] {
        lastFrameNs = now
        // A WCH report can carry the same contact id in two slots, one of them stale (seen in
        // the probe-session-1 replay: one frame with two slots for id 3, one touched and one
        // lifted). The touching instance is authoritative; the last one in the report wins.
        var byID: [UInt8: Contact] = [:]
        for contact in frame.contacts where contact.touching { byID[contact.id] = contact }
        let touching = byID.keys.sorted().map { byID[$0]! }
        let touchingIDs = Set(byID.keys)

        var events: [ContactEvent] = []

        for contact in touching {
            if let lift = pending[contact.id],
               now - lift.atNs <= debounceNs,
               distanceMm(contact, lift.press) <= adoptionRadiusMm(lift.press, now - lift.atNs) {
                // Same id back within the window and within a finger's travel: a tip flap. The
                // press continues; the moved pass below reports where the finger went.
                pending.removeValue(forKey: contact.id)
                active[contact.id] = lift.press
            } else if let (oldId, lift) = nearestPending(to: contact, now: now) {
                // A different id near a held lift's finger: the same finger under a new number
                // (id churn, or the mouse stream's contact 0 after a flap). The moved pass
                // below reports the jump, under the id the gesture already knows.
                pending.removeValue(forKey: oldId)
                alias[contact.id] = oldId
                active[oldId] = lift.press
            }
        }

        // An aliased contact keeps its press alive under the canonical id, this frame and after.
        let liveIDs = Set(touching.map { alias[$0.id] ?? $0.id })
        for contact in frame.contacts where !contact.touching && !touchingIDs.contains(contact.id) {
            let key = alias[contact.id] ?? contact.id
            if let press = active.removeValue(forKey: key) {
                // A lift report only says the finger is gone. Its coordinates are not used: the
                // JN-MD-IPST105FP sends about one lift report in fourteen with (0, 0), and the
                // press's position is where a tap is placed.
                pending[key] = (press, now)
            }
        }
        for (id, press) in active.sorted(by: { $0.key < $1.key }) where !liveIDs.contains(id) {
            pending[id] = (press, now)
        }
        active = active.filter { liveIDs.contains($0.key) }

        flushDue(now: now, into: &events)

        var began: [ContactEvent] = []
        var moved: [ContactEvent] = []
        for contact in touching {
            let key = alias[contact.id] ?? contact.id
            if let previous = active[key] {
                if (previous.x, previous.y) != (contact.x, contact.y) {
                    moved.append(ContactEvent(id: key, phase: .moved, x: contact.x, y: contact.y))
                }
            } else {
                // The touch is genuinely new; any held lift for this id is over (the finger
                // came back farther than a flap would put it), so believe it now.
                if let lift = pending.removeValue(forKey: key) {
                    events.append(ContactEvent(id: key, phase: .ended, x: lift.press.x, y: lift.press.y))
                }
                began.append(ContactEvent(id: key, phase: .began, x: contact.x, y: contact.y))
            }
            active[key] = advanced(active[key] ?? Press(x: contact.x, y: contact.y, lastAtNs: now),
                                   to: contact, atNs: now)
        }
        return events + began + moved
    }

    public mutating func expire(atNs now: UInt64) -> [ContactEvent] {
        var events: [ContactEvent] = []
        flushDue(now: now, into: &events)
        guard !active.isEmpty, now > lastFrameNs, now - lastFrameNs > liftTimeoutNs else { return events }
        for (id, press) in active.sorted(by: { $0.key < $1.key }) {
            events.append(ContactEvent(id: id, phase: .ended, x: press.x, y: press.y))
        }
        active.removeAll()
        alias.removeAll()
        return events
    }

    /// A held lift a returning contact under a different id could be: within the debounce
    /// window and within a velocity-scaled radius of where the finger was.
    private func nearestPending(to contact: Contact, now: UInt64) -> (UInt8, (press: Press, atNs: UInt64))? {
        var best: (UInt8, (press: Press, atNs: UInt64))?
        var bestMm = Double.infinity
        for (id, lift) in pending where id != contact.id {
            let gapNs = now - lift.atNs
            guard gapNs <= debounceNs else { continue }
            let distMm = distanceMm(contact, lift.press)
            guard distMm <= adoptionRadiusMm(lift.press, gapNs), distMm < bestMm else { continue }
            best = (id, lift)
            bestMm = distMm
        }
        return best
    }

    /// How far a returning finger may have travelled over the gap: a base radius plus the
    /// press's speed over the gap.
    private func adoptionRadiusMm(_ press: Press, _ gapNs: UInt64) -> Double {
        adoptionBaseMm + press.speed * Double(gapNs) / 1_000_000_000 * adoptionSpeedGain
    }

    /// Emits the lifts whose hold window has passed, and forgets their aliases.
    private mutating func flushDue(now: UInt64, into events: inout [ContactEvent]) {
        for (id, lift) in pending.sorted(by: { $0.key < $1.key }) where now - lift.atNs >= debounceNs {
            events.append(ContactEvent(id: id, phase: .ended, x: lift.press.x, y: lift.press.y))
            pending.removeValue(forKey: id)
            alias = alias.filter { $0.value != id }
        }
    }

    private func distanceMm(_ contact: Contact, _ press: Press) -> Double {
        hypot((contact.x - press.x) * panelWidthMm, (contact.y - press.y) * panelHeightMm)
    }

    private func advanced(_ press: Press, to contact: Contact, atNs now: UInt64) -> Press {
        var next = press
        let dt = Double(now - press.lastAtNs) / 1_000_000_000
        if dt >= 0.001 {
            let v = distanceMm(contact, press) / dt
            next.speed = press.speed == 0 ? v : press.speed * 0.7 + v * 0.3
        }
        next.x = contact.x
        next.y = contact.y
        next.lastAtNs = now
        return next
    }
}
