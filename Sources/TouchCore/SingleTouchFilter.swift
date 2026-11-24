import Foundation

/// A single-contact stream reports one contact. When a second finger lands, that contact
/// teleports between the fingers frame to frame, and a flapping controller can jump coordinates
/// on its own. A jump this large is not a finger moving: split it into a lift and a fresh touch,
/// so the gesture under way ends cleanly instead of dragging across the screen.
public struct SingleTouchFilter: Sendable {
    public let maxJumpMm: Double
    private var last: Contact?

    public init(maxJumpMm: Double = 25) {
        self.maxJumpMm = maxJumpMm
    }

    /// The frames to feed the tracker instead of `frame`. Digitizer frames pass through
    /// untouched: multiple contacts are real, and contact ids already track lifecycles.
    public mutating func filter(_ frame: TouchFrame, panelWidthMm: Double, panelHeightMm: Double) -> [TouchFrame] {
        guard frame.source == .mouse else { return [frame] }
        guard let contact = frame.contacts.first else {
            last = nil
            return [frame]
        }
        let previous = last
        last = contact
        guard let previous else { return [frame] }
        guard previous.touching, contact.touching else { return [frame] }
        let jumpMm = hypot((contact.x - previous.x) * panelWidthMm, (contact.y - previous.y) * panelHeightMm)
        guard jumpMm > maxJumpMm else { return [frame] }
        let lift = Contact(id: previous.id, touching: false, x: previous.x, y: previous.y)
        return [
            TouchFrame(source: .mouse, contacts: [lift], scanTime: frame.scanTime),
            frame,
        ]
    }
}
