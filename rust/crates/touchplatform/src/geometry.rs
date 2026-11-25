//! Conversions between the core's screen geometry and Core Graphics points and rects.
//!
//! The core knows nothing of the platform, so its `ScreenPoint` and `ScreenRect` are what this crate's
//! API speaks; Core Graphics and AppKit speak `CGPoint` and `CGRect`. Every crossing goes through
//! these functions.

use objc2_core_foundation::{CGPoint, CGRect};
use touchcore::mapper::{ScreenPoint, ScreenRect};

/// A core point as Core Graphics sees it.
pub fn cg_point(point: ScreenPoint) -> CGPoint {
    CGPoint::new(point.x, point.y)
}

/// A Core Graphics point in the core's terms.
pub fn screen_point(point: CGPoint) -> ScreenPoint {
    ScreenPoint { x: point.x, y: point.y }
}

/// A Core Graphics rect in the core's terms.
pub fn screen_rect(rect: CGRect) -> ScreenRect {
    ScreenRect { x: rect.origin.x, y: rect.origin.y, width: rect.size.width, height: rect.size.height }
}
