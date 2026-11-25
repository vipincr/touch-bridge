//! `--system-ui-selftest`: which display do Notification Center and Launchpad open on, for each way
//! of triggering them? Tries each variant, reads back where their windows appeared, and closes them
//! again.
//!
//! VISIBLE on screen (opens and closes Notification Center and Launchpad): run only with the owner
//! watching. `--nc` or `--launchpad` runs one half. `--move-dock` adds a variant that moves the Dock
//! to the touch display, opens Launchpad, and moves the Dock back. `--spaces` (only on its own)
//! switches the touch display's Space with the hidden cursor there and nothing focused, then
//! switches back.

use std::time::Duration;

use objc2_application_services::AXIsProcessTrusted;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CGPoint, CFRetained};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGEvent, CGEventField, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
    CGWarpMouseCursorPosition,
};
use objc2_foundation::{NSURL, NSString};
use touchcore::mapper::{ScreenPoint, ScreenRect};
use touchcore::menubar::MenuBarGeometry;
use touchcore::policy::WindowHitTester;

use crate::cursor::CursorVisibility;
use crate::dock::DockLocator;
use crate::dock_swipe::{DockSwipe, Kind};
use crate::dock_swipe_self_test::post_swipe;
use crate::notification::NotificationCenterOpener;
use crate::pipeline::{self, PipelineSettings};
use crate::system_displays::SystemDisplays;
use crate::window::WindowLocator;

/// Runs the self-test the command line asks for.
pub struct SystemUISelfTest;

impl SystemUISelfTest {
    /// Reads the touch display from the pipeline's settings, then runs Notification Center,
    /// Launchpad and Space-switching variants as the flags select them.
    pub fn run(log: &mut dyn FnMut(&str)) {
        // SAFETY: the call only asks whether this process holds the Accessibility permission.
        if !unsafe { AXIsProcessTrusted() } {
            log("needs Accessibility; stopping");
            return;
        }
        let settings = PipelineSettings::default();
        let Some((touch_id, _)) =
            pipeline::locate_touch_display(settings.display_vendor, settings.display_model)
        else {
            log("touch display not found; stopping");
            return;
        };
        CursorVisibility::allow_hiding_in_background();
        let mut session = Session::new(touch_id, log);
        let args: Vec<String> = std::env::args().collect();
        let wants_nc = args.iter().any(|arg| arg == "--nc");
        let wants_launchpad = args.iter().any(|arg| arg == "--launchpad");
        let wants_spaces = args.iter().any(|arg| arg == "--spaces");
        let all = !wants_nc && !wants_launchpad && !wants_spaces;
        session.write(&format!("start: {}", session.state()));
        if wants_spaces {
            session.space_switch();
        }
        if all || wants_nc {
            session.notification_center();
        }
        if all || wants_launchpad {
            session.launchpad(args.iter().any(|arg| arg == "--move-dock"));
        }
        session.return_cursor();
        session.write(&format!("done: {}", session.state()));
    }
}

/// The state one run of the self-test works with: the touch display, the cursor's home, the swipe
/// poster, and whether this run has hidden the cursor.
struct Session<'a> {
    log: &'a mut dyn FnMut(&str),
    touch: ScreenRect,
    touch_uuid: Option<String>,
    swipe: DockSwipe,
    source: CFRetained<CGEventSource>,
    /// Where the cursor was at the start; every variant puts it back here.
    home: CGPoint,
    hidden: bool,
}

impl<'a> Session<'a> {
    fn new(touch_id: u32, log: &'a mut dyn FnMut(&str)) -> Self {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .expect("CGEventSource for the HID system state");
        Session {
            log,
            touch: SystemDisplays::rect(touch_id),
            touch_uuid: SystemDisplays::uuid(touch_id),
            swipe: DockSwipe::new(),
            source,
            home: CGEvent::new(None)
                .map(|event| CGEvent::location(Some(&event)))
                .unwrap_or(CGPoint::new(0.0, 0.0)),
            hidden: false,
        }
    }

    fn write(&mut self, line: &str) {
        (self.log)(line);
    }

    fn touch_centre(&self) -> CGPoint {
        CGPoint::new(self.touch.x + self.touch.width / 2.0, self.touch.y + self.touch.height / 2.0)
    }

    // MARK: Notification Center

    fn notification_center(&mut self) {
        self.close_notification_center(); // start from a closed panel
        // Presses first, while the active menu bar is still where the owner works (clicks on the
        // touch display's clock move it there): this separates "follows the cursor" from "follows
        // the menu bar".
        self.nc("nc-1 press the active clock, cursor untouched (v0.5.0)", |_session| {
            let Some(clock) = NotificationCenterOpener::clock() else { return false };
            NotificationCenterOpener::press(&clock)
        });
        self.nc(
            "nc-2 press the active clock, cursor held on the touch display 1 s",
            |session| {
                let Some(clock) = NotificationCenterOpener::clock() else { return false };
                let centre = session.touch_centre();
                session.park(centre, false);
                NotificationCenterOpener::press(&clock)
            },
        );
        self.nc(
            "nc-3 press the active clock, cursor moved (with events) onto the touch display and held 1 s",
            |session| {
                let Some(clock) = NotificationCenterOpener::clock() else { return false };
                let centre = session.touch_centre();
                session.park(centre, true);
                NotificationCenterOpener::press(&clock)
            },
        );
        self.nc(
            "nc-4 click the touch display's clock, cursor back after 50 ms (v0.5.1)",
            |session| {
                let Some(p) = session.touch_clock() else { return false };
                session.park(p, false);
                session.click(p);
                std::thread::sleep(Duration::from_millis(50));
                session.return_cursor();
                true
            },
        );
        self.nc(
            "nc-5 click the touch display's clock, cursor held there 1 s",
            |session| {
                let Some(p) = session.touch_clock() else { return false };
                session.park(p, false);
                session.click(p);
                true
            },
        );
    }

    /// Runs one named trigger between the state line and the result line, and closes the panel again.
    fn nc(&mut self, name: &str, trigger: impl FnOnce(&mut Session<'a>) -> bool) {
        self.write(&format!("{name}: {}", self.state()));
        let triggered = trigger(self);
        std::thread::sleep(Duration::from_secs(1));
        self.write(&format!(
            "  → {}Notification Center on {}",
            if triggered { "" } else { "trigger failed; " },
            self.notification_center_display(),
        ));
        self.return_cursor();
        self.close_notification_center();
    }

    /// Toggles the panel shut through the active clock, then through the clock on the panel's display.
    fn close_notification_center(&mut self) {
        if self.notification_center_display() == "closed" {
            return;
        }
        if let Some(clock) = NotificationCenterOpener::clock() {
            NotificationCenterOpener::press(&clock);
        }
        std::thread::sleep(Duration::from_millis(800));
        if self.notification_center_display() == "touch display"
            && let Some(p) = self.touch_clock()
        {
            self.park(p, false);
            self.click(p);
            std::thread::sleep(Duration::from_millis(300));
            self.return_cursor();
            std::thread::sleep(Duration::from_millis(800));
        }
        let now = self.notification_center_display();
        self.write(&if now == "closed" {
            "  closed".to_string()
        } else {
            format!("  still open on {now}: please close it")
        });
    }

    /// The touch display's own clock, only if a ControlCenter status item is visible there.
    fn touch_clock(&mut self) -> Option<CGPoint> {
        let Some(clock) = NotificationCenterOpener::clock() else {
            self.write("  no clock");
            return None;
        };
        let centre = ScreenPoint { x: clock.frame.mid().x, y: clock.frame.mid().y };
        let source = SystemDisplays::online().into_iter().map(SystemDisplays::rect).find(|rect| rect.contains(centre))?;
        let p = MenuBarGeometry::same_status_item(centre, source, self.touch);
        let windows = WindowLocator::on_screen_windows();
        let top = WindowHitTester::top_window(p, &windows, -1);
        if top.map(|w| w.pid) != Some(clock.pid) {
            self.write("  touch display's clock not visible");
            return None;
        }
        Some(CGPoint::new(p.x, p.y))
    }

    fn notification_center_display(&self) -> String {
        let panels = NotificationCenterOpener::open_panels();
        if panels.is_empty() {
            "closed".to_string()
        } else {
            panels
                .iter()
                .map(|rect| self.display_name_centre(rect))
                .collect::<Vec<_>>()
                .join(" + ")
        }
    }

    // MARK: Space switching

    /// Does a Space switch land on the touch display with only the hidden cursor there (no window
    /// focused, the other display keeping the active menu bar)? Switches back afterwards.
    fn space_switch(&mut self) {
        let touch_uuid = self.touch_uuid.clone();
        let Some(touch_spaces) = SystemDisplays::space_lists().into_iter().find(|list| {
            Some(list.uuid.as_str()) == touch_uuid.as_deref()
        }) else {
            self.write("the touch display has a single Space; nothing to switch");
            return;
        };
        let Some(index) = touch_spaces
            .spaces
            .iter()
            .position(|space| *space == touch_spaces.current)
        else {
            self.write("the touch display has a single Space; nothing to switch");
            return;
        };
        if touch_spaces.spaces.len() <= 1 {
            self.write("the touch display has a single Space; nothing to switch");
            return;
        }
        let d = if index > 0 { -0.05 } else { 0.05 }; // towards a Space that exists: negative = left
        let before = SystemDisplays::current_spaces();
        self.write(&format!(
            "sp-1 Space switch {}, cursor held on the touch display, nothing focused: {}",
            if d < 0.0 { "left" } else { "right" },
            self.state(),
        ));
        let centre = self.touch_centre();
        self.park(centre, false);
        post_swipe(&mut self.swipe, Kind::Horizontal, d, false);
        std::thread::sleep(Duration::from_millis(1_200));
        self.write(&format!("  → {}", self.changes(&before)));
        post_swipe(&mut self.swipe, Kind::Horizontal, -d, false);
        std::thread::sleep(Duration::from_millis(1_200));
        let restored = SystemDisplays::current_spaces()
            .iter()
            .map(|current| current.1)
            .eq(before.iter().map(|entry| entry.1));
        self.write(&if restored {
            "  switched back".to_string()
        } else {
            format!("  not back where it started: {}", self.changes(&before))
        });
        self.return_cursor();
    }

    fn changes(&self, before: &[(String, u64)]) -> String {
        let changed: Vec<&str> = SystemDisplays::current_spaces()
            .iter()
            .filter(|current| {
                before
                    .iter()
                    .find(|entry| entry.0 == current.0)
                    .map(|entry| entry.1)
                    != Some(current.1)
            })
            .map(|current| {
                if Some(current.0.as_str()) == self.touch_uuid.as_deref() {
                    "touch display"
                } else {
                    "other display"
                }
            })
            .collect();
        if changed.is_empty() {
            "no Space changed".to_string()
        } else {
            format!("Space changed on {}", changed.join(" + "))
        }
    }

    // MARK: Launchpad

    fn launchpad(&mut self, move_dock: bool) {
        self.close_launchpad();
        self.lp("lp-1 pinch, cursor held on the touch display (v0.5.1)", |session| {
            let centre = session.touch_centre();
            session.park(centre, false);
            session.pinch(true);
        });
        self.lp(
            "lp-2 pinch, cursor moved (with events) onto the touch display and held",
            |session| {
                let centre = session.touch_centre();
                session.park(centre, true);
                session.pinch(true);
            },
        );
        self.lp(
            "lp-3 open Launchpad.app, cursor moved (with events) onto the touch display and held",
            |session| {
                let centre = session.touch_centre();
                session.park(centre, true);
                NSWorkspace::sharedWorkspace().openURL(&NSURL::fileURLWithPath(
                    &NSString::from_str("/System/Applications/Launchpad.app"),
                ));
            },
        );
        if !move_dock {
            return;
        }
        let dock_was_on = self.dock_display();
        let touch = self.touch;
        self.push_dock(touch);
        self.lp(
            "lp-4 pinch with the Dock moved to the touch display, cursor held there",
            |session| {
                let centre = session.touch_centre();
                session.park(centre, false);
                session.pinch(true);
            },
        );
        if dock_was_on == "other display" {
            let other = SystemDisplays::online()
                .into_iter()
                .map(SystemDisplays::rect)
                .find(|rect| *rect != touch);
            if let Some(other) = other {
                self.push_dock(other);
            }
        }
        self.return_cursor();
    }

    /// Runs one named trigger between the state line and the result line, and closes Launchpad again.
    fn lp(&mut self, name: &str, trigger: impl FnOnce(&mut Session<'a>)) {
        self.write(&format!("{name}: {}", self.state()));
        trigger(self);
        std::thread::sleep(Duration::from_millis(1_200));
        self.write(&format!("  → Launchpad on {}", self.launchpad_display()));
        self.return_cursor();
        self.close_launchpad();
    }

    /// Pinching out closes an open Launchpad (and would show the desktop if none were open, so only then).
    fn close_launchpad(&mut self) {
        if self.launchpad_display() == "closed" {
            return;
        }
        self.pinch(false);
        std::thread::sleep(Duration::from_millis(1_200));
        let now = self.launchpad_display();
        self.write(&if now == "closed" {
            "  closed".to_string()
        } else {
            format!("  still open on {now}: please close it (Esc)")
        });
    }

    /// The thumb-pinch dock swipe as the pipeline sends it (inverted flag on macOS 15).
    fn pinch(&mut self, inward: bool) {
        // Inward (positive) opens Launchpad, outward closes it; the pipeline sends the pinch inverted.
        let d = if inward { 0.05 } else { -0.05 };
        post_swipe(&mut self.swipe, Kind::Pinch, d, true);
    }

    fn launchpad_display(&self) -> String {
        let windows = DockLocator::launchpad_windows();
        if windows.is_empty() {
            "closed".to_string()
        } else {
            windows
                .iter()
                .map(|rect| self.display_name_centre(rect))
                .collect::<Vec<_>>()
                .join(" + ")
        }
    }

    /// The Dock moves to a display when the cursor pushes against that display's bottom edge.
    fn push_dock(&mut self, display: ScreenRect) {
        let p = CGPoint::new(display.x + display.width / 2.0, display.y + display.height - 1.0);
        self.park(p, false);
        for _ in 0..40 {
            if let Some(e) =
                CGEvent::new_mouse_event(Some(&self.source), CGEventType::MouseMoved, p, CGMouseButton::Left)
            {
                CGEvent::set_integer_value_field(Some(&e), CGEventField::MouseEventDeltaY, 4);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        std::thread::sleep(Duration::from_millis(800));
        self.write(&format!(
            "pushed the Dock towards {}: Dock now on {}",
            self.display_name_centre(&display),
            self.dock_display(),
        ));
    }

    // MARK: cursor and state

    fn park(&mut self, p: CGPoint, with_moves: bool) {
        if !self.hidden {
            CursorVisibility::hide();
            self.hidden = true;
        }
        CGWarpMouseCursorPosition(p);
        if !with_moves {
            return;
        }
        for i in 0..5i32 {
            let at = CGPoint::new(p.x + i as f64, p.y);
            if let Some(e) =
                CGEvent::new_mouse_event(Some(&self.source), CGEventType::MouseMoved, at, CGMouseButton::Left)
            {
                CGEvent::set_integer_value_field(Some(&e), CGEventField::MouseEventDeltaX, 1);
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn return_cursor(&mut self) {
        if !self.hidden {
            return;
        }
        CGWarpMouseCursorPosition(self.home);
        CGAssociateMouseAndMouseCursorPosition(true);
        CursorVisibility::show();
        self.hidden = false;
    }

    fn click(&mut self, p: CGPoint) {
        for event_type in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
            if let Some(e) =
                CGEvent::new_mouse_event(Some(&self.source), event_type, p, CGMouseButton::Left)
            {
                CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&e));
            }
        }
    }

    fn state(&self) -> String {
        let cursor = CGEvent::new(None)
            .map(|event| CGEvent::location(Some(&event)))
            .unwrap_or(CGPoint::new(0.0, 0.0));
        let bar = SystemDisplays::active_menu_bar_display_uuid();
        let bar_name = match bar.as_deref() {
            None => "unknown",
            Some(uuid) if Some(uuid) == self.touch_uuid.as_deref() => "touch display",
            _ => "other display",
        };
        format!(
            "cursor on {}, active menu bar on {}, Dock on {}",
            self.display_name(ScreenPoint { x: cursor.x, y: cursor.y }),
            bar_name,
            self.dock_display(),
        )
    }

    fn dock_display(&self) -> String {
        match DockLocator::anchor() {
            Some(anchor) => self.display_name(anchor),
            None => "unknown".to_string(),
        }
    }

    fn display_name_centre(&self, rect: &ScreenRect) -> String {
        self.display_name(ScreenPoint {
            x: rect.x + rect.width / 2.0,
            y: rect.y + rect.height / 2.0,
        })
    }

    fn display_name(&self, p: ScreenPoint) -> String {
        if self.touch.contains(p) {
            return "touch display".to_string();
        }
        if SystemDisplays::online()
            .into_iter()
            .map(SystemDisplays::rect)
            .any(|rect| rect.contains(p))
        {
            "other display".to_string()
        } else {
            "no display".to_string()
        }
    }
}
