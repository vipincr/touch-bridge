# Changelog

## 2.0.0 (2026-11-25) — Rust port

- **TouchNext is written in Rust now.** `rust/` holds the whole driver in four crates: `touchcore`
  (the gesture logic, with no system framework in it), `touchhid` (the panel over IOKit HID),
  `touchplatform` (macOS I/O: displays, windows, event synthesis, Direct and Warp delivery, system
  gestures, the pipeline) and `touchnext` (the menu-bar app and its diagnostics). The menu, log
  lines and command lines are those of 1.2.0, and so are the gestures and thresholds but for the
  drags and the stream's repair below.
- **Slow drags are recognised.** The radius in which a finger counts as stationary grew at 25 mm/s
  for as long as the finger was down, so a drag at or below that speed never became one: nothing
  moved, and the lift after half a second was a right-click. On a window's drag area (toolbar,
  title bar, title text) the 8 mm tap slop is the whole test, however long the finger has been
  down. Anywhere else the radius went on growing to 16.75 mm by the long-press time until the
  evening of 2026-11-26 (see "A slow drag inside a window scrolls" below).
- **A window drag is pressed where the finger is.** A drag is recognised only after some travel,
  and the window was pressed where the finger had landed and then moved to where it was: it jumped
  the dead travel. The press is now made where the finger is when the drag is recognised, as long
  as that is still a drag area.
- **The panel's reports are repaired before a touch is tracked.** In mouse mode the JN-MD-IPST105FP
  misreports. Over three recordings (2 100 reports, 2026-11-24 and 2026-11-25): 7 frames in 100
  with the tip down put the finger 10 to 200 mm from where it was, 6 in 100 are followed by one to
  eight tip-off reports while the finger is still down, and around each landing and lift come one
  to seven frames of a touch somewhere else. The port took each report as it came, as 1.2.0 did: a
  frame far from the finger was the finger, and the recognizer's drag grace then resumed a drag
  when a touch came near. Owner, 2026-11-25: "i dragged around this console window, when trying to
  tap the titlebar of the telegram app, it just goes to the background". A jump filter
  (`SingleTouchFilter`, behind the new `ContactStream`) now stands before the contact tracker. A
  frame is believed at once only within 6 mm of where the finger was heading. Any other is held
  until it has support: two frames that move as a finger can, three on one place, five frames on
  their own. While the finger is down and no lift report has come, a place it could not have got
  to is never taken for it on the panel's word alone, however long the panel keeps to it (it kept
  to one misread for seven frames); after fifteen such frames the finger is given up for lost. A
  new press is believed on its third frame, or on its second when its lift is reported.
- **A lift report is held for longer.** The tracker held one for 60 ms at the most. It now holds
  it for 250 ms for a finger that was moving (measured: a finger whose tip came back 194 ms after
  its lift report) and for 150 ms for one standing still (measured: back within 106 ms), and ends
  the touch when the panel has then been quiet for 50 ms. A tip back by then continues the press
  only within a finger's travel of where it was.
- On the recording of that drag and those taps (31 s, 806 reports), the build before made 44
  contacts, 27 taps and 11 scrolls; the first of these two repairs made 22, 12 and 8, and this
  one 22, 14 and 7. The three taps on a title bar are three clicks there. Before, two of the five
  clicks in that time were on the desktop, 107 mm and 98 mm to the left of it.
- **A lift report from the panel's origin no longer ends a touch, and a tap is judged while the
  finger was there.** (2026-11-26) The first repair ended a touch at the first lift report that
  gave the panel's origin, taking it for the panel saying the finger had left. The panel sends
  that report before a finger lands and while one is still down and moving: in 44 s of the
  owner's use (650 reports, 35 of them from the origin) it came in three bursts before a touch,
  in three after one, and in the middle of both drags. Both drags were let go at it, 1.8 s and
  54 ms in, and the rest of each came out as a scroll and as taps, the last of them 1 pt from a
  window's minimize button. Owner, 2026-11-26: "it is still hard to touch/grab". The report is
  held like any other now. It ends the touch at once only when a lift report from where the
  finger was is already being held: the two together do say the finger has left. With every
  report held and nothing more, a second tap 212 ms after the first one's lift report was taken
  for the first one's finger coming back, and the two were one touch long enough to right-click.
  The 150 ms hold for a still finger keeps a tap that the panel goes on reporting after from
  waiting 250 ms or, on the panel's own places near the finger, 460 ms. And the recognizer
  judges a tap and a long press by the time the finger was last seen: the time the tracker held
  the lift report was counted into the touch, so a tap of 233 ms followed by half a second of
  the panel's own reports was 750 ms long. Replayed, that recording makes the build before one
  long press and this one none.
- **A slow drag inside a window scrolls.** (2026-11-26, 18:16) Anywhere but a window's drag area,
  the radius in which a finger counted as stationary grew to 16.75 mm by half a second, so that a
  finger held to right-click had room to settle. The owner's slow drags are made at about the rate
  it grew at: land, wait 150 to 350 ms, move at 25 mm/s, lift. On the build of 16:01 they scrolled
  a terminal for 34 s with small nudges (492 reports, seven touches). One travelled 11.7 mm and
  never left the radius: nothing scrolled, and the lift at 913 ms was a right-click. Another
  began with 6.7 mm and scrolled at 679 ms, after 16 mm. The slop is the 8 mm of a tap
  everywhere now, however long the finger is down. The two scroll 387 ms and 266 ms after the
  tracker began the touch, and the recording makes six scrolls and a tap. The same rule had made
  a right-click of the slow drag on a title bar at 24.211 s of 2026-11-25 (in a replay, which has
  no drag areas; that recording makes 8 scrolls and no long press now, 7 and one before) and a
  click of a 10 mm drag in the multi-finger probe (18.097 s). With the slop flat, a report of the
  panel's is judged where it leaves the finger. After the tap at 10.917 s of the 44 s recording
  the tracker passed on two places of the panel's, in one report each: 8 mm out and 7.5 mm back,
  then 12 mm the other way and 6 mm back. Judged on each place, that tap scrolled; it is a tap.
  A finger held 3.6 s to right-click later that evening, at the panel's left edge, wandered 5 mm
  along it as it lifted, and is a right-click still (long-press-2026-11-26).
- **The drag grace is off.** The Swift build, and the port until now, held each drag's press for
  1.2 s after the lift and resumed the drag when a touch came near. The tracker now carries a
  contact through the panel's tip flaps, so a lift that reaches the recognizer is one, and the
  next touch is a new gesture. With the grace on, that next touch was lost: measured on
  2026-11-25, a tap after a drag was taken for the drag resuming, and the window moved 68 pt.
- **A debug build keeps the raw panel reports.** Each run writes every report it is given to
  `~/Library/Logs/TouchNext/reports-<date>-<time>.log`, in the probe's format, and the log names
  the file. `--replay` runs it again, and `--replay … --frames` prints every position the
  recognizer was given. The log also says where each finger began and ended, and names each press
  and drag the app posted.
- The tracker no longer subtracts a frame time from an earlier one. Where the pipeline's clock and
  the panel's stamps crossed, a checked build panicked and a release build read the held lift as
  overdue.
- **Build with `cargo`.** `scripts/bundle.sh` runs `cargo build --release` and signs the result as
  before. A Rust toolchain (1.88 or later) replaces the Swift one, and `cargo test` replaces
  `swift test`.
- **Timers became deadlines.** The pipeline, the deliverer and the Dock swipe synthesizer each keep
  their state in one struct and say when it can next change; one thread sleeps until a report
  arrives or that moment passes. The Swift ran a 50 ms timer on the pipeline queue for as long as
  the app was open. While reports come, the pipeline runs on their stamps, so a slow Accessibility
  query between two of them no longer moves a lift.
- **What the port leaves out:** the drag resampler (`DragResampler`, off by default in 1.2.0, with
  its tests), `GestureConfig.tapSlopMm` (declared and never read) and the pipeline's `gestureStart`
  (stored and never read).
- Two texts no longer name the earlier panel: the menu's Display line waits for "the touch
  display", and the probe's summary says "touch the screen".
- **The log file and the diagnostics are a build option.** They exist only in a build with the
  Cargo feature `debug`: `./scripts/bundle.sh --debug`, or `./scripts/install.sh --debug` to
  install one for as long as a problem is being looked into. The default build is the prod build.
  It opens no log file, builds no log line and has no diagnostic modes; given one of their
  arguments, it says so and exits. The menu's status lines are in both builds; "Open Event Sink"
  and "Open Logs Folder" are in the menu of a debug build only. Measured on the development Mac
  (20 000 samples, release build): one log line cost 1.4 to 2.5 µs on the pipeline's thread, one
  sample in a hundred past 5 µs and the worst at 1.4 ms, and a gesture wrote a few.
- **The lines only the log reads are compiled out with it:** the deliverer's begin and dry-run
  lines, and the result lines a second after each system gesture (which also read the Spaces and
  the windows then). A status line the menu does not show no longer leaves the pipeline's thread
  in either build.
- **`--replay <recording>`** (debug build) runs a probe recording through the pipeline in dry run,
  at the speed it was recorded and with the saved menu settings, and prints every line the
  pipeline reports to the terminal. Each report is stamped for the moment the recording says it
  came, so one recording plays the same every time. It posts nothing and does not take the panel.
- **`--ax-probe --menu`** (debug build) lists the installed copy's menu as Accessibility reports
  it, status lines and check marks included. Each `--press "Title"` presses one item without
  moving the cursor and lists the menu again, so the app's menu can be driven from a script.
- **The press journal keeps its file open.** A press overwrites the file and a release empties it;
  before, each click created the file and each release removed it. Same measurement: 5 µs a click
  instead of 47 µs, and the worst click at 33 µs instead of 2 ms, on the path that leads to the
  mouse-down. The guarantee is unchanged (a crash can't leave a button held), and the file stays
  readable by the Swift app's journal.

 — JAPANNEXT JN-MD-IPST105FP support (vipincr fork)

- **New reference panel: JAPANNEXT JN-MD-IPST105FP** (10.5-inch IPS, 1920×1280, 5 reported contacts).
  Its WCH controller (`wch.cn`, USB `0x27C0:0x0859`) has its own report decoder: report ID `0x0D`,
  ten 5-byte slots with the contact id in the high nibble and the tip switch at bit 0, scan time
  LE16 and contact count at bytes 51/53 — all taken from the panel's live HID report descriptor.
- **That panel is single-touch on a Mac.** Its firmware withholds the digitizer stream from macOS:
  it decides at USB enumeration, from how the host reads string descriptors, and [TinyUSB issue
  4346](https://github.com/hathach/tinyusb/issues/4346) shows that on the same controller. TouchNext
  writes the Input Mode feature (report 0x21) at every panel attach; the controller acknowledges
  it and ignores it. Measured over raw USB on 2026-11-24 with a finger on the glass: the firmware
  stores no mode (the feature reads back the request packet), and neither Linux-style string reads
  nor a reconfigure without drivers made the digitizer interface report. TouchNext therefore reads
  the panel's mouse reports, and ignores a mouse report for a second after any digitizer frame so
  that a finger is never delivered twice. See the hardware note in the README.
- **Single-touch stream quality**, from recordings of that panel:
  - a lift report's coordinates are never used: the panel sends about one lift report in fourteen
    with (0, 0), which put taps on the Apple menu;
  - a coordinate jump beyond what a finger can travel in the time is discarded, as libinput does
    for touchpads;
  - a press that disappears mid-drag and returns within 1.2 s, near where it would have travelled,
    continues the drag.
- **Panel discovery is by HID class now**, not USB IDs: any USB touch-screen digitizer is matched
  (Apple's own devices excluded), and what was seized is logged with its product name and USB IDs.
- **The touch display falls back to the single external display** when its EDID vendor/model match
  nothing, so an unlisted panel works without editing settings.
- **Gesture thresholds use the panel's physical size**, set in one place (`PipelineSettings`,
  defaulting to the JN-MD-IPST105FP's 221.9 × 147.9 mm active area).
- The ILITEK path (ARZOPA, USB `0x222A:0x0001`) keeps working: same parser, plus the slot count
  now derives from the report length for shorter ILITEK-family panels. The ILITEK single-touch
  recovery feature reads only apply to ILITEK controllers.
- **Permissions are asked for.** TouchNext requests Input Monitoring with the system prompt whenever
  it is missing, as it already did for Accessibility, and restarts itself once both are granted. A
  failed seize names the permission it needs.
- **The build stops instead of signing ad-hoc** when its signing identity is not in the keychain.
  An ad-hoc build installed over a signed one takes the privacy grants away from it.
- Unit tests for both WCH report formats, and two recorded panel sessions replayed through the
  parser, the tracker and the recognizer.

## 1.1.0 (2026-10-11)

- **Touch Mode menu** (✋ → Touch Mode). It resets to Gestures at each start, and the menu-bar icon shows a special mode.
  - **Drawing** (✏️): the first finger is a pen, from touch-down to lift. Every other finger, a resting palm and every gesture are ignored. Strokes go through the window server, so drawing apps and websites see a real mouse press.
  - **Multitouch Apps** (🖐): every finger, up to ten, as TUIO 1.1 over UDP to `127.0.0.1:3333`, for TUIO apps such as TouchDesigner, Processing, openFrameworks, Unity, Max or Kivy. Nothing is delivered to macOS in this mode.
- **Event Sink as a multitouch test canvas:** it reads TUIO and paints every finger in its own colour, plus the mouse stroke in grey, with a live finger count and a Clear button.
- The README explains what macOS allows: ordinary apps, web pages and iPhone/iPad apps get a single pointer, so more fingers reach only apps that read a multitouch protocol such as TUIO.

## 1.0.0 (2026-10-11)

The first stable release. On the tested setup (an ARZOPA touch monitor with an ILITEK controller, macOS 15), the touch display behaves like a touchscreen with a trackpad built in:

- Touches land on the touch display, and for most gestures the cursor stays where it is.
- **Gestures:**
  - tap, double and triple tap, right-click (hold);
  - scrolling with momentum, pinch, rotate;
  - click-and-drag, three-finger drag, window moves by the title bar.
- **The trackpad's system gestures with the system's own animations**, opening on the touch display: Mission Control, App Exposé, Spaces, Launchpad, Show Desktop and Notification Center.
- **Back/forward page swipes** that follow your fingers, in Safari and other apps with trackpad page swipes.
- **Follows the trackpad settings** in System Settings.
- **Daily reliability:**
  - starts at login and restarts after a crash;
  - recovers after replug and sleep;
  - crash-safe mouse presses.
- README for newcomers, MIT license. The replay recording now ships with the repo, so a fresh clone runs all tests.

No functional changes since 0.6.4.

## 0.6.4

- The menu lists the followed trackpad settings one per line, instead of one long line that made the menu too wide.

## 0.6.3

- **Crash-safe press recovery.** The press journal records where a press was made and, for presses sent straight to an app, the window and the app. After a crash, the restarted app releases the press there:
  - an interrupted drag drops back where it started;
  - a reused process ID never receives the release.

## 0.6.2

- **Real trackpad page swipes.** Two-finger scrolls now carry the scroll-gesture stream a real trackpad sends: type-29 gesture events, subtype 61, then 6 for each movement, then 62. AppKit's swipe tracking measures progress from that stream, so Safari's back/forward swipe follows the fingers and completes or snaps back natively.
- The stream was found by tapping all events sent to a test window's process, because the swipe tracker takes them before any app-level monitor sees them.
- 0.6.0 and 0.6.1 were prototypes: a quick flick sent ⌘[ / ⌘], then pressed the app's Back/Forward menu item instead. Both were replaced by the real swipe.

## 0.5.10

- **Three-finger drag, "rest a finger, add two".** Rest one finger on an item, add two more within 1.5 s, and move. The drag grabs exactly where the resting finger first touched.

## 0.5.8

- A Space switch on the touch display follows the hidden cursor alone; the earlier step of focusing a window first was removed.
- **Keep Focus When Switching Spaces** (on by default) gives focus back to the app on the other screen after a Space switch by touch.

## 0.5.5

- **System gestures with the system's own animations**, via synthesized dock-swipe events:
  - four fingers up / down: Mission Control / App Exposé;
  - four fingers left / right: switch Space;
  - thumb pinch / spread: Launchpad / Show Desktop.
- Two fingers from the right edge open Notification Center.
- **All of them open on the touch display:**
  - Notification Center and Spaces follow the cursor, so the hidden cursor waits on the touch display while they open;
  - Launchpad follows the Dock, so the Dock is brought over first.
- Menu "Launchpad on the Touch Display" chooses whether the Dock comes over, comes over then goes back, or stays put.
- Diagnostics: `--dockswipe-selftest`, `--ax-probe --menubar`, `--system-ui-selftest`.

## 0.4.0

- **Follows the trackpad settings** in System Settings → Trackpad: three-finger drag, scrolling, momentum, pinch and rotate. Changes are picked up within seconds and after wake. A "Follow Trackpad Settings" master switch controls this.

## 0.3.0

- **Three-finger drag**, like the trackpad setting, grabbing at the centre of the fingers or under the first finger.
- Menu settings survive restarts.

## 0.2.0

- **Start at Login**: a per-user launch agent that starts TouchNext at login and restarts it after a crash.
- `scripts/install.sh` installs and updates the copy in `~/Applications`.
- **Multitouch after replug and sleep.** The panel then starts in single-touch "mouse" mode; TouchNext reads its feature reports to switch it back, as Linux's hid-multitouch driver does.

## 0.1.0

- **First version.** A user-space driver that seizes the touch panel and delivers touches to the touch display without moving the cursor.
- **Delivery:**
  - Direct: events posted to the app under the finger with window addressing, and the app activated before clicks;
  - Warp: a fallback for window moves, the Dock and menu bar, pop-up menus and Chromium apps.
- **Gestures:** tap, double and triple tap, right-click, scrolling with momentum, pinch, rotate, click-and-drag, window moves.
- Menu-bar agent with diagnostics.
