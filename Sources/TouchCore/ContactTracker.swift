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

/// Turns successive frames into began/moved/ended events per contact id.
/// A contact ends when its tip lifts, when it disappears from a frame, or when no frame
/// arrives for `liftTimeoutNs` (call `expire` from a timer to catch a lost lift report).
///
/// Lifts are debounced: a contact that returns within `debounceNs` — a WCH tip flap during a
/// press or lift — continues as a move instead of ending and beginning again, so a held finger
/// stays one contact (and a long-press stays a long-press) and no ghost taps fire. A lift is
/// believed once the window has passed; call `expire` from a timer to flush the last one.
public struct ContactTracker: Sendable {
    private var active: [UInt8: (x: Double, y: Double)] = [:]
    /// Lifts held back, with the frame time they happened at.
    private var pending: [UInt8: (x: Double, y: Double, atNs: UInt64)] = [:]
    private var lastFrameNs: UInt64 = 0
    private let liftTimeoutNs: UInt64
    private let debounceNs: UInt64

    public init(liftTimeoutNs: UInt64 = 100_000_000, debounceNs: UInt64 = 60_000_000) {
        self.liftTimeoutNs = liftTimeoutNs
        self.debounceNs = debounceNs
    }

    public var activeCount: Int { active.count }

    public mutating func update(_ frame: TouchFrame, atNs now: UInt64) -> [ContactEvent] {
        lastFrameNs = now
        let touching = frame.contacts.filter(\.touching)
        let touchingIDs = Set(touching.map(\.id))

        var events: [ContactEvent] = []

        // A returning contact absorbs its held lift and continues as the same contact.
        for contact in touching {
            if let lift = pending.removeValue(forKey: contact.id) {
                active[contact.id] = (lift.x, lift.y)
            }
        }

        for contact in frame.contacts where !contact.touching && active[contact.id] != nil {
            pending[contact.id] = (contact.x, contact.y, now)
            active[contact.id] = nil
        }
        for (id, position) in active.sorted(by: { $0.key < $1.key }) where !touchingIDs.contains(id) {
            pending[id] = (position.x, position.y, now)
        }
        active = active.filter { touchingIDs.contains($0.key) }

        flushDue(now: now, into: &events)

        var began: [ContactEvent] = []
        var moved: [ContactEvent] = []
        for contact in touching {
            if let previous = active[contact.id] {
                if previous != (contact.x, contact.y) {
                    moved.append(ContactEvent(id: contact.id, phase: .moved, x: contact.x, y: contact.y))
                }
            } else {
                began.append(ContactEvent(id: contact.id, phase: .began, x: contact.x, y: contact.y))
            }
            active[contact.id] = (contact.x, contact.y)
        }
        return events + began + moved
    }

    public mutating func expire(atNs now: UInt64) -> [ContactEvent] {
        var events: [ContactEvent] = []
        flushDue(now: now, into: &events)
        guard !active.isEmpty, now > lastFrameNs, now - lastFrameNs > liftTimeoutNs else { return events }
        for (id, position) in active.sorted(by: { $0.key < $1.key }) {
            events.append(ContactEvent(id: id, phase: .ended, x: position.x, y: position.y))
        }
        active.removeAll()
        return events
    }

    /// Emits the lifts whose debounce window has passed.
    private mutating func flushDue(now: UInt64, into events: inout [ContactEvent]) {
        for (id, lift) in pending.sorted(by: { $0.key < $1.key }) where now - lift.atNs >= debounceNs {
            events.append(ContactEvent(id: id, phase: .ended, x: lift.x, y: lift.y))
            pending.removeValue(forKey: id)
        }
    }
}
