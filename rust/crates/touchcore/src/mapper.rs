//! Maps normalized panel coordinates (native orientation) onto the touch
//! display's global bounds. `rotation` is the display rotation in degrees as
//! CGDisplayRotation reports it; macOS "90" turns the picture clockwise on the
//! panel (to be confirmed on hardware in M5).

/// A point in global display space (CoreGraphics points, top-left origin of the main display).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPoint {
    pub x: f64,
    pub y: f64,
}

/// A display's bounds in global display space. Kept free of CoreGraphics so touchcore stays pure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ScreenRect {
    /// CoreGraphics global bounds are half-open: the top-left edge belongs to
    /// the display, the bottom-right edge belongs to the neighbour.
    pub fn contains(&self, p: ScreenPoint) -> bool {
        p.x >= self.x && p.x < self.x + self.width && p.y >= self.y && p.y < self.y + self.height
    }
}

/// A coordinate mapper for one touch display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoordinateMapper {
    pub bounds: ScreenRect,
    /// The display rotation in degrees, normalized into 0..359.
    pub rotation: i32,
}

impl CoordinateMapper {
    pub fn new(bounds: ScreenRect, rotation: f64) -> Self {
        Self { bounds, rotation: ((rotation.round() as i32).rem_euclid(360)) }
    }

    /// Maps a normalized panel point onto the display's global bounds.
    pub fn point(&self, u: f64, v: f64) -> ScreenPoint {
        let (lx, ly) = match self.rotation {
            90 => (v, 1.0 - u),
            180 => (1.0 - u, 1.0 - v),
            270 => (1.0 - v, u),
            _ => (u, v),
        };
        ScreenPoint { x: self.bounds.x + lx * self.bounds.width, y: self.bounds.y + ly * self.bounds.height }
    }

    /// Like [`CoordinateMapper::point`], but never lands on the far edge,
    /// which CoreGraphics assigns to the neighbouring display.
    pub fn clamped_point(&self, u: f64, v: f64) -> ScreenPoint {
        let p = self.point(u, v);
        ScreenPoint {
            x: p.x.min(self.bounds.x + self.bounds.width - 0.5).max(self.bounds.x),
            y: p.y.min(self.bounds.y + self.bounds.height - 0.5).max(self.bounds.y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ARZOPA sits to the right of the MacBook screen in global points.
    const ARZOPA: ScreenRect = ScreenRect { x: 1728.0, y: 0.0, width: 1920.0, height: 1080.0 };

    #[test]
    fn maps_panel_corners_and_centre_onto_the_display_bounds() {
        let mapper = CoordinateMapper::new(ARZOPA, 0.0);
        assert_eq!(mapper.point(0.0, 0.0), ScreenPoint { x: 1728.0, y: 0.0 });
        assert_eq!(mapper.point(1.0, 1.0), ScreenPoint { x: 3648.0, y: 1080.0 });
        assert_eq!(mapper.point(0.5, 0.5), ScreenPoint { x: 2688.0, y: 540.0 });
    }

    #[test]
    fn rotation_180_flips_both_axes() {
        let mapper = CoordinateMapper::new(ARZOPA, 180.0);
        assert_eq!(mapper.point(0.0, 0.0), ScreenPoint { x: 3648.0, y: 1080.0 });
        assert_eq!(mapper.point(0.25, 0.75), ScreenPoint { x: 1728.0 + 1440.0, y: 270.0 });
    }

    /// Convention (verified on hardware in M5): macOS "90 degrees" turns the
    /// picture clockwise on the panel, so the panel's native top-right corner
    /// is the desktop's top-left.
    #[test]
    fn rotation_90_maps_native_top_right_to_desktop_top_left() {
        let portrait = ScreenRect { x: 1728.0, y: 0.0, width: 1080.0, height: 1920.0 };
        let mapper = CoordinateMapper::new(portrait, 90.0);
        assert_eq!(mapper.point(1.0, 0.0), ScreenPoint { x: 1728.0, y: 0.0 });
        assert_eq!(mapper.point(0.0, 1.0), ScreenPoint { x: 2808.0, y: 1920.0 });
    }

    #[test]
    fn rotation_270_is_the_mirror_of_90() {
        let portrait = ScreenRect { x: 1728.0, y: 0.0, width: 1080.0, height: 1920.0 };
        let mapper = CoordinateMapper::new(portrait, 270.0);
        assert_eq!(mapper.point(0.0, 1.0), ScreenPoint { x: 1728.0, y: 0.0 });
        assert_eq!(mapper.point(1.0, 0.0), ScreenPoint { x: 2808.0, y: 1920.0 });
    }

    #[test]
    fn last_pixel_stays_inside_the_display() {
        // CG global bounds are half-open; a touch at the panel edge must still hit this display.
        let mapper = CoordinateMapper::new(ARZOPA, 0.0);
        let p = mapper.clamped_point(1.0, 1.0);
        assert!(p.x < 3648.0 && p.y < 1080.0);
    }
}
