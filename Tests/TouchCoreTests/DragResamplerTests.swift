import CoreGraphics
import Foundation
import Testing
@testable import TouchCore

private let ms: UInt64 = 1_000_000
private let tick: UInt64 = 8_333_333   // 120 Hz
/// 1920 points over 221.9 mm: the JN-MD-IPST105FP.
private let pointsPerMm = 1920.0 / 221.9
private func mm(_ value: Double) -> Double { value * pointsPerMm }

private func make() -> DragResampler { DragResampler(start: .zero, mmPerUnit: 1 / pointsPerMm) }

/// Drives the resampler for `durationNs`, feeding `samples` (time, position) as they come due.
/// Returns every posted position.
private func run(_ resampler: inout DragResampler, samples: [(atNs: UInt64, x: Double)], from startNs: UInt64 = 0, durationNs: UInt64) -> [CGPoint] {
    var posted: [CGPoint] = []
    var next = 0
    var clock = startNs
    while clock <= startNs + durationNs {
        while next < samples.count, samples[next].atNs <= clock {
            resampler.sample(CGPoint(x: samples[next].x, y: 0), atNs: samples[next].atNs)
            next += 1
        }
        if let point = resampler.step(atNs: clock) { posted.append(point) }
        clock += tick
    }
    return posted
}

/// The recorded texture of a slow drag (probe 2026-11-24): a 59 Hz stream that repeats its
/// position every other frame. Posted as reported, that is about 30 updates a second in
/// bursts; the resampler must post evenly and stay within one step of the finger.
@Test func aBurstyStreamBecomesEvenMotion() {
    var samples: [(atNs: UInt64, x: Double)] = []
    var x = 0.0
    for frame in 0..<60 {
        if frame % 2 == 0 { x = mm(Double(frame) * 0.7) }   // a new position every other frame
        samples.append((UInt64(frame) * 17 * ms, x))
    }
    var resampler = make()
    let posted = run(&resampler, samples: samples, durationNs: 1_020 * ms)

    // Far more real updates than the 30 distinct positions the stream carried.
    #expect(posted.count > 75)
    // Never ahead of the finger.
    for point in posted { #expect(point.x <= x + 0.01) }
    // Monotonic, and no single posted step larger than the largest finger step.
    let steps = zip(posted, posted.dropFirst()).map { $1.x - $0.x }
    #expect(steps.allSatisfy { $0 >= 0 && $0 <= mm(1.4) + 0.01 })
    // Settles exactly on the finger's final position.
    let last = resampler.finish()
    #expect(abs(last.x - x) < 1e-9)
}

@Test func theCursorStaysWithinOneFingerStepOfTheTarget() {
    var resampler = make()
    var worstLag: Double = 0
    for frame in 0..<120 {
        // A report every 17 ms, a 120 Hz tick between each pair (the live ordering).
        let reportNs = UInt64(frame) * 17 * ms
        let x = mm(Double(frame) * 0.9)
        resampler.sample(CGPoint(x: x, y: 0), atNs: reportNs)
        if let point = resampler.step(atNs: reportNs + tick) { worstLag = max(worstLag, (x - point.x) / pointsPerMm) }
        if let point = resampler.step(atNs: reportNs + 2 * tick) { worstLag = max(worstLag, (x - point.x) / pointsPerMm) }
    }
    // 0.9 mm per frame: the cursor trails by no more than about one frame of travel.
    #expect(worstLag < 1.2)
}

@Test func aStepBeforeTheGlideStartsMovesNothingAndNeverTraps() {
    // Unsigned clock arithmetic traps on underflow; this runs on a driver's input path, where a
    // trap kills touch input. A step clocked before the glide began is simply no movement.
    var resampler = make()
    resampler.sample(CGPoint(x: mm(3), y: 0), atNs: 1_000_000_000)
    #expect(resampler.step(atNs: 0) == nil, "no movement before the glide begins")
    #expect(resampler.step(atNs: 500_000_000) == nil)
    // And it recovers: the glide runs from its own start time, not from the first step.
    #expect(resampler.step(atNs: 1_000_000_000 + 34 * ms) == CGPoint(x: mm(3), y: 0))
}

@Test func aStillFingerProducesNoPosts() {
    var resampler = make()
    let posted = run(&resampler, samples: [(atNs: 0, x: 0), (atNs: 17 * ms, x: 0), (atNs: 34 * ms, x: mm(0.05))], durationNs: 200 * ms)
    #expect(posted.isEmpty, "0.05 mm is inside the dead zone; nothing twitches")
}

@Test func tremorInsideTheDeadZoneNeverMovesTheCursor() {
    var resampler = make()
    var posted: [CGPoint] = []
    for frame in 0..<200 {
        // ±0.1 mm wobble, alternating: a finger resting on a title bar.
        resampler.sample(CGPoint(x: mm(frame.isMultiple(of: 2) ? 0.1 : 0.0), y: 0), atNs: UInt64(frame) * 17 * ms)
        for offset in 0..<2 {
            if let point = resampler.step(atNs: UInt64(frame) * 17 * ms + UInt64(offset) * tick) { posted.append(point) }
        }
    }
    #expect(posted.isEmpty)
}

@Test func tremorThatAccumulatesPastTheDeadZoneDoesMove() {
    // The dead zone is a rest filter, not a trap: a finger that drifts 0.1 mm a frame for a
    // while has moved, and the cursor must follow it.
    var resampler = make()
    var moved = false
    for frame in 0..<200 {
        resampler.sample(CGPoint(x: mm(Double(frame) * 0.1), y: 0), atNs: UInt64(frame) * 17 * ms)
        if resampler.step(atNs: UInt64(frame) * 17 * ms + 2 * tick) != nil { moved = true }
    }
    #expect(moved)
}

@Test func aFarTargetIsReachedAtOnce() {
    var resampler = make()
    resampler.sample(CGPoint(x: mm(40), y: 0), atNs: 0)
    #expect(resampler.step(atNs: tick) == CGPoint(x: mm(40), y: 0), "40 mm is a relocation, not a glide")
}

@Test func theDragEndsExactlyWhereTheFingerLifted() {
    var resampler = make()
    resampler.sample(CGPoint(x: mm(30), y: 0), atNs: 0)
    _ = resampler.step(atNs: tick)   // part-way there
    resampler.sample(CGPoint(x: mm(31), y: 0), atNs: 17 * ms)
    #expect(resampler.finish() == CGPoint(x: mm(31), y: 0))
}

@Test func thePressStartsExactlyWhereItLanded() {
    var resampler = make()
    #expect(resampler.step(atNs: 0) == nil, "a press has no glide: the cursor is already under the finger")
}
