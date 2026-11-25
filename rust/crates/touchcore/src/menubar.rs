//! Every display has its own menu bar (Displays have separate Spaces); status
//! items such as the clock sit at the same distance from each display's right
//! edge.

use crate::mapper::{ScreenPoint, ScreenRect};

/// Mapping of menu-bar status items between displays.
pub struct MenuBarGeometry;

impl MenuBarGeometry {
    /// Vertical centre of a standard 24 pt menu bar. The notch display's bar is
    /// 38 pt, so its items sit lower; a point mapped from there is kept inside
    /// a standard bar.
    const STANDARD_ITEM_CENTRE: f64 = 12.0;

    /// The point of the status item at the same distance from the target
    /// display's right edge.
    pub fn same_status_item(point: ScreenPoint, source: ScreenRect, target: ScreenRect) -> ScreenPoint {
        ScreenPoint {
            x: target.x + target.width - (source.x + source.width - point.x),
            y: target.y + (point.y - source.y).min(Self::STANDARD_ITEM_CENTRE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACBOOK: ScreenRect = ScreenRect { x: 0.0, y: 0.0, width: 1728.0, height: 1117.0 };
    const ARZOPA: ScreenRect = ScreenRect { x: 1728.0, y: 0.0, width: 1920.0, height: 1080.0 };

    #[test]
    fn the_clock_keeps_its_distance_from_the_right_edge_on_another_display() {
        // Clock 90 pt from the MacBook's right edge, 12 pt below its top.
        let clock = ScreenPoint { x: 1728.0 - 90.0, y: 12.0 };
        assert_eq!(
            MenuBarGeometry::same_status_item(clock, MACBOOK, ARZOPA),
            ScreenPoint { x: 3648.0 - 90.0, y: 12.0 }
        );
    }

    #[test]
    fn on_the_same_display_the_point_is_unchanged() {
        let clock = ScreenPoint { x: 3550.0, y: 11.0 };
        assert_eq!(MenuBarGeometry::same_status_item(clock, ARZOPA, ARZOPA), clock);
    }

    #[test]
    fn displays_below_or_above_keep_the_offset_from_their_own_top() {
        let below = ScreenRect { x: 0.0, y: 1117.0, width: 1920.0, height: 1080.0 };
        assert_eq!(
            MenuBarGeometry::same_status_item(ScreenPoint { x: 1638.0, y: 12.0 }, MACBOOK, below),
            ScreenPoint { x: 1830.0, y: 1129.0 }
        );
    }

    #[test]
    fn a_rect_contains_its_top_left_edge_but_not_its_bottom_right() {
        assert!(ARZOPA.contains(ScreenPoint { x: 1728.0, y: 0.0 }));
        assert!(!ARZOPA.contains(ScreenPoint { x: 3648.0, y: 500.0 }));
        assert!(!MACBOOK.contains(ScreenPoint { x: 1728.0, y: 10.0 }));
    }

    #[test]
    fn from_the_taller_notch_menu_bar_the_click_stays_inside_a_standard_menu_bar() {
        // MacBook menu bar is 38 pt (notch); the clock's centre there is about
        // 19 pt down, the ARZOPA's bar is 24 pt.
        let p = MenuBarGeometry::same_status_item(ScreenPoint { x: 1638.0, y: 19.0 }, MACBOOK, ARZOPA);
        assert_eq!(p, ScreenPoint { x: 3558.0, y: 12.0 });
    }
}
