import Testing
@testable import TouchCore

private let width = 221.9
private let height = 147.9
private let ms: UInt64 = 1_000_000

private func mouseFrame(_ x: Double, _ y: Double, touching: Bool = true) -> TouchFrame {
    TouchFrame(source: .mouse, contacts: [Contact(id: 0, touching: touching, x: x, y: y)], scanTime: nil)
}

@Test func smallMouseMovementPassesThrough() {
    var filter = SingleTouchFilter()
    #expect(filter.filter(mouseFrame(0.5, 0.5), atNs: 0, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.5, 0.5)])
    #expect(filter.filter(mouseFrame(0.52, 0.5), atNs: 17 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.52, 0.5)])
}

@Test func aSlowPressGlitchIsDiscardedAndThePositionHeld() {
    // Probe 2026-11-24: single-frame jumps up to 129 mm while the tip stays down. libinput's
    // jump rule: 20 mm per 12 ms normalized (7 mm growth) is not finger motion — discard it.
    var filter = SingleTouchFilter()
    _ = filter.filter(mouseFrame(0.5, 0.5), atNs: 0, panelWidthMm: width, panelHeightMm: height)
    _ = filter.filter(mouseFrame(0.51, 0.5), atNs: 17 * ms, panelWidthMm: width, panelHeightMm: height)
    let frames = filter.filter(mouseFrame(0.9, 0.5), atNs: 34 * ms, panelWidthMm: width, panelHeightMm: height)
    #expect(frames == [mouseFrame(0.51, 0.5)])
    // A report 2 mm from the held point is real finger motion again, and passes.
    #expect(filter.filter(mouseFrame(0.5, 0.5), atNs: 51 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.5, 0.5)])
}

@Test func aFastFlickPasses() {
    // 0.04 x-units per 17 ms ≈ 520 mm/s: under the 20 mm/12 ms normalized ceiling.
    var filter = SingleTouchFilter()
    _ = filter.filter(mouseFrame(0.50, 0.5), atNs: 0, panelWidthMm: width, panelHeightMm: height)
    #expect(filter.filter(mouseFrame(0.54, 0.5), atNs: 17 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.54, 0.5)])
    #expect(filter.filter(mouseFrame(0.58, 0.5), atNs: 34 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.58, 0.5)])
}

@Test func aPersistentRelocationIsAcceptedAfterTheCap() {
    var filter = SingleTouchFilter(jumpAcceptCap: 3)
    _ = filter.filter(mouseFrame(0.1, 0.1), atNs: 0, panelWidthMm: width, panelHeightMm: height)
    for t in 1...2 {
        #expect(filter.filter(mouseFrame(0.8, 0.7), atNs: UInt64(t) * 17 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.1, 0.1)])
    }
    #expect(filter.filter(mouseFrame(0.8, 0.7), atNs: 3 * 17 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.8, 0.7)])
}

@Test func aStaleSampleBypassesTheJumpTest() {
    // libinput: frames staler than 30 ms are not jump evidence (the finger may be anywhere).
    var filter = SingleTouchFilter()
    _ = filter.filter(mouseFrame(0.1, 0.1), atNs: 0, panelWidthMm: width, panelHeightMm: height)
    #expect(filter.filter(mouseFrame(0.9, 0.9), atNs: 200 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.9, 0.9)])
}

@Test func liftAndDigitizerFramesResetTheRepairState() {
    var filter = SingleTouchFilter()
    _ = filter.filter(mouseFrame(0.1, 0.1), atNs: 0, panelWidthMm: width, panelHeightMm: height)
    _ = filter.filter(mouseFrame(0.1, 0.1, touching: false), atNs: 17 * ms, panelWidthMm: width, panelHeightMm: height)
    // A new press starts from its own position, not held against the old one.
    #expect(filter.filter(mouseFrame(0.8, 0.7), atNs: 100 * ms, panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.8, 0.7)])

    let digitizer = TouchFrame(source: .digitizer, contacts: [
        Contact(id: 1, touching: true, x: 0.1, y: 0.1),
        Contact(id: 2, touching: true, x: 0.9, y: 0.9),
    ], scanTime: 5)
    #expect(filter.filter(digitizer, atNs: 120 * ms, panelWidthMm: width, panelHeightMm: height) == [digitizer])
}

@Test func aLingerBeforeMovingIsAHoldDragAndASlowSwipeScrolls() {
    // 100 ms of drift past 1 mm, then clear travel: scrolls (previously it selected text,
    // because the drag was classified by touch age instead of by stillness).
    var recognizer = GestureRecognizer(config: GestureConfig())
    _ = recognizer.handle([ContactEvent(id: 0, phase: .began, x: 0.5, y: 0.5)], atNs: 0)
    _ = recognizer.handle([ContactEvent(id: 0, phase: .moved, x: 0.508, y: 0.5)], atNs: 100_000_000)
    let actions = recognizer.handle([ContactEvent(id: 0, phase: .moved, x: 0.59375, y: 0.5)], atNs: 600_000_000)
    #expect(actions.first == .scroll(dx: 0.09375, dy: 0, phase: .began))

    // A finger still for 700 ms and only then moving: click-and-drag.
    var planted = GestureRecognizer(config: GestureConfig())
    _ = planted.handle([ContactEvent(id: 0, phase: .began, x: 0.5, y: 0.5)], atNs: 0)
    let holdActions = planted.handle([ContactEvent(id: 0, phase: .moved, x: 0.65, y: 0.5)], atNs: 700_000_000)
    #expect(holdActions.first == .dragBegan(at: PanelPoint(x: 0.5, y: 0.5), hold: true))
}
