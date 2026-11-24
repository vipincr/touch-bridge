/// The panel can report either multitouch digitizer frames or single-touch "mouse" frames.
/// An ILITEK panel reverts to mouse frames after losing power until the host reads its feature
/// reports (as Linux's hid-multitouch does), so this decides when to ask, and notices when
/// multitouch returns. A WCH panel on macOS streams mouse frames for good — its digitizer
/// interface is firmware-gated to non-Apple hosts — so `mouseModeIsNormal` makes those frames
/// ordinary input instead of a fault to recover from.
public struct PanelModeWatch: Sendable {
    public enum Action: Equatable, Sendable {
        /// Read the panel's feature reports to switch it to multitouch.
        case requestMultitouch
        /// Multitouch frames arrive again after mouse frames.
        case multitouchRestored
    }

    static let retryIntervalNs: UInt64 = 5_000_000_000
    /// True when mouse frames are the panel's normal output, not a degraded state.
    public var mouseModeIsNormal = false
    private var lastRequestNs: UInt64?
    private var sawMouse = false

    public init() {}

    public mutating func observe(_ source: TouchFrame.Source, atNs now: UInt64) -> Action? {
        switch source {
        case .mouse:
            sawMouse = true
            guard !mouseModeIsNormal else { return nil }
            if let last = lastRequestNs, now - last < Self.retryIntervalNs { return nil }
            lastRequestNs = now
            return .requestMultitouch
        case .digitizer:
            guard sawMouse else { return nil }
            sawMouse = false
            return .multitouchRestored
        }
    }
}
