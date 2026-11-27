# TouchNext

**A real touchscreen for macOS, for USB touch monitors.** Touches act where you touch, the cursor stays where it is, and the trackpad's gestures work with your fingers on the screen: scrolling, pinch, rotate, Mission Control, Spaces, Launchpad, Notification Center and Safari's back/forward page swipes.

TouchNext is a small menu-bar app, written in Rust, that runs in user space:
- It takes over the touch panel and recognizes gestures itself.
- It sends macOS the same events a trackpad and mouse would, aimed at the window under your fingers.
- No kernel or system extension, no root, and System Integrity Protection stays on.
- Quitting it hands the panel back to macOS.

> **Hardware note:** this fork is maintained for the JAPANNEXT **JN-MD-IPST105FP** touch panel, and the input layer auto-detects USB HID touch-screen digitizers by class. **Status on that panel, 2026-11-24: single-touch only, and no macOS software can change that.** The panel's firmware withholds its multitouch reports from a Mac (see [Hardware support](#hardware-support)). Taps and one-finger gestures work with limited precision and drags stutter. With two or more fingers down, the panel's mouse emulation jumps between fingers.

## Contents

- [Why](#why)
- [Features](#features)
- [Gestures](#gestures)
- [Drawing and multitouch apps](#drawing-and-multitouch-apps)
- [Hardware support](#hardware-support)
- [Requirements](#requirements)
- [Install](#install)
- [The menu](#the-menu)
- [How it works](#how-it-works)
- [Adapting to another touchscreen](#adapting-to-another-touchscreen)
- [Logs and diagnostics](#logs-and-diagnostics)
- [Troubleshooting](#troubleshooting)
- [Uninstall](#uninstall)
- [Development](#development)
- [Caveats](#caveats)
- [Credits](#credits)
- [License](#license)

## Why

macOS has no touchscreen support:
- **Touches land on the wrong screen.** macOS treats a USB touch panel as an absolute pointing device, mapped onto whichever display the cursor is on. With a touch monitor as a second display, touching it while the cursor is on the laptop clicks the matching spot on the laptop screen.
- **One finger at a time.** There's no multitouch: no two-finger scrolling, no pinch, no gestures.

Commercial drivers exist. TouchNext is a free, user-space alternative. Its goal is to make the touch display feel like it has a trackpad built in.

## Features

- **Touches land on the touch display**, wherever the cursor is. For most gestures the cursor doesn't move at all: events go straight to the app under your finger.
- **Multitouch gestures**: tap, double and triple tap, right-click (hold), scrolling with momentum, pinch to zoom, rotate, click-and-drag, three-finger drag, and moving windows by the title bar.
- **The trackpad's system gestures, with the system's own animations**: Mission Control, App Exposé, switching Spaces, Launchpad, Show Desktop and Notification Center, all opening on the touch display.
- **Back/forward page swipes** in Safari and other apps that support trackpad page swipes. The page slides with your fingers.
- **Follows your trackpad settings** (System Settings → Trackpad), and picks up changes as you make them.
- **Daily reliability**:
  - starts at login and restarts after a crash;
  - recovers after unplugging and sleep;
  - can't leave a mouse button stuck, even across a crash.
- **Touch modes for drawing and multitouch apps**: a Drawing mode, where one finger draws and nothing else gets in the way, and a Multitouch mode that sends all ten fingers to apps that read TUIO.
- **Tracks up to 10 fingers.** Gestures use up to five; Multitouch mode passes on all ten.

## Gestures

| Touch | Result |
|---|---|
| Tap / double-tap / triple-tap | Left click (click count 1, 2, 3) |
| Hold about 0.5 s, then lift | Right-click |
| One-finger drag | Scroll with momentum (default), or click-and-drag (menu setting) |
| Hold, then drag | Click-and-drag (select text, move files) |
| Drag starting on a toolbar, title bar or title text | Move the window |
| Two fingers moving together | Scroll with momentum |
| Two fingers apart / together | Pinch to zoom |
| Two fingers around each other | Rotate |
| Two fingers swiping right / left, in apps with trackpad page swipes such as Safari | Back / Forward. The page slides with your fingers and completes or snaps back when you lift. |
| Two fingers from the right edge, moving left | Notification Center |
| Three fingers moving together | Click-and-drag, grabbing at the centre of the fingers or under the first finger (menu setting) |
| One finger resting on an item, two more added within 1.5 s, then moving | Click-and-drag that grabs exactly where the resting finger first touched |
| Four fingers up / down | Mission Control / App Exposé |
| Four fingers left / right | Switch Space |
| Thumb and three or four fingers pinching in / spreading out | Launchpad / Show Desktop |
| Six or more fingers | Ignored (palm rejection) |

**When a drag counts as one.** A finger travels 8 mm from where it landed before a swipe scrolls or a drag drags, so that a tap can wobble. That is the whole test, however slowly the finger moves and however long it has been down: a slow drag starts once it has travelled that far. On a toolbar, title bar or title text the window is pressed where the finger is by then, as long as that is still a drag area, so it starts moving without a jump. Where a report of the panel's leaves the finger is where the finger is: the places it gives along the way in the same report are not travel. A finger that stays inside the 8 mm for half a second and then lifts is a right-click, unless it had gone 5 mm from where it landed up to 150 ms after that: then it was a nudge that fell short, and the lift scrolls by how far it went. Once the finger lifts, the next touch is a new gesture.

**What the panel reports around a touch.** In mouse mode the WCH panel reports one touch at a time, and it misreports. Over three recordings (2 100 reports): 7 frames in 100 with the tip down put the finger 10 to 200 mm from where it was, 6 in 100 are followed by a tip-off report while the finger is still down, and around each landing and lift come up to seven frames of a touch somewhere else. Taken as they came, these ended touches that had not ended, began ones that never happened, and passed the jumps on as the finger's: a drag broke or jumped, and the touches around a lift were clicks wherever they landed. TouchNext believes a frame at once only within 6 mm of where the finger was heading. Any other is held until it has support: two frames that move as a finger can, three on one place, five frames on their own. A chain that keeps to its first frame is one on a place, unless it began with the report right after the finger's last sighting, within 12 mm of it, and moved on from there faster than a finger at rest wanders: that is a finger leaving fast, and it is believed on its second frame. While the finger is down and no lift has been reported, a place it could not have got to is never taken for it on the panel's word alone, however long the panel keeps to it. A tip-off report ends the touch when the panel has then been quiet for 50 ms, or after 250 ms for a finger that was moving and 150 ms for one standing still; a finger back by then, within a finger's travel, continues the same press. The panel also reports the tip off at its origin (0, 0), before a finger lands, after one leaves and while one is still down and moving, so that report is held like any other. It ends the touch at once only when a tip-off report from where the finger was is already being held. A new press counts on its third frame, or on its second when its lift is reported. A tap is judged by how long the finger was down, so the time TouchNext waits to be sure it has gone does not turn it into a long press. One recording of 44 s (2026-11-26, taps, scrolls and two drags) shows how much of this there is: 24 frames in 100 with the tip down were 6 mm or more from the one before, 35 of its 129 tip-off reports came from the origin, and after some lifts the panel went on reporting for a second.
**What the panel reports after a lift.** Once a finger lifts, the panel goes on reporting touches of its own for up to 1.3 s: bursts of one to nine frames on the finger's row, anywhere from side to side, most closed by a lift report, and often preceded by lift reports from its origin. In 29 s of the owner's use (2026-11-26, 22:03) they made ten taps (one the second click of a double-click on a window's title bar), two scrolls and a drag cut in two. So for as long as the panel keeps reporting after a lift, a touch that begins farther from the lift than the tracker would still take for the same finger is held until it shows ten frames of its own or has gone 8 mm across its row, and nothing else is believed meanwhile. A frame of the panel's between a touch's frames does not end it. The storm is over once the panel has sent nothing for 120 ms. A touch that begins within 100 ms of the lift report, farther than a finger that stood still could have got, and then proves itself, is the landing the panel misread before it: what lifted before it is no click. A tap that stands still within 15 mm of where the finger lifted is the finger back for half a second after the lift report, and is judged as at any other time. The panel also reports the tip off while the finger is down, then says nothing for a report or two, and the finger comes back 13 to 22 mm on and to rest. A still chain that begins within 50 ms of that lift report, where the tracker would still take the finger for the contact, is believed on its third frame as anywhere else; one that begins later waits for five. And one frame of the panel's just before a landing is left out of the landing when the finger stands still from its second frame on.


**Where the system gestures appear.** Space switching, Notification Center and Launchpad open on the touch display, not on whichever screen has the cursor or the focused window:
- **Spaces and Notification Center** appear on the display with the cursor. While they open, TouchNext briefly parks the hidden cursor on the touch display.
- **Launchpad** opens on the display that has the Dock. A pinch-in on the touch display first brings the Dock there, then (by default) sends it back after Launchpad closes.
- **Focus after a Space switch.** macOS moves focus to the switched display. TouchNext gives it back to the app you were using on the other screen; you can turn this off.

**Trackpad settings.** With "Follow Trackpad Settings" on (the default), each gesture follows its switch in System Settings → Trackpad: three-finger drag, scrolling, momentum, pinch, rotate, Mission Control, Spaces, Launchpad/Desktop and Notification Center.

## Drawing and multitouch apps

**What macOS allows.** macOS has no touchscreen input layer, unlike Windows, iOS and Android. As a result:
- **Apps get one mouse pointer.** That's all an ordinary app can receive.
- **Browsers on a Mac never produce touch events**, so web pages only ever see the mouse.
- **iPhone and iPad apps running on a Mac** get a single touch made from the pointer.

So more than one finger can only reach apps that read a multitouch protocol themselves. TUIO is the standard one: an open protocol from the multitouch-table community, used in creative coding, interactive installations and DIY touch tables.

**The ✋ → Touch Mode menu** picks what touches do. It goes back to Gestures every time TouchNext starts, so a special mode can't be left on by accident.

| Touch Mode | Icon | What touches do |
|---|---|---|
| Gestures (default) | ✋ | Everything in [Gestures](#gestures) |
| Drawing: One Finger Draws, Nothing Else | ✏️ | The first finger is a pen; every other finger and every gesture is ignored |
| Multitouch Apps: All Fingers as TUIO | 🖐 | Every finger goes to TUIO apps; nothing reaches macOS |

**Drawing mode** suits drawing apps and websites, signature boxes and whiteboards:
- The first finger to touch is a pen: it presses the mouse at touch-down (so a tap makes a dot), drags while it moves, and releases at lift.
- Strokes go through the window server like a real mouse. Canvases check that a mouse button is really pressed while the pointer moves.
- Every other finger, including a palm resting on the screen, is ignored, and so is every gesture.
- When the pen lifts, the next finger to touch draws.

**Multitouch Apps mode** sends every finger, up to ten, as TUIO 1.1 ("2Dcur" cursors) over UDP to `127.0.0.1:3333`. That's the default port of TUIO apps such as TouchDesigner, Processing, openFrameworks, Unity, Max and Kivy.
- Nothing reaches macOS: no clicks and no gestures, so four fingers in your TUIO app won't open Mission Control. Use the trackpad for everything else while this mode is on.
- While nothing touches, TouchNext sends a frame once a second, as TUIO apps expect.

**Testing.** ✋ → Open Event Sink opens a test window that paints touches:
- in Multitouch mode, every finger in its own colour, marked with its touch number while it's down;
- in Drawing mode, the mouse stroke in grey, which is exactly what an ordinary app receives.

The top line shows fingers now and the most at once. Clear wipes the canvas.

**What won't work, and why.**
- **Touch-only web pages** do nothing at all on a Mac, not even with one finger. These are pages that listen only for touch events, such as many online multitouch testers. The browser never produces those events.
- **Most drawing websites draw one stroke at a time**, even on a tablet. AutoDraw, for example, reads only the first touch.
- **Multi-finger drawing in ordinary apps isn't possible.** They get one mouse.

## Hardware support

**Tested with:**

| | |
|---|---|
| Touch monitor | JAPANNEXT **JN-MD-IPST105FP**, 10.5-inch IPS, 1920×1280, 5-point touch |
| Touch controller | WCH (wch.cn) `0x27C0:0x0859`: single-touch on macOS, its firmware gates the multitouch stream (note below); ILITEK `0x222A:0x0001` unchanged |
| Mac | MacBook Pro (Apple silicon), macOS 26 |

The upstream project's reference panel (ARZOPA 1920×1080, ILITEK `0x222A:0x0001`, 10 contacts) keeps working through the same code paths.

**What the input layer does automatically now:**

1. **Device discovery** matches any USB HID touch-screen digitizer (usage page 0x0D, usage touch screen), excluding Apple's own devices. The menu's Panel line names what it seized, with its USB IDs (a debug build's log does too).
2. **The report format** (ILITEK family) derives the contact-slot count from each report's own length, so 10-contact panels padded to 64 bytes and shorter 5-contact reports both parse.
3. **The display** is found by its EDID vendor and model numbers when they match, and otherwise falls back to the single external display — an unlisted panel works without editing anything.
4. **The physical size** used for gesture thresholds defaults to the JN-MD-IPST105FP's active area (221.9 × 147.9 mm) and is set in one place (`PipelineSettings`).

Panels whose controllers use a different report layout than the ILITEK family still need the parser adapted. [Adapting to another touchscreen](#adapting-to-another-touchscreen) walks through each step.

**The WCH panel declares full multitouch and withholds it from a Mac.** The controller's HID report descriptor declares a ten-finger digitizer report, and it answers a feature read with a maximum of 15 contacts. Windows and Linux hosts get those reports. A Mac gets single-touch mouse reports on the controller's mouse interface.

The firmware decides during USB enumeration, from how the host reads its string descriptors. [TinyUSB issue #4346](https://github.com/hathach/tinyusb/issues/4346) shows on the same controller that a host reading two bytes of each string and then the rest gets no multitouch, and one reading each string in a single 255-byte request (as Linux does) gets it. macOS shows the first result. [chill1Penguin/macos-xeneon-edge-multitouch](https://github.com/chill1Penguin/macos-xeneon-edge-multitouch) gets multitouch on macOS by patching that check out of the firmware.

Measured on the JN-MD-IPST105FP on 2026-11-24 over raw USB, with the device taken from macOS and a finger on the glass:

- The Windows write (`SET_REPORT`, feature `0x21`, data `21 02 00`) is acknowledged. Reading that feature back returns the first bytes of the request packet after every mode written, so the firmware stores no mode. Contact Count Maximum (feature `0x0A`) reads back `0a 0f`.
- Linux-style string descriptor reads (one 255-byte request each), a reconfigure that leaves the interfaces without drivers, and the Windows write, in each combination tried, left the digitizer interface at zero reports while the mouse interface delivered 116 to 348 reports per 8 seconds.
- The mouse emulation carries no multi-finger input. Over 24 seconds of two- and three-finger gestures, all 842 reports had button values 0 or 1 and a zero wheel byte, and the pointer jumped between fingers.

Input Mode 1 and 3 were not judged: no touch arrived in those windows.

TouchNext therefore reads the single-touch mouse reports, and replugging changes nothing. Multitouch on macOS needs one of two things outside this app: a USB device between the Mac and the panel that enumerates it as Windows does (the panel is a full-speed device), or a firmware without the check. The community reports of multitouch after the Input Mode write ([Touch-Up issue #40](https://github.com/shueber/Touch-Up/issues/40)) concern the sibling controller `0x0858`.

## Requirements

- **macOS 15 (Sequoia).** Developed on 15.7; other versions are untested. TouchNext relies on undocumented event formats that can change between macOS releases (see [Caveats](#caveats)).
- **An Apple silicon Mac.** Intel is untested.
- **A Rust toolchain**, 1.88 or later (`rustup` installs it), plus Xcode's command-line tools for `codesign`.
- **No other touchscreen driver** running, such as UPDD or Touchscreen Gestures. They take the panel exclusively, and so does TouchNext.

## Install

1. **Clone the repository.**
   ```bash
   git clone https://github.com/vipincr/touch-bridge.git
   ```
   ```bash
   cd touch-bridge
   ```

2. **Create a signing certificate (recommended, one time).** macOS ties the Accessibility and Input Monitoring permissions to the app's signature. An ad-hoc signed build loses them every time you rebuild; a stable self-signed certificate keeps them.
   1. Open Keychain Access and choose Keychain Access → Certificate Assistant → Create a Certificate….
   2. Enter the name `TouchNext Dev`, choose Identity Type "Self-Signed Root" and Certificate Type "Code Signing", and create it.

   To use another certificate, set `TOUCHNEXT_SIGN_IDENTITY` to its keychain name when building. If the name is not in the keychain the build stops and lists the identities; it never falls back to ad-hoc signing.

3. **Build and install.**
   ```bash
   ./scripts/install.sh
   ```
   This does four things:
   - builds `build/TouchNext.app`, the prod build (it keeps no log and has no diagnostics);
   - copies it to `~/Applications/TouchNext.app`;
   - turns on Start at Login;
   - starts it.

4. **Grant permissions.**
   1. TouchNext asks for both permissions when it starts. Allow Accessibility and Input Monitoring in System Settings → Privacy & Security. A panel that reports through a mouse interface, the JN-MD-IPST105FP among them, needs Input Monitoring.
   2. TouchNext restarts itself once both are granted. To do it by hand: choose ✋ → Quit TouchNext, then open `~/Applications/TouchNext.app` again.

5. **Touch the screen.** The ✋ menu shows whether the panel and the touch display were found.

**To update:** pull the latest changes and run `./scripts/install.sh` again. Rebuilding with `./scripts/bundle.sh` never touches the installed copy. `./scripts/install.sh --debug` installs a debug build instead, which keeps a log and has the diagnostics ([Logs and diagnostics](#logs-and-diagnostics)).

## The menu

TouchNext lives in the menu bar as ✋. Settings are saved across restarts, except Enabled and Dry Run, which reset at each start.

| Item | What it does |
|---|---|
| Status lines | Panel connection, touch display, the last gesture and how it was delivered, and how many fingers are down |
| Enabled | Turns touch handling on or off. Off hands the panel back to macOS. |
| Touch Mode | Gestures (default), Drawing (one finger draws, nothing else), or Multitouch Apps (all fingers as TUIO, nothing to macOS). Resets to Gestures at each start. See [Drawing and multitouch apps](#drawing-and-multitouch-apps). |
| Delivery | Automatic (default) picks Direct or Warp per gesture (see [How it works](#how-it-works)). "Direct only" and "Warp only" are for testing how an app copes. |
| Pinch & Rotate Direct | Sends pinch and rotate straight to the app (default). Off routes them through the window server instead. |
| One-Finger Drag Scrolls | On (default): one finger scrolls, and holding first then dragging is click-and-drag. Off: one finger drags. |
| Swipe Between Pages (Two Fingers) | Two-finger back/forward page swipes (default on) |
| Follow Trackpad Settings | Gestures follow System Settings → Trackpad (default on). The settings read are listed underneath, one per line. |
| Three-Finger Drag | Three fingers drag. Greyed out while following the trackpad setting. |
| Three-Finger Drag Point | Where a three-finger drag grabs: centre of the fingers (default) or first finger to touch |
| Launchpad on the Touch Display | **Bring the Dock Over, Then Back** (default): the Dock comes over for Launchpad and goes back afterwards. **Bring the Dock Over (it stays)**: the Dock stays on the touch display. **Leave the Dock Where It Is**: Launchpad opens wherever the Dock is. |
| Return Focus to Previous App | After you tap an app on the touch display, gives focus back to the app you were using about 0.6 s later. Off by default: focus follows touch. |
| Keep Focus When Switching Spaces | After a Space switch on the touch display, gives focus back to the app on the other screen (default on) |
| Dry Run | Recognizes gestures without sending anything; the Last gesture line shows them. In a debug build the log has them too. |
| Start at Login | Runs TouchNext at login and restarts it if it crashes. Quit from the menu stays quit until the next login. |
| Open Event Sink (debug build) | A test window on the touch display. It logs every event it receives and paints touches: TUIO fingers in colour, the mouse in grey. |
| Open Logs Folder (debug build) | Opens `~/Library/Logs/TouchNext/` |

## How it works

```
touch panel ──HID──▶ ReportParser ─▶ ContactStream ─▶ GestureRecognizer ─▶ DeliveryPolicy ─▶ Deliverer ─▶ apps / macOS
 (seized)            (bytes→contacts) (jump filter,    (taps, drags,       (Direct or Warp)  (synthesized
                                      contact tracker) scroll, pinch, …)                    events)
```

**Taking the panel.** TouchNext opens the touch panel with exclusive access (`kIOHIDOptionsTypeSeizeDevice`), so macOS stops turning touches into cursor movements. It reads the raw multitouch reports itself, parses each finger's position, and tracks contacts across frames. Panel coordinates are mapped onto the touch display's current bounds, updated live when displays are rearranged. Rotation is supported in the mapping but hasn't been tested on hardware.

**Recognizing gestures.** `GestureRecognizer` is pure Rust with no system frameworks, and it is unit tested, including replays of a real panel recording. Nothing is sent until a gesture is classified. Two-finger gestures are classified by how the fingers move:
- both the same way → scroll;
- towards or away from each other → pinch;
- around each other → rotate.

**Delivering to apps.** Each gesture is delivered one of two ways:
- **Direct:** the event is posted straight to the app under the finger (`CGEventPostToPid`). The cursor never moves. AppKit routes a posted event to a window only if it carries the window number in event field 51 and the window-relative location (set with the private `CGEventSetWindowLocation`). For taps, drags and pinches the app is activated first, because macOS ignores clicks aimed at a background window.
- **Warp:** the cursor is hidden, moved to the finger, and returned about 50 ms later. The event goes through the window server like real mouse input. This is used where Direct can't work:
  - window moves and drag-and-drop;
  - the Dock and the menu bar, and pop-up menus;
  - Chromium/Electron apps;
  - the desktop.

**System gestures.** Mission Control, Spaces, Launchpad and Show Desktop are driven by synthesized "dock swipe" events: a type-30 event with subtype 23 plus a type-29 companion. These are the private events the trackpad itself produces, so macOS animates them as usual. Notification Center is opened through its menu-bar clock. Each one appears on the touch display:
- Space switching and Notification Center follow the cursor;
- Launchpad follows the Dock.

**Page swipes.** Each two-finger scroll from a real trackpad comes with a second, hidden stream of gesture events: type 29, subtype 61 at the start, subtype 6 for every movement, subtype 62 at the end. AppKit's swipe tracking (behind Safari's back/forward swipe) measures progress from that stream, not from the scroll events. TouchNext sends the stream with two-finger scrolls, so apps animate the swipe natively.

**Touch modes.**
- **Drawing:** a separate path in `GestureRecognizer`. The first finger becomes a pen, delivered as a Warp drag so the mouse button is really down system-wide. Everything else is dropped.
- **Multitouch Apps:** gesture recognition is skipped entirely. Each frame of contacts becomes a TUIO bundle: `TUIOSessions` gives every touch its own session id and a velocity, and `TUIO.frame` builds the OSC bytes, sent over UDP.

**Staying reliable.**
- **Multitouch mode:** TouchNext asks the panel for multitouch when it connects and again whenever single-touch (mouse-mode) reports appear. On the WCH `0x0859` panel the firmware acknowledges the write and ignores it (see the hardware note).
- **Stuck buttons:** every open press is journaled to disk, so a restart after a crash releases it where it was made.
- **Login item:** a per-user launch agent starts TouchNext at login and restarts it after a crash.

The design history is in `docs/`, including findings, experiments that failed and why:
- `docs/spec.md`: the original plan plus implementation notes;
- `docs/backlog.md`: per-feature investigations and decisions.

## Adapting to another touchscreen

You'll change a few constants and possibly the report parser, then rebuild.

1. **Check the panel enumerates as a digitizer.** Discovery is by HID class, so there are no USB IDs to set: any USB touch-screen digitizer is matched. Open System Information → USB, or run:
   ```bash
   ioreg -p IOUSB -l -w0 | grep -E '"USB Product Name"|"idVendor"|"idProduct"'
   ```
   The menu's Panel line names the panel TouchNext seized, with its USB IDs; a debug build's log does too.

2. **Record the panel's reports.** With nothing else holding the panel, build with `./scripts/bundle.sh --debug`, then run:
   ```bash
   open -n build/TouchNext.app --args --probe 90
   ```
   Touch with one, two and three fingers. `~/Library/Logs/TouchNext/` gets the raw reports (`probe-*.jsonl`) and a summary.

3. **Adapt the parser.** `rust/crates/touchcore/src/report.rs` has one decoder per supported controller family: WCH (report ID `0x0D`; the JN-MD-IPST105FP) and ILITEK (report ID 4; the ARZOPA, with the slot count derived from the report length). For another format, read your panel's HID report descriptor:
   ```bash
   ioreg -r -n TouchScreen -l -w0 | grep ReportDescriptor
   ```
   decode the touch collection (report ID, per-contact field order and sizes, logical maximums, where the contact count lives) and add a decoder alongside the two. Add your recording to `rust/crates/touchcore/tests/fixtures/` and a replay test for it to `rust/crates/touchcore/tests/replay.rs`.

4. **Point it at your display.** With exactly one external display, nothing to do: the pipeline falls back to it when no EDID match is configured. To pin it exactly, run the read-only probe:
   ```bash
   open -g -n build/TouchNext.app --args --ax-probe --menubar
   ```
   `axprobe.log` (from a debug build) lists every display with its vendor and model numbers. Set `display_vendor` and `display_model` in `PipelineSettings` (`rust/crates/touchplatform/src/pipeline.rs`). The diagnostics take the touch display from these same numbers.

5. **Set the physical size.** Set `panel_width_mm` and `panel_height_mm` in `PipelineSettings` (the same file) to the panel's active area. Gesture thresholds are in millimetres.

6. **Check the multitouch switch.** The ILITEK panel needs feature reports 2 and 6 read before it leaves single-touch mode (`request_multitouch_mode` in `rust/crates/touchhid/src/macos.rs`); this happens automatically for ILITEK controllers only. A WCH `0x0859` controller acknowledges the Input Mode write TouchNext sends and ignores it on macOS (see the hardware note).

7. **Test and install.**
   ```bash
   cargo test --manifest-path rust/Cargo.toml --workspace
   ```
   ```bash
   ./scripts/install.sh
   ```

Making all of this automatic would be the natural next step: parsing any standard HID multitouch panel from its descriptor (discovery and slot-count handling already are), a display picker in the menu, and the physical size from the display's EDID. If you adapt TouchNext to another panel, please consider sharing the changes.

## Logs and diagnostics

Only a debug build keeps a log and has the diagnostics: one made with `./scripts/bundle.sh --debug`, which turns on the Cargo feature `debug`. The build `./scripts/install.sh` installs is the prod build. It opens no log file and has no diagnostics; given one of their arguments, it says so and exits. The menu's status lines are there in both builds. To read what the installed copy does, install a debug build for the while: `./scripts/install.sh --debug`.

A debug build's logs are in `~/Library/Logs/TouchNext/`:
- `touchnext.log`: every gesture, how and where it was delivered, where each finger began and ended, and every press and drag it posted;
- `reports-<date>-<time>.log`: the raw panel reports of one run of the app, one line a report in the probe's format, so that `--replay` can run what the app was given. The ten newest runs are kept.
- `eventsink.log`: what the Event Sink test window received;
- other files from the diagnostic modes below.

Diagnostic modes run on a debug build (`./scripts/bundle.sh --debug` leaves it in `build/TouchNext.app`). Most run next to the installed copy. The ones marked † need the panel, or the single running instance, so quit the installed copy first (✋ → Quit TouchNext).

| Command (`open -n build/TouchNext.app --args …`) | What it does |
|---|---|
| `--probe 90` † | Records raw panel reports for 90 s |
| `--dry-run` † | Starts the app in Dry Run: it recognizes and logs gestures without sending anything |
| `--event-sink` | Opens the Event Sink test window. It logs every event it receives, including those AppKit's swipe tracker takes first, and paints TUIO fingers (listening on port 3333) and mouse strokes. |
| `--direct-selftest` | Posts test events to the Event Sink only (`--swipe` adds the page-swipe variants) |
| `--ax-probe --map` | Read-only map of the window-drag areas on screen |
| `--ax-probe --menubar` | Read-only report: displays, menu bars and clocks, active menu-bar display, current Spaces, the Dock |
| `--ax-probe --menu` | Lists the installed copy's menu as Accessibility reports it, status lines and check marks included. Each `--press "Title"` (or `--press "Submenu/Item"`) then presses one item, without moving the cursor, and lists the menu again. |
| `--restart-selftest` † | Starts, stops and restarts the panel input |
| `--dockswipe-selftest` | Posts Mission Control dock swipes and closes them again. **Visible on screen.** |
| `--system-ui-selftest` | Opens and closes Notification Center and Launchpad (`--nc`, `--launchpad`, `--move-dock`, `--spaces`) and logs where each appeared. **Visible on screen.** |

Add `-g` after `open` to keep a diagnostic run in the background.

`--replay` prints to the terminal, so it runs from a shell, on the executable inside the bundle:

```bash
build/TouchNext.app/Contents/MacOS/TouchNext --replay ~/Library/Logs/TouchNext/probe-x.jsonl
```

It runs a recording (a probe's `probe-*.jsonl`, or a run's `reports-*.log`) through the pipeline in dry run, at the speed it was recorded and with the saved menu settings, and prints every line the pipeline reports: what it would have delivered, and where. Each report is stamped for the moment the recording says it came, so one recording plays the same every time, and the same after a change is made to compare against. Add `--frames` to print every position the recognizer was given as well, one line a frame. It posts nothing and does not take the panel.

## Troubleshooting

**Touching does nothing.**
- Another touchscreen driver may be holding the panel. Quit or uninstall it, then use ✋ → Enabled, or restart TouchNext.
- The ✋ status lines say whether the panel and the display were found.

**Touches move the cursor, or land on the wrong screen.** TouchNext isn't running or isn't enabled, so macOS is handling the panel itself.

**The panel is found but apps don't react.** Accessibility permission is missing, or was granted to a differently signed build. Switch it off and on again in System Settings → Privacy & Security → Accessibility, then restart TouchNext.

**`seize failed: kIOReturnNotPermitted` on the panel's mouse interface.** Input Monitoring is off for TouchNext, which is what an ad-hoc signed build installed over a signed one leaves behind. Switch it on again in System Settings → Privacy & Security → Input Monitoring. If the entry will not switch on, run `tccutil reset ListenEvent com.gramini.touchnext`, start TouchNext and allow its prompt.

**Only one finger works.** On the WCH `0x27C0:0x0859` panel that is as far as a Mac can go: see the hardware note. Replugging does not change it.

**A gesture behaves differently from the trackpad.** Check "Follow Trackpad Settings" and the trackpad switches listed under it.

**A drag breaks, jumps or does not start, or a tap lands where the finger wasn't.** Look first at what the panel reported around the touch (see "What the panel reports around a touch"). Install a debug build for as long as it takes, do it again, then replay the run it recorded with `--frames`: `reports-<date>-<time>.log` has every report, and the replay prints every position the recognizer was given.

**One app ignores taps.**
- Try ✋ → Delivery → Warp only (testing) to compare.
- Chromium/Electron apps already use Warp automatically.

**Page swipes don't work in an app.** The app must support trackpad page swipes, and "Swipe between pages" must be on in System Settings → Trackpad → More Gestures.

**Gestures get in the way of drawing.** Use ✋ → Touch Mode → Drawing: one finger draws, and every other finger and gesture is ignored.

**Multitouch mode does nothing in my apps.** That's by design. In Multitouch mode fingers go only to apps that read TUIO, and nothing is delivered to macOS. Ordinary apps, web pages, and iPhone or iPad apps can't receive more than one finger on a Mac. Use the Event Sink to check that the fingers arrive.

**A web page ignores touches completely.** It probably listens only for touch events, which browsers on a Mac never produce. TouchNext reaches web pages as a mouse.

## Uninstall

1. ✋ → turn off Start at Login, which removes the launch agent. Then ✋ → Quit TouchNext.
2. Move `~/Applications/TouchNext.app` to the Trash.
3. Optionally, remove TouchNext's files and settings:
   - `~/Library/Logs/TouchNext/`
   - `~/Library/Application Support/TouchNext/`
   - the saved settings, with this command:
     ```bash
     defaults delete com.gramini.touchnext
     ```
4. Remove TouchNext from System Settings → Privacy & Security → Accessibility.

## Development

```bash
cargo test --manifest-path rust/Cargo.toml --workspace
cargo test --manifest-path rust/Cargo.toml --workspace --features debug
```
```bash
./scripts/bundle.sh --debug
```
```bash
./scripts/install.sh
```

- `cargo test` runs the unit tests, the delivery tests and the replay of ten recorded panel sessions, on the prod build's code. The second command runs them again with the `debug` feature: the log and the diagnostics are compiled only then, so that pass is theirs.
- `./scripts/bundle.sh` builds and signs `build/TouchNext.app`: the prod build, or with `--debug` the one with the log and the diagnostics.
- `./scripts/install.sh` builds, installs to `~/Applications` and restarts the login item. `--debug` installs a debug build.
- `CHANGELOG.md` lists the changes in each version.

| Path | Contents |
|---|---|
| `rust/crates/touchcore` | Pure logic: report parser, jump filter, contact tracker, coordinate mapping, gesture recognizer, delivery policy, focus and Dock logic. No system frameworks; fully unit tested. |
| `rust/crates/touchhid` | The touch panel over IOKit HID, and the clock the driver runs on |
| `rust/crates/touchplatform` | macOS I/O: displays and windows, event synthesis, Direct and Warp delivery, system gestures, the pipeline that connects them, diagnostics |
| `rust/crates/touchnext` | The menu-bar app: menu, saved settings, login item. With the `debug` feature also the log file, the Event Sink, the probe and the replay |
| `rust/crates/touchcore/tests/fixtures` | Nine recorded panel sessions, replayed by `rust/crates/touchcore/tests/replay.rs` |
| `docs` | The original plan with implementation notes (`spec.md`) and per-feature investigations (`backlog.md`) |

Apple's frameworks are reached through the `objc2` bindings; the tree holds no hand-written `extern` block. The private calls the headers leave out are looked up by name at run time.

The Swift implementation the port replaced left the tree on 2026-11-26. It is in git history, under `Sources/` and `Tests/` up to commit `e0433f6`.

## Caveats

- **Private APIs.** TouchNext depends on undocumented CoreGraphics, SkyLight and IOKit behaviour:
  - event fields and gesture subtypes learned by observing real trackpad events;
  - a few private calls, looked up at runtime.

  A macOS update can break individual features; where a private call is missing, that feature turns off and the rest keeps working. This is not App Store material.
- **One tested setup.** It's a personal project, tested on one monitor, one Mac and one macOS version.
- **Two touches close together can count as one.** A touch that lands before the last lift report's hold is over (150 ms for a still finger, 250 ms for a moving one), near the last one, continues that press: a tap and a press 5 mm apart become one touch.
- **A click leaves 50 ms after the finger lifts at the earliest.** When the panel keeps reporting after the lift, it leaves 150 ms after the lift report (250 ms if the finger was moving).
- **A storm that starts again after a pause can still tap.** Right after a lift the panel's places wait to prove themselves. Once the panel has been quiet for 120 ms the storm counts as over, and what follows is judged as at any other time. The panel does start again after longer silences: in a 44 s recording of 2026-11-26 it did so after 198 ms, 356 ms and 643 ms, each time with a pair or a five-frame burst at another place on the row the finger had been on, and each was a click. A still run of ten frames or more is a press wherever it begins.
- **A touch that begins amid the storm starts late, and a short one is lost.** Held to prove itself, a touch goes out when it does, whole and dated from its first frame. In the 29 s recording of 2026-11-26 a drag began 158 ms after its first frame and a flick 33 ms. A flick shorter than ten frames that begins there is let go, and so is a tap of fewer than ten frames that begins within 100 ms of a lift report more than 15 mm from it: in a recording of 20:54 that day, a tap at the panel's left edge 17 ms after the lift report of a touch the panel made at its bottom-right corner.
- **A flap can still stand a drag still for 70 ms.** Now and then the panel reports the tip off while the finger is down, then says nothing for a report or two. The finger's frames after that are believed on their third when the first comes within 50 ms of the lift report; later they wait for five, as any chain after a lift report. That happened once in the 7.4 s drag of 2026-11-27 (16:25:07, one of the panel's frames between the report and the finger's first frame), for 72 ms.
- **A flap in the middle of a fast flick loses the end of the flick.** When the panel reports the tip off while the finger is going fast, its next frame is farther than the tracker would still take the finger, and what follows is judged like any other touch after a lift. At 18.335 s of the same recording the finger was going 289 mm/s, and the flick's last 139 ms (30 mm) did not scroll.
- **A tap followed by a sweep back to where the finger lifted is a small scroll.** In the 44 s recording of 2026-11-26 the tap at 10.917 s lasted 233 ms. The panel's first sweep after it ran from 12 mm right of the finger back to where it lifted and closed with a lift report from there, and the stream takes it for the finger. Counted from its first frame the touch is then last seen 529 ms after it landed, 4.7 mm from where: a nudge that fell short, so its lift scrolls 4.7 mm and clicks nothing. The build of 21:11 that day counted the touch from its third frame, which made it 493 ms, and tapped.
- **A flick under 8 mm does not scroll while the finger is down.** A flick shorter than the 8 mm a tap may wobble is a click where the finger stopped. If the finger has gone 5 mm by 650 ms after landing and is still down then, the lift scrolls by how far it went. The slop cannot simply be smaller: as the tracker sees them, taps slide up to 5.8 mm, since the panel reports a tapping finger 20 to 25 pt from where it landed. On 2026-11-26 small nudges began with flicks of 5.5 mm, 6.7 mm and 9 mm; the first two scrolled only once the finger had drifted on past 8 mm.
- **A fast scrub is rough.** When the finger darts back and forth, most of what the panel reports is its own, and the gesture comes out shorter and jumpier than the finger was.
- **The cursor is borrowed briefly.** Warp delivery and some system gestures move the hidden cursor for a fraction of a second. If you're using the trackpad at that exact moment, the cursor returns to where it was before.

## Credits

- [Mac Mouse Fix](https://github.com/noah-nuebling/mac-mouse-fix) by Noah Nuebling: its `TouchSimulator` showed the format of the trackpad's dock-swipe events.
- WebKit's CoreGraphics SPI headers: names and numbers for gesture event fields.

## License

MIT. See [LICENSE](LICENSE).
