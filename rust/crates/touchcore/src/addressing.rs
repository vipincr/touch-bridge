//! Direct delivery must address a window the way the window server does when
//! it routes an event: the window number plus the point relative to the
//! window's top-left corner (found on hardware, see spec).

use crate::mapper::{ScreenPoint, ScreenRect};

/// Direct-delivery event addressing.
pub struct DirectAddressing;

impl DirectAddressing {
    /// The touch point in the window's own coordinates, as posted with the
    /// window number in a direct event.
    pub fn window_location(point: ScreenPoint, window: ScreenRect) -> ScreenPoint {
        ScreenPoint { x: point.x - window.x, y: point.y - window.y }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_location_is_the_global_point_relative_to_the_windows_top_left() {
        let window = ScreenRect { x: 2238.0, y: 212.0, width: 900.0, height: 628.0 };
        assert_eq!(
            DirectAddressing::window_location(ScreenPoint { x: 2488.0, y: 662.0 }, window),
            ScreenPoint { x: 250.0, y: 450.0 }
        );
    }

    #[test]
    fn window_location_of_the_window_origin_is_zero() {
        let window = ScreenRect { x: -1200.0, y: 40.0, width: 800.0, height: 600.0 };
        assert_eq!(
            DirectAddressing::window_location(ScreenPoint { x: -1200.0, y: 40.0 }, window),
            ScreenPoint { x: 0.0, y: 0.0 }
        );
    }
}
