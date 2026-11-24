//! Maps normalized panel coordinates (native orientation) onto the touch
//! display's global bounds. `rotation` is the display rotation in degrees as
//! the host reports it; macOS "90" turns the picture clockwise on the panel.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl ScreenRect {
    pub fn contains(&self, p: ScreenPoint) -> bool {
        p.x >= self.x && p.x < self.x + self.width && p.y >= self.y && p.y < self.y + self.height
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoordinateMapper {
    pub bounds: ScreenRect,
    pub rotation: i32,
}

impl CoordinateMapper {
    pub fn new(bounds: ScreenRect, rotation_degrees: f64) -> Self {
        Self { bounds, rotation: ((rotation_degrees.round() as i32).rem_euclid(360)) }
    }

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
    /// which the host window server assigns to the neighbouring display.
    pub fn clamped_point(&self, u: f64, v: f64) -> ScreenPoint {
        let p = self.point(u, v);
        ScreenPoint {
            x: p.x.clamp(self.bounds.x, self.bounds.x + self.bounds.width - 0.5),
            y: p.y.clamp(self.bounds.y, self.bounds.y + self.bounds.height - 0.5),
        }
    }
}
