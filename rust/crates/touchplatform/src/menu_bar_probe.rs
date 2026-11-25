//! Diagnostics on menu bars (`--ax-probe --menubar`, `--ax-probe --menu`).
//!
//! `--menubar` is read-only: what decides the display that Notification Center, Launchpad and Space
//! switching use. It logs each display's menu bar, the clock as Accessibility reports it on every
//! display, the active menu-bar display and current Space per display, and the Dock's windows.
//!
//! `--menu` lists the installed copy's own menu as Accessibility reports it, status lines included.
//! With a `--press "Title"` for each it presses those items in turn, through Accessibility (the
//! cursor stays where it is), and lists the menu again after each: the app's menu, driven without
//! a hand on it.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayIsMain, CGDisplayModelNumber, CGDisplayVendorNumber,
};
use objc2_application_services::AXUIElement;
use objc2_core_graphics::CGEvent;
use touchcore::mapper::ScreenPoint;
use touchcore::menubar::MenuBarGeometry;
use touchcore::policy::WindowHitTester;

use crate::accessibility::{self, ENABLED, EXTRAS_MENU_BAR, IDENTIFIER, MARK, ROLE, SUBROLE, TITLE};
use crate::apps;
use crate::swift_text::{swift_f64, swift_point, swift_rect};
use crate::system_displays::SystemDisplays;
use crate::window::WindowLocator;

/// Runs the menu-bar probes.
pub struct MenuBarProbe;

/// This app's bundle identifier: how the installed copy is told from everything else.
const BUNDLE_ID: &str = "com.gramini.touchnext";

/// One item of a menu as Accessibility reports it: how it is named here (`Touch Mode/Drawing…` for
/// an item of a submenu), whether it is chosen, and whether it can be.
struct MenuItem {
    path: String,
    chosen: bool,
    enabled: bool,
    element: CFRetained<AXUIElement>,
}

impl MenuBarProbe {
    /// Lists the installed copy's menu, presses each item named in `presses` in turn, and lists
    /// the menu again after each press.
    pub fn drive(presses: &[String], log: &mut dyn FnMut(&str)) {
        let copies = apps::others_with_bundle_id(BUNDLE_ID);
        let Some(app) = copies.first() else {
            log("no other TouchNext is running");
            return;
        };
        let pid = app.processIdentifier();
        log(&format!(
            "TouchNext pid {pid}: {}",
            app.bundleURL().and_then(|url| url.path()).map(|path| path.to_string()).unwrap_or_else(|| "?".to_owned())
        ));
        let element = accessibility::application(pid, 2.0);
        let listing = || list(&element);
        dump(&listing(), log);
        for press in presses {
            let Some(item) = listing().into_iter().find(|item| item.path == *press) else {
                log(&format!("press {press}: no such item"));
                continue;
            };
            // The press is answered on the app's main thread; its effects follow a moment later.
            let accepted = accessibility::press(&item.element);
            std::thread::sleep(std::time::Duration::from_millis(1500));
            log(&format!("pressed {press}: {}", if accepted { "accepted" } else { "refused" }));
            dump(&listing(), log);
        }
        log("done");
    }

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

/// The items of the status item's menu, each submenu's items after their parent.
fn list(app: &AXUIElement) -> Vec<MenuItem> {
    let describe = |element: CFRetained<AXUIElement>, path: String| MenuItem {
        chosen: accessibility::string(&element, MARK).is_some_and(|mark| !mark.is_empty()),
        enabled: accessibility::flag(&element, ENABLED).unwrap_or(true),
        element,
        path,
    };
    let Some(bar) = accessibility::element(app, EXTRAS_MENU_BAR) else { return Vec::new() };
    let Some(status_item) = accessibility::children(&bar).into_iter().next() else { return Vec::new() };
    let Some(menu) = accessibility::children(&status_item).into_iter().next() else { return Vec::new() };
    let mut items = Vec::new();
    for item in accessibility::children(&menu) {
        let Some(title) = accessibility::string(&item, TITLE).filter(|title| !title.is_empty()) else { continue };
        let submenu = accessibility::children(&item).into_iter().find(|child| role(child) == "AXMenu");
        items.push(describe(item, title.clone()));
        if let Some(submenu) = submenu {
            for entry in accessibility::children(&submenu) {
                if let Some(name) = accessibility::string(&entry, TITLE).filter(|name| !name.is_empty()) {
                    items.push(describe(entry, format!("{title}/{name}")));
                }
            }
        }
    }
    items
}

/// Logs every item: a check mark on a chosen one, and "disabled" on a status line.
fn dump(items: &[MenuItem], log: &mut dyn FnMut(&str)) {
    for item in items {
        let mark = if item.chosen { "✓ " } else { "  " };
        let submenu = if item.path.contains('/') { "    " } else { "" };
        let state = if item.enabled { "" } else { "  (disabled)" };
        log(&format!("  {submenu}{mark}{}{state}", item.path));
    }
}
