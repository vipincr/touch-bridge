import Testing
@testable import TouchCore

private let width = 221.9
private let height = 147.9

private func mouseFrame(_ x: Double, _ y: Double, touching: Bool = true) -> TouchFrame {
    TouchFrame(source: .mouse, contacts: [Contact(id: 0, touching: touching, x: x, y: y)], scanTime: nil)
}

@Test func smallMouseMovementPassesThrough() {
    var filter = SingleTouchFilter()
    #expect(filter.filter(mouseFrame(0.5, 0.5), panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.5, 0.5)])
    #expect(filter.filter(mouseFrame(0.52, 0.5), panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.52, 0.5)])
}

@Test func aTeleportSplitsIntoLiftAndFreshTouch() {
    var filter = SingleTouchFilter()
    _ = filter.filter(mouseFrame(0.1, 0.1), panelWidthMm: width, panelHeightMm: height)
    let frames = filter.filter(mouseFrame(0.8, 0.7), panelWidthMm: width, panelHeightMm: height)
    #expect(frames.count == 2)
    #expect(frames[0].contacts == [Contact(id: 0, touching: false, x: 0.1, y: 0.1)])
    #expect(frames[1].contacts == [Contact(id: 0, touching: true, x: 0.8, y: 0.7)])
}

@Test func aLiftThenTouchElsewhereIsNotSplit() {
    var filter = SingleTouchFilter()
    _ = filter.filter(mouseFrame(0.1, 0.1), panelWidthMm: width, panelHeightMm: height)
    _ = filter.filter(mouseFrame(0.1, 0.1, touching: false), panelWidthMm: width, panelHeightMm: height)
    #expect(filter.filter(mouseFrame(0.8, 0.7), panelWidthMm: width, panelHeightMm: height) == [mouseFrame(0.8, 0.7)])
}

@Test func digitizerFramesPassThroughUntouched() {
    var filter = SingleTouchFilter()
    let frame = TouchFrame(source: .digitizer, contacts: [
        Contact(id: 1, touching: true, x: 0.1, y: 0.1),
        Contact(id: 2, touching: true, x: 0.9, y: 0.9),
    ], scanTime: 5)
    #expect(filter.filter(frame, panelWidthMm: width, panelHeightMm: height) == [frame])
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
