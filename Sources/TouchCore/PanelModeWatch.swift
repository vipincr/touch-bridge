/// The panel can report either multitouch digitizer frames or single-touch "mouse" frames.
/// Mouse frames always mean the panel is in single-touch emulation — either the initial
/// mode switch did not take or the panel slipped back — so every mouse frame asks for
/// multitouch again (rate-limited): one feature-report write while the panel streams
/// mouse input costs nothing, and staying in emulation silently costs every gesture.
/// ILITEK panels are switched back by reading feature reports 2 and 6, as Linux's
/// hid-multitouch does.
public struct PanelModeWatch: Sendable {
    public enum Action: Equatable, Sendable {
        /// Read the panel's feature reports to switch it to multitouch.
        case requestMultitouch
        /// Multitouch frames arrive again after mouse frames.
        case multitouchRestored
    }

    static let retryIntervalNs: UInt64 = 2_000_000_000
    private var lastRequestNs: UInt64?
    private var sawMouse = false

    public init() {}

    public mutating func observe(_ source: TouchFrame.Source, atNs now: UInt64) -> Action? {
        switch source {
        case .mouse:
            sawMouse = true
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
