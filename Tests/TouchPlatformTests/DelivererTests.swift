import CoreGraphics
import Foundation
import Testing
@testable import TouchCore
@testable import TouchPlatform

/// Records what the deliverer does instead of touching the window server.
final class FakeEffects: DeliveryEffects, @unchecked Sendable {
    var posted: [(type: CGEventType, location: CGPoint, scrollPhase: Int64, momentumPhase: Int64, target: Deliverer.Target, subtype: Int64)] = []
    var cursor = CGPoint(x: 100, y: 100)   // starts on the MacBook screen
    var hidden = false
    var activated: [pid_t] = []
    var frontmost: pid_t? = 1
    var press: PressRecord?
    var running: [pid_t: String] = [42: "com.example.app"]

    func post(_ event: CGEvent, to target: Deliverer.Target) {
        posted.append((event.type, event.location,
                       event.getIntegerValueField(CGEventField(rawValue: 99)!),
                       event.getIntegerValueField(CGEventField(rawValue: 123)!), target,
                       event.getIntegerValueField(CGEventField(rawValue: 110)!)))
    }
    func cursorLocation() -> CGPoint? { cursor }
    func warpCursor(to point: CGPoint) { cursor = point }
    func reassociateCursor() {}
    func hideCursor() { hidden = true }
    func showCursor() { hidden = false }
    func frontmostApp() -> pid_t? { frontmost }
    func activate(_ pid: pid_t) { activated.append(pid); frontmost = pid }
    func recordPress(_ record: PressRecord) { press = record }
    func clearPress() { press = nil }
    func pendingPress() -> PressRecord? { press }
    func runningApp(_ pid: pid_t) -> String? { running[pid] }
}

private let arzopaPoint = CGPoint(x: 2500, y: 500)
private let direct = Deliverer.Target.direct(DirectTarget(pid: 42, window: 7, origin: CGPoint(x: 2000, y: 100)))

private func make() -> (Deliverer, FakeEffects, DispatchQueue) {
    let queue = DispatchQueue(label: "deliverer-test")
    let effects = FakeEffects()
    let deliverer = Deliverer(queue: queue, effects: effects)
    // These tests assert on what each call posts; the drag resampler posts from a timer instead
    // and has its own tests below.
    deliverer.dragResampling = false
    return (deliverer, effects, queue)
}

private let pointPerMm = 8.65   // 1920 points over 221.9 mm (the JN-MD-IPST105FP)
private func mm(_ value: Double) -> CGFloat { value * pointPerMm }

/// Waits long enough for the 120 Hz drag timer to have ticked a few times.
private func letTheDragTimerTick() {
    Thread.sleep(forTimeInterval: Deliverer.dragTickInterval * 6)
}

@Test func aResampledDragIsPostedByTheTimerAndEndsWhereTheFingerLifted() {
    let queue = DispatchQueue(label: "deliverer-resample")
    let fx = FakeEffects()
    let d = Deliverer(queue: queue, effects: fx)
    d.dragResampling = true
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.dragDown(at: arzopaPoint)
    }
    // Nothing glides at the press: the cursor is under the finger.
    letTheDragTimerTick()
    queue.sync {}
    #expect(fx.posted.map(\.type.rawValue) == [1], "only the left mouse-down")

    // A slow drag: 59 Hz reports with every other frame repeated (the panel's recorded texture).
    for frame in 0..<12 {
        let x = frame.isMultiple(of: 2) ? mm(Double(frame) * 0.7) : mm(Double(frame - 1) * 0.7)
        queue.sync { d.dragMove(to: CGPoint(x: arzopaPoint.x + x, y: arzopaPoint.y)) }
        Thread.sleep(forTimeInterval: 0.017)
    }
    letTheDragTimerTick()
    queue.sync { d.dragUp(at: CGPoint(x: arzopaPoint.x + mm(7.7), y: arzopaPoint.y)) }
    letTheDragTimerTick()
    queue.sync {}

    let moves = fx.posted.filter { $0.type == .leftMouseDragged }.map(\.location.x)
    #expect(moves.count > 12, "more cursor updates than distinct reported positions (6), posted at display rate")
    #expect(zip(moves, moves.dropFirst()).allSatisfy { $1 >= $0 }, "monotonic: never backwards, never ahead of the finger")
    #expect(abs((moves.last ?? 0) - (arzopaPoint.x + mm(7.7))) < 0.01, "the drag ends exactly where the finger lifted")
    #expect(fx.posted.last?.type == .leftMouseUp)
}

@Test func aResampledDragPostsNothingWhileTheFingerIsStill() {
    let queue = DispatchQueue(label: "deliverer-resample-still")
    let fx = FakeEffects()
    let d = Deliverer(queue: queue, effects: fx)
    d.dragResampling = true
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.dragDown(at: arzopaPoint)
        d.dragMove(to: arzopaPoint)   // same position again
    }
    letTheDragTimerTick()
    queue.sync {}
    #expect(fx.posted.count == 1, "mouse-down only; a still finger posts nothing")
    queue.sync { d.dragUp(at: arzopaPoint) }
    queue.sync {}
    #expect(fx.posted.map(\.type.rawValue) == [1, 2], "mouse-down, then mouse-up: no drag events for a still finger")
}

@Test func abortStopsTheDragTimer() {
    let queue = DispatchQueue(label: "deliverer-resample-abort")
    let fx = FakeEffects()
    let d = Deliverer(queue: queue, effects: fx)
    d.dragResampling = true
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.dragDown(at: arzopaPoint)
        d.dragMove(to: CGPoint(x: arzopaPoint.x + mm(20), y: arzopaPoint.y))
        d.abortGesture()
    }
    letTheDragTimerTick()
    queue.sync {}
    #expect(fx.posted.last?.type == .leftMouseUp, "nothing is posted after the abort")
    let count = fx.posted.count
    letTheDragTimerTick()
    queue.sync {}
    #expect(fx.posted.count == count, "the timer is gone")
}

// Review finding 1: a Direct gesture that interrupts a Warp scroll's momentum left the cursor hidden.
@Test func aDirectGestureAfterWarpMomentumGivesTheCursorBack() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(.warp, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 0, dy: 10, phase: .began)
        d.scrollEnded(velocityX: 0, velocityY: 3000)   // momentum running, cursor parked at the finger
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: true)
    }
    #expect(fx.hidden == false)
    #expect(fx.cursor == CGPoint(x: 100, y: 100), "back where it was on the MacBook screen")
}

@Test func aWarpTapReturnsTheCursorShortlyAfter() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(.warp, at: arzopaPoint, appPID: 42, activate: true)
        d.click(at: arzopaPoint, count: 1, button: .left)
        d.end()
    }
    #expect(fx.hidden == true, "stays at the finger for a moment")
    Thread.sleep(forTimeInterval: Deliverer.warpReturnDelay + 0.1)
    queue.sync {}
    #expect(fx.hidden == false)
    #expect(fx.cursor == CGPoint(x: 100, y: 100))
}

// Launchpad and Space switching open where the cursor is: it waits on the touch display until the animation is under way.
@Test func aSystemGestureKeepsTheCursorParkedForTheRequestedTime() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(.warp, at: arzopaPoint, appPID: nil, activate: false)
        d.end(returnDelay: 0.4)
    }
    Thread.sleep(forTimeInterval: Deliverer.warpReturnDelay + 0.1)
    queue.sync {}
    #expect(fx.hidden == true)
    #expect(fx.cursor == arzopaPoint, "still on the touch display")
    Thread.sleep(forTimeInterval: 0.4)
    queue.sync {}
    #expect(fx.hidden == false)
    #expect(fx.cursor == CGPoint(x: 100, y: 100))
    #expect(fx.posted.isEmpty, "parking posts nothing")
}

// Launchpad opens where the Dock is; the Dock moves to a display when the cursor pushes against its bottom edge.
@Test func aNudgePushesTheParkedCursorThroughTheWindowServer() {
    let (d, fx, queue) = make()
    let edge = CGPoint(x: 2688, y: 1079)
    queue.sync {
        d.begin(.warp, at: edge, appPID: nil, activate: false)
        d.nudge(at: edge, dx: 0, dy: 4)
    }
    #expect(fx.posted.count == 1)
    #expect(fx.posted.first?.type == .mouseMoved)
    #expect(fx.posted.first?.location == edge)
    #expect(fx.posted.first?.target == .warp)
    #expect(fx.hidden == true, "the cursor stays hidden while it pushes")
}

@Test func aNudgeInDryRunPostsNothing() {
    let (d, fx, queue) = make()
    queue.sync {
        d.dryRun = true
        d.nudge(at: CGPoint(x: 2688, y: 1079), dx: 0, dy: 4)
    }
    #expect(fx.posted.isEmpty)
}

// Safari's page swipe measures progress from the trackpad's gesture stream, not the scroll events (Event Sink, S17).
@Test func aTwoFingerScrollCarriesTheTrackpadsGestureStream() {
    let (d, fx, queue) = make()
    queue.sync {
        d.scrollGestureStream = true
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 2, dy: 0, phase: .began)
        d.scroll(dx: 18, dy: 0, phase: .changed)
        d.scrollEnded(velocityX: 0, velocityY: 0)
    }
    // scroll began, gesture begin (61); scroll changed, gesture move (6); scroll ended, gesture move ended (6), gesture end (62)
    #expect(fx.posted.map { $0.type.rawValue } == [22, 29, 22, 29, 22, 29, 29])
    #expect(fx.posted.filter { $0.type.rawValue == 29 }.map(\.subtype) == [61, 6, 6, 62])
}

@Test func withoutTheStreamAScrollIsPlain() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 2, dy: 0, phase: .began)
        d.scroll(dx: 18, dy: 0, phase: .changed)
        d.scrollEnded(velocityX: 0, velocityY: 0)
    }
    #expect(fx.posted.allSatisfy { $0.type.rawValue == 22 })
}

@Test func abortingAStreamedScrollClosesTheGesture() {
    let (d, fx, queue) = make()
    queue.sync {
        d.scrollGestureStream = true
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 2, dy: 0, phase: .began)
        d.abortGesture()
    }
    #expect(fx.posted.last?.subtype == 62, "the gesture is closed too")
}

// Crash safety (review finding 5): the journal holds enough to release a press where it was made.
private let journaledDirect = PressRecord(pid: 42, right: false, x: 2500, y: 500, window: 7, originX: 2000, originY: 100, bundleID: "com.example.app")

@Test func aDirectPressIsJournaledWithItsWindowPointAndApp() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.dragDown(at: arzopaPoint)
    }
    #expect(fx.press == journaledDirect)
}

@Test func aCrashedDirectPressIsReleasedOnItsWindowWhereItWasPressed() {
    let (d, fx, queue) = make()
    fx.press = journaledDirect
    queue.sync { d.releaseStuckPress() }
    #expect(fx.posted.count == 1)
    #expect(fx.posted.last?.type == .leftMouseUp)
    #expect(fx.posted.last?.location == arzopaPoint, "where it was pressed, not at the cursor")
    #expect(fx.posted.last?.target == direct, "on the window it was pressed in")
    #expect(fx.press == nil)
    #expect(fx.cursor == CGPoint(x: 100, y: 100), "the cursor never moves")
}

@Test func aCrashedPressIsDroppedWhenItsAppIsGoneOrThePidWasReused() {
    for running: [pid_t: String] in [[:], [42: "com.other.app"]] {
        let (d, fx, queue) = make()
        fx.press = journaledDirect
        fx.running = running
        queue.sync { d.releaseStuckPress() }
        #expect(fx.posted.isEmpty, "\(running)")
        #expect(fx.press == nil)
    }
}

@Test func aCrashedWarpPressIsReleasedWhereItWasPressed() {
    let (d, fx, queue) = make()
    fx.press = PressRecord(pid: 0, right: false, x: 2500, y: 500)
    queue.sync { d.releaseStuckPress() }
    #expect(fx.posted.last?.type == .leftMouseUp)
    #expect(fx.posted.last?.location == arzopaPoint, "a drag under way drops back where it started")
    #expect(fx.posted.last?.target == .warp)
    Thread.sleep(forTimeInterval: Deliverer.warpReturnDelay + 0.1)
    queue.sync {}
    #expect(fx.cursor == CGPoint(x: 100, y: 100))
    #expect(fx.hidden == false)
}

@Test func anOldJournalWithoutAPointIsReleasedAtTheCursorAsBefore() {
    let (d, fx, queue) = make()
    fx.press = PressRecord(pid: 0, right: true)
    queue.sync { d.releaseStuckPress() }
    #expect(fx.posted.last?.type == .rightMouseUp)
    #expect(fx.posted.last?.location == CGPoint(x: 100, y: 100))
}

// Review findings 3 and 4: stopping or losing the display mid-gesture must close everything.
@Test func abortDuringAWarpDragReleasesTheButtonAndTheCursor() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(.warp, at: arzopaPoint, appPID: 42, activate: true)
        d.dragDown(at: arzopaPoint)
        d.dragMove(to: CGPoint(x: 2550, y: 520))
        d.abortGesture()
    }
    #expect(fx.posted.last?.type == .leftMouseUp)
    #expect(fx.press == nil)
    #expect(fx.hidden == false)
    #expect(fx.cursor == CGPoint(x: 100, y: 100))
}

@Test func abortDuringADirectScrollEndsTheScrollPhase() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 0, dy: 5, phase: .began)
        d.abortGesture()
    }
    let last = fx.posted.last
    #expect(last?.type == .scrollWheel)
    #expect(last?.scrollPhase == 4, "kCGScrollPhaseEnded")
}

@Test func abortDuringMomentumStopsIt() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 0, dy: 10, phase: .began)
        d.scrollEnded(velocityX: 0, velocityY: 3000)
        d.abortGesture()
    }
    let count = queue.sync { fx.posted.count }
    #expect(fx.posted.last?.momentumPhase == 3, "momentum end")
    Thread.sleep(forTimeInterval: 0.1)
    #expect(queue.sync { fx.posted.count } == count, "no momentum events after the abort")
}

@Test func abortDuringAPinchEndsIt() {
    let (d, fx, queue) = make()
    queue.sync {
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: true)
        d.magnify(0.1, phase: .began)
        d.abortGesture()
    }
    #expect(fx.posted.count == 2, "began + ended")
}

@Test func abortWithNothingOpenDoesNothing() {
    let (d, fx, queue) = make()
    queue.sync { d.abortGesture() }
    #expect(fx.posted.isEmpty)
    #expect(fx.hidden == false)
}

// Trackpad setting "momentum scrolling" off: a flick ends without momentum.
@Test func withMomentumOffAFlickEndsWithoutMomentum() {
    let (d, fx, queue) = make()
    queue.sync {
        d.momentumEnabled = false
        d.begin(direct, at: arzopaPoint, appPID: 42, activate: false)
        d.scroll(dx: 0, dy: 10, phase: .began)
        d.scrollEnded(velocityX: 0, velocityY: 3000)
    }
    Thread.sleep(forTimeInterval: 0.05)
    #expect(queue.sync { fx.posted.allSatisfy { $0.momentumPhase == 0 } })
    #expect(fx.posted.last?.scrollPhase == 4)
}

// Owner's choice 2026-10-11: Launchpad by touch borrows the Dock and gives it back.
@Test func launchpadBringsTheDockOverThenBackByDefault() {
    #expect(PipelineSettings().launchpadDock == .bringDockAndReturn)
}

// Owner's choice 2026-10-11: a Space switch by touch keeps focus on the other display, as a setting.
@Test func aSpaceSwitchKeepsFocusByDefault() {
    #expect(PipelineSettings().keepFocusOnSpaceSwitch == true)
}

// Owner's choice 2026-10-11: two-finger page swipes, on by default, with a menu switch.
@Test func swipeBetweenPagesIsOnByDefault() {
    #expect(PipelineSettings().swipeBetweenPages == true)
}

@Test func theJournalRoundTripsAPressAndReadsTheOldFormat() throws {
    let url = FileManager.default.temporaryDirectory.appending(path: "touchnext-journal-\(UUID().uuidString).json")
    defer { try? FileManager.default.removeItem(at: url) }
    let journal = PressJournal(url: url)
    journal.record(journaledDirect)
    #expect(journal.pending() == journaledDirect)
    journal.clear()
    #expect(journal.pending() == nil)
    try Data(#"{"pid":321,"right":true}"#.utf8).write(to: url)   // written by versions before 0.6.3
    #expect(journal.pending() == PressRecord(pid: 321, right: true))
}
