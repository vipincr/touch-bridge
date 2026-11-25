//! Read-only diagnostic (`--ax-probe`): samples a grid of points across the top band of on-screen
//! windows and logs what the Accessibility API reports there (role chain up to the window, plus the
//! toolbar's frame). Used to design the window-drag-area rules from real apps. Sends no events.

use objc2::MainThreadMarker;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::CGPoint;
use touchcore::mapper::ScreenPoint;
use touchcore::policy::WindowHitTester;

use crate::accessibility::{self, ROLE, SUBROLE};
use crate::menu_bar_probe::MenuBarProbe;
use crate::window::{AppDirectory, WindowDragArea, WindowLocator};

/// The band a window's top edge is sampled over, in points below its top.
const ROW_OFFSETS: [f64; 11] = [4.0, 12.0, 20.0, 27.0, 32.0, 38.0, 45.0, 52.0, 60.0, 75.0, 95.0];
/// The default view's columns, as fractions of the window's width.
const COLUMNS: [f64; 5] = [0.12, 0.3, 0.5, 0.7, 0.88];

/// Runs the AX probe.
pub struct AXProbe;

impl AXProbe {
    /// Runs the variant the command line asks for: the menu-bar probe (`--menubar`), the drag-area
    /// map (`--map`), one window's role chain column (`--owner NAME`), or the default grid.
    pub fn run(mtm: MainThreadMarker, log: &mut dyn FnMut(&str)) {
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|arg| arg == "--menubar") {
            MenuBarProbe::run(mtm, log);
            return;
        }
        if args.iter().any(|arg| arg == "--map") {
            map(log);
            return;
        }
        if let Some(index) = args.iter().position(|arg| arg == "--owner")
            && args.len() > index + 1
        {
            column(&args[index + 1], log);
            return;
        }
        let system = accessibility::system_wide(0.3);
        let windows: Vec<_> = WindowLocator::on_screen_windows()
            .into_iter()
            .filter(|w| {
                w.layer == 0
                    && w.bounds.width > 300.0
                    && w.bounds.height > 200.0
                    && w.owner_name != "TouchNext"
            })
            .collect();
        for w in windows.iter().take(8) {
            log(&format!(
                "window {} #{} at {},{} {}×{}",
                w.owner_name,
                w.number,
                w.bounds.x as i64,
                w.bounds.y as i64,
                w.bounds.width as i64,
                w.bounds.height as i64,
            ));
            for dy in ROW_OFFSETS {
                let mut row = Vec::new();
                for fx in COLUMNS {
                    let x = w.bounds.x + w.bounds.width * fx;
                    let y = w.bounds.y + dy;
                    row.push(describe(CGPoint::new(x, y), &system, w.pid));
                }
                log(&format!("  dy={:2.0}  {}", dy, row.join(" | ")));
            }
        }
        log("done");
    }
}

/// Role chains down one column (20 % of the width) of one app's window, to explain a map.
fn column(owner: &str, log: &mut dyn FnMut(&str)) {
    let Some(w) = WindowLocator::on_screen_windows().into_iter().find(|w| {
        w.owner_name == owner && w.layer == 0 && w.bounds.width > 300.0
    }) else {
        log(&format!("no window for {owner}"));
        return;
    };
    let system = accessibility::system_wide(0.3);
    log(&format!(
        "window {} #{} at {},{}",
        w.owner_name,
        w.number,
        w.bounds.x as i64,
        w.bounds.y as i64,
    ));
    for step in 0..=23usize {
        let dy = 2.0 + 6.0 * step as f64;
        let p = CGPoint::new(w.bounds.x + w.bounds.width * 0.2, w.bounds.y + dy);
        log(&format!(
            "  {:3.0} pt  {}  {}",
            dy,
            if WindowDragArea::is_drag_area(p, &w, true) { "D" } else { "·" },
            describe(p, &system, w.pid),
        ));
    }
    log("done");
}

/// Runs the real WindowDragArea decision over a grid on each window: D = a pan moves the window.
fn map(log: &mut dyn FnMut(&str)) {
    let windows = WindowLocator::on_screen_windows();
    let apps = AppDirectory::new(); // same Chromium rule as the pipeline
    let chosen: Vec<_> = windows
        .iter()
        .filter(|w| {
            w.layer == 0
                && w.bounds.width > 300.0
                && w.bounds.height > 200.0
                && w.owner_name != "TouchNext"
        })
        .take(8)
        .collect();
    for w in chosen {
        log(&format!(
            "window {} #{} at {},{} {}×{}   (columns = 5%…95% of width)",
            w.owner_name,
            w.number,
            w.bounds.x as i64,
            w.bounds.y as i64,
            w.bounds.width as i64,
            w.bounds.height as i64,
        ));
        for step in 0..=18usize {
            let dy = 2.0 + 6.0 * step as f64;
            let mut row = String::new();
            for column in 1..=19usize {
                let fx = 0.05 * column as f64;
                let p = CGPoint::new(w.bounds.x + w.bounds.width * fx, w.bounds.y + dy);
                let top = WindowHitTester::top_window(ScreenPoint { x: p.x, y: p.y }, &windows, -1);
                if !top.is_some_and(|top| top.number == w.number) {
                    row.push(' '); // covered by another window
                    continue;
                }
                row.push(if WindowDragArea::is_drag_area(p, w, !apps.is_chromium(w.pid)) {
                    'D'
                } else {
                    '·'
                });
            }
            log(&format!("  {:3.0} pt  {}", dy, row));
        }
    }
    log("done");
}

/// The role chain from the element at `point` up to its window, with the pid when the element is
/// not the window the probe expected. "—" when nothing answers there.
fn describe(point: CGPoint, system: &AXUIElement, expected_pid: i32) -> String {
    let Some(element) = accessibility::element_at(system, point) else {
        return "—".to_string();
    };
    let pid = accessibility::pid(&element).unwrap_or(0);
    let mut chain = Vec::new();
    let mut current = Some(element);
    while let Some(element) = current {
        if chain.len() >= 6 {
            break;
        }
        let role = accessibility::string(&element, ROLE).unwrap_or_else(|| "?".to_string());
        let subrole = accessibility::string(&element, SUBROLE)
            .map(|subrole| format!("/{}", strip_ax(&subrole)))
            .unwrap_or_default();
        chain.push(format!("{}{subrole}", strip_ax(&role)));
        if role == "AXWindow" {
            break;
        }
        current = accessibility::parent(&element);
    }
    let prefix = if pid == expected_pid {
        String::new()
    } else {
        format!("[pid {pid}] ")
    };
    format!("{prefix}{}", chain.join("<"))
}

/// Removes the "AX" markers a role or subrole carries, as the Swift probe does.
fn strip_ax(name: &str) -> String {
    name.replace("AX", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ax_removes_every_marker() {
        assert_eq!(strip_ax("AXButton"), "Button");
        assert_eq!(strip_ax("AXStandardWindow"), "StandardWindow");
    }
}
