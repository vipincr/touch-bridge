//! Spike: posts vertical dock swipes in both directions and reports whether Mission Control opened
//! (the Dock shows full-screen windows at layers 18/20 while it is open). Closes it again after each try.

use std::time::{Duration, Instant};

use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFArray, CFDictionary, CFRetained, CFNumber, CFString, CFType};
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption, kCGWindowLayer, kCGWindowName, kCGWindowOwnerName};
use objc2_foundation::NSURL;
use objc2_foundation::NSString;
use touchhid::clock;

use crate::dock_swipe::{DockSwipe, Kind, Phase};
use crate::swift_text::swift_f64;

/// Runs the dock-swipe self-test.
pub struct DockSwipeSelfTest;

impl DockSwipeSelfTest {
    /// Runs the self-test the command line asks for: the pinch experiments (`--pinch-inverted`),
    /// every raw event kind (`--all-kinds`), or the default vertical swipes.
    pub fn run(log: &mut dyn FnMut(&str)) {
        let mut swipe = DockSwipe::new();
        let args: Vec<String> = std::env::args().collect();
        let scale = args
            .iter()
            .position(|arg| arg == "--scale")
            .and_then(|index| args.get(index + 1))
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.05);
        if args.iter().any(|arg| arg == "--pinch-inverted") {
            // Mac Mouse Fix: before macOS 26, pinch dock swipes need the inverted flag (and the sign flipped).
            for sign in [1.0, -1.0] {
                let d = sign * scale;
                log(&format!("pinch inverted {}", if sign > 0.0 { "+" } else { "−" }));
                post_swipe(&mut swipe, Kind::Pinch, d, true);
                wait(&mut swipe, 4_000);
            }
            log("done");
            return;
        }
        if args.iter().any(|arg| arg == "--all-kinds") {
            // Watched by the owner: horizontal, pinch, then a guessed type 4, each positive then negative.
            for raw in [1i64, 3, 4] {
                for sign in [1.0, -1.0] {
                    let d = sign * scale;
                    log(&format!("kind {raw} {}", if sign > 0.0 { "+" } else { "−" }));
                    swipe.post_raw(d, raw, Phase::Began);
                    for _ in 0..10 {
                        wait(&mut swipe, 16);
                        swipe.post_raw(d, raw, Phase::Changed);
                    }
                    wait(&mut swipe, 16);
                    swipe.post_raw(d, raw, Phase::Ended);
                    wait(&mut swipe, 3_000);
                }
            }
            log("done");
            return;
        }
        for sign in [1.0, -1.0] {
            let d = sign * scale;
            log(&format!(
                "vertical dock swipe, {} steps of {}",
                if sign > 0.0 { "positive" } else { "negative" },
                swift_f64(d),
            ));
            post_swipe(&mut swipe, Kind::Vertical, d, false);
            wait(&mut swipe, 1_200);
            let open = Self::mission_control_open();
            log(&format!("  → Mission Control open: {open}"));
            if open {
                NSWorkspace::sharedWorkspace()
                    .openURL(&NSURL::fileURLWithPath(&NSString::from_str(
                        "/System/Applications/Mission Control.app",
                    )));
                wait(&mut swipe, 1_200);
                log(&format!("  closed again: {}", !Self::mission_control_open()));
            }
        }
        log("done");
    }

    /// Whether Mission Control is open: the Dock's full-screen windows at layers 18/20, whatever
    /// they are called except the wallpaper.
    pub fn mission_control_open() -> bool {
        type WindowEntry = CFDictionary<CFString, CFType>;
        let list = CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenOnly, 0);
        // SAFETY: the window server fills the array with dictionaries keyed by CFString, exactly
        // what `WindowEntry` says.
        let list: Option<CFRetained<CFArray<WindowEntry>>> =
            list.map(|list| unsafe { CFRetained::cast_unchecked(list) });
        list.into_iter()
            .flatten()
            .any(|entry| mission_control_window(&entry))
    }
}

/// One window-list entry as a Mission Control window: the Dock's, at layer 18 or 20, not a wallpaper.
fn mission_control_window(entry: &CFDictionary<CFString, CFType>) -> bool {
    let owner = entry
        .get(unsafe { kCGWindowOwnerName })
        .and_then(|value| value.downcast_ref::<CFString>().map(|text| text.to_string()));
    let layer = entry
        .get(unsafe { kCGWindowLayer })
        .and_then(|value| value.downcast_ref::<CFNumber>().and_then(|number| number.as_i64()));
    let name = entry
        .get(unsafe { kCGWindowName })
        .and_then(|value| value.downcast_ref::<CFString>().map(|text| text.to_string()))
        .unwrap_or_default();
    owner.as_deref() == Some("Dock")
        && matches!(layer, Some(18) | Some(20))
        && !name.starts_with("Wallpaper")
}

/// Waits `ms`, calling [`DockSwipe::tick`] about every 10 ms so the re-sends of a finished swipe
/// leave on time, as the Swift tests' dispatch queues sent them.
pub fn wait(swipe: &mut DockSwipe, ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10).min(deadline - Instant::now()));
        swipe.tick(clock::now_ns());
    }
    swipe.tick(clock::now_ns());
}

/// Posts one whole dock swipe: `began`, ten `changed` steps 16 ms apart, then `ended`.
pub fn post_swipe(swipe: &mut DockSwipe, kind: Kind, delta: f64, inverted: bool) {
    swipe.post(delta, kind, Phase::Began, inverted, clock::now_ns());
    for _ in 0..10 {
        wait(swipe, 16);
        swipe.post(delta, kind, Phase::Changed, inverted, clock::now_ns());
    }
    wait(swipe, 16);
    swipe.post(delta, kind, Phase::Ended, inverted, clock::now_ns());
}
