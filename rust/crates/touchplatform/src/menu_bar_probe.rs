//! Read-only diagnostic (`--ax-probe --menubar`): what decides the display that Notification Center,
//! Launchpad and Space switching use. Logs each display's menu bar, the clock as Accessibility
//! reports it on every display, the active menu-bar display and current Space per display, and the
//! Dock's windows. Sends no events.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayIsMain, CGDisplayModelNumber, CGDisplayVendorNumber,
};
use objc2_core_graphics::CGEvent;
use touchcore::mapper::ScreenPoint;
use touchcore::menubar::MenuBarGeometry;
use touchcore::policy::WindowHitTester;

use crate::accessibility::{self, EXTRAS_MENU_BAR, IDENTIFIER, ROLE, SUBROLE};
use crate::apps;
use crate::swift_text::{swift_f64, swift_point, swift_rect};
use crate::system_displays::SystemDisplays;
use crate::window::WindowLocator;

/// Runs the menu-bar probe.
pub struct MenuBarProbe;

impl MenuBarProbe {
    /// Samples every display, the menu-bar clocks, the Space state and the Dock's windows, and
    /// logs what decides where each system surface opens.
    pub fn run(mtm: MainThreadMarker, log: &mut dyn FnMut(&str)) {
        for id in SystemDisplays::online() {
            let b = CGDisplayBounds(id);
            log(&format!(
                "display {} vendor {} model {} at {},{} {}×{} uuid {} main {}",
                id,
                CGDisplayVendorNumber(id),
                CGDisplayModelNumber(id),
                b.origin.x as i64,
                b.origin.y as i64,
                b.size.width as i64,
                b.size.height as i64,
                SystemDisplays::uuid(id).unwrap_or_else(|| "?".to_string()),
                CGDisplayIsMain(id),
            ));
        }
        for screen in NSScreen::screens(mtm).iter() {
            let frame = screen.frame();
            let visible = screen.visibleFrame();
            log(&format!(
                "screen {} frame {} visible {} menu bar {} pt",
                screen.localizedName(),
                swift_rect(frame),
                swift_rect(visible),
                swift_f64(frame.max().y - visible.max().y),
            ));
        }
        log(&format!(
            "active menu-bar display {}",
            SystemDisplays::active_menu_bar_display_uuid()
                .unwrap_or_else(|| "unavailable".to_string()),
        ));
        for (uuid, space) in SystemDisplays::current_spaces() {
            log(&format!("current space on {uuid}: {space}"));
        }
        let cursor = CGEvent::new(None)
            .map(|event| CGEvent::location(Some(&event)))
            .unwrap_or(CGPoint::new(0.0, 0.0));
        log(&format!("cursor at {}", swift_point(cursor)));

        let system = accessibility::system_wide(0.3);
        let mut clock_frame: Option<CGRect> = None;
        for bundle in ["com.apple.controlcenter", "com.apple.systemuiserver"] {
            let Some(app) = apps::with_bundle_id(bundle) else { continue };
            let pid = app.processIdentifier();
            let element = accessibility::application(pid, 0.3);
            // An array of strings, as Swift prints one: each in quotes.
            let top: Vec<String> = accessibility::children(&element)
                .iter()
                .map(|child| format!("\"{}\"", role(child)))
                .collect();
            log(&format!("{bundle} pid {pid} top-level children: [{}]", top.join(", ")));
            let Some(bar) = accessibility::element(&element, EXTRAS_MENU_BAR) else {
                log("  no AXExtrasMenuBar");
                continue;
            };
            log(&format!(
                "  AXExtrasMenuBar frame {}",
                accessibility::frame(&bar).map(swift_rect).unwrap_or_else(|| "?".to_string()),
            ));
            for item in accessibility::children(&bar) {
                let id = accessibility::string(&item, IDENTIFIER)
                    .unwrap_or_else(|| "?".to_string());
                log(&format!(
                    "  item {id} frame {}",
                    accessibility::frame(&item).map(swift_rect).unwrap_or_else(|| "?".to_string()),
                ));
                if id == "com.apple.menuextra.clock" {
                    clock_frame = accessibility::frame(&item);
                }
            }
        }

        if let Some(clock) = clock_frame {
            let centre = ScreenPoint { x: clock.mid().x, y: clock.mid().y };
            let displays: Vec<(u32, touchcore::mapper::ScreenRect)> = SystemDisplays::online()
                .into_iter()
                .map(|id| (id, SystemDisplays::rect(id)))
                .collect();
            match displays.iter().find(|(_, rect)| rect.contains(centre)) {
                Some((_, source)) => {
                    let windows = WindowLocator::on_screen_windows();
                    for (id, rect) in &displays {
                        let p = MenuBarGeometry::same_status_item(centre, *source, *rect);
                        let window = WindowHitTester::top_window(p, &windows, -1);
                        let window = window
                            .map(|w| format!("{} pid {} layer {}", w.owner_name, w.pid, w.layer))
                            .unwrap_or_else(|| "none".to_string());
                        log(&format!(
                            "display {id}: clock expected at {},{} → window {}; AX {}",
                            p.x as i64,
                            p.y as i64,
                            window,
                            describe(CGPoint::new(p.x, p.y), &system),
                        ));
                    }
                }
                None => log(&format!("clock centre {} is on no display", swift_screen_point(centre))),
            }
        }

        if let Some(dock) = apps::with_bundle_id("com.apple.dock") {
            let element = accessibility::application(dock.processIdentifier(), 0.3);
            for child in accessibility::children(&element) {
                log(&format!(
                    "Dock AX {} frame {}",
                    role(&child),
                    accessibility::frame(&child).map(swift_rect).unwrap_or_else(|| "?".to_string()),
                ));
            }
        }
        for w in WindowLocator::on_screen_windows()
            .into_iter()
            .filter(|w| w.owner_name == "Dock" || w.owner_name == "Notification Center")
        {
            log(&format!(
                "{} window #{} layer {} at {},{} {}×{} alpha {}",
                w.owner_name,
                w.number,
                w.layer,
                w.bounds.x as i64,
                w.bounds.y as i64,
                w.bounds.width as i64,
                w.bounds.height as i64,
                swift_f64(w.alpha),
            ));
        }
        log("done");
    }
}

/// What the system-wide element reports at a point: the owning app, the element's role, identifier
/// and frame. "nothing" when no element answers there.
fn describe(point: CGPoint, system: &objc2_application_services::AXUIElement) -> String {
    let Some(element) = accessibility::element_at(system, point) else {
        return "nothing".to_string();
    };
    let pid = accessibility::pid(&element).unwrap_or(0);
    let owner = apps::with_pid(pid)
        .and_then(|app| app.bundleIdentifier())
        .map(|id| id.to_string())
        .unwrap_or_else(|| format!("pid {pid}"));
    format!(
        "{} {} id {} frame {}",
        owner,
        role(&element),
        accessibility::string(&element, IDENTIFIER).unwrap_or_else(|| "-".to_string()),
        accessibility::frame(&element).map(swift_rect).unwrap_or_else(|| "?".to_string()),
    )
}

/// An element's role and subrole, as the probe logs them ("AXButton/AXIconButton").
fn role(element: &objc2_application_services::AXUIElement) -> String {
    format!(
        "{}{}",
        accessibility::string(element, ROLE).unwrap_or_else(|| "?".to_string()),
        accessibility::string(element, SUBROLE)
            .map(|subrole| format!("/{subrole}"))
            .unwrap_or_default(),
    )
}

/// A core point as Swift prints a struct of this shape: `ScreenPoint(x: x, y: y)`.
fn swift_screen_point(point: ScreenPoint) -> String {
    format!(
        "ScreenPoint(x: {}, y: {})",
        swift_f64(point.x),
        swift_f64(point.y),
    )
}
