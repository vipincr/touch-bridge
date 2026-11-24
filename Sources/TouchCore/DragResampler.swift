import CoreGraphics
import Foundation

/// Smooths the cursor path of a drag that follows a finger.
///
/// The WCH panel reports at about 59 Hz and, during a slow drag, holds still for whole frames
/// while the finger moves: of 38 frames in one recorded slow drag, 17 carried no movement at
/// all (probe 2026-11-24), with median steps of 0.7 mm. Posted frame for frame, the cursor
/// advances in bursts a couple of frames apart, about 20 to 30 real updates a second, and a
/// held frame or a bridged dropout ends in one long jump. A trackpad never does this, because
/// macOS resamples tablet and trackpad input at display rate.
///
/// So this does the same. Each reported finger position becomes a target, and the cursor
/// glides to it over `smoothingNs` with an ease-out: most of the step early, settling on it.
/// Consecutive reports land about 17 ms apart, so the cursor is always finishing one step as
/// the next arrives, which turns bursty 59 Hz input into even motion however often the caller
/// posts (the deliverer posts at 120 Hz). On the recorded drags that doubled the real update
/// rate and kept the cursor within 2 mm of the finger (median).
///
/// A target at least `maxLagMm` away is jumped to at once instead of glided: that is a bridged
/// dropout or a relocation the panel reported late, and trailing it would be seen.
///
/// Positions are in any unit (the deliverer uses screen points) scaled by `mmPerUnit`. Nothing
/// here traps: every clock subtraction is guarded, because this runs on the input path of a
/// driver that must never die on a clock reading.
public struct DragResampler: Sendable {
    public struct Config: Sendable {
        /// How long the cursor takes to settle on each new reported position.
        public var smoothingNs: UInt64
        /// A target at least this far away is jumped to rather than glided to.
        public var maxLagMm: Double
        /// Sub-threshold tremor that does not move the target.
        public var deadZoneMm: Double

        public init(smoothingNs: UInt64 = 34_000_000, maxLagMm: Double = 12, deadZoneMm: Double = 0.15) {
            self.smoothingNs = smoothingNs
            self.maxLagMm = maxLagMm
            self.deadZoneMm = deadZoneMm
        }
    }

    private let config: Config
    private let mmPerUnit: Double

    /// Where the finger was last reported.
    private var target: CGPoint
    /// Where the cursor was last posted. At most one of these is ever emitted per `step`.
    private var cursor: CGPoint
    /// Where the cursor was when the current glide began, and when.
    private var glideFrom: CGPoint
    private var glideStartNs: UInt64
    /// False only before the drag's first report; the start position counts as one.
    private var hasSample = true

    /// A drag starting at `start`; the cursor begins exactly there.
    public init(start: CGPoint, mmPerUnit: Double, config: Config = Config()) {
        self.config = config
        self.mmPerUnit = mmPerUnit
        target = start
        cursor = start
        glideFrom = start
        glideStartNs = 0
    }

    /// A new report of the finger's position.
    public mutating func sample(_ point: CGPoint, atNs now: UInt64) {
        if hasSample, distance(target, point) <= config.deadZoneMm {
            return   // tremor around the position being followed: nothing new to follow
        }
        hasSample = true
        target = point
        glideFrom = cursor
        glideStartNs = now
    }

    /// The next cursor position to post, or nil when there is nothing to post: the cursor is
    /// already where it belongs, or this step would leave it where it was.
    public mutating func step(atNs now: UInt64) -> CGPoint? {
        let fraction = glideFraction(now: now)
        let next = CGPoint(x: glideFrom.x + (target.x - glideFrom.x) * fraction,
                           y: glideFrom.y + (target.y - glideFrom.y) * fraction)
        // Compare in millimetres so the threshold does not depend on the unit.
        guard distance(next, cursor) >= 0.01 else { return nil }
        cursor = next
        return cursor
    }

    /// How far along the current glide the cursor is at `now`: 0 at its start, 1 when settled.
    private func glideFraction(now: UInt64) -> Double {
        if distance(glideFrom, target) >= config.maxLagMm {
            return 1   // too far behind to glide: close the gap at once
        }
        // Unsigned subtraction traps on underflow, and a driver must never trap on a clock
        // reading: a step taken at or before the glide started has not moved yet.
        let elapsedNs = now > glideStartNs ? now - glideStartNs : 0
        let linear = min(1, Double(elapsedNs) / Double(max(config.smoothingNs, 1)))
        return 1 - (1 - linear) * (1 - linear)   // ease-out
    }

    /// Ends the drag at the finger's last reported position, for the caller to post once.
    public mutating func finish() -> CGPoint {
        cursor = target
        glideFrom = target
        glideStartNs = 0
        return cursor
    }

    private func distance(_ a: CGPoint, _ b: CGPoint) -> Double {
        hypot((a.x - b.x) * mmPerUnit, (a.y - b.y) * mmPerUnit)
    }
}
