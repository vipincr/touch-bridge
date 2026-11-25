//! Numbers and shapes the way the Swift app printed them. Log lines, probe summaries and the
//! analysis scripts that read them kept that text word for word.

use objc2_core_foundation::{CGPoint, CGRect};

/// A double the way Swift's string interpolation prints it: the shortest form that round-trips, with
/// a decimal point even on whole numbers (`1920.0`).
pub fn swift_f64(value: f64) -> String {
    let mut text = format!("{value}");
    if !text.contains('.') && !text.contains('e') {
        text.push_str(".0");
    }
    text
}

/// A point as Swift prints one: `(2000.0, 100.0)`.
pub fn swift_point(point: CGPoint) -> String {
    format!("({}, {})", swift_f64(point.x), swift_f64(point.y))
}

/// A rectangle as Swift prints one: `((0.0, 0.0), (1920.0, 1080.0))`.
pub fn swift_rect(rect: CGRect) -> String {
    let size = CGPoint::new(rect.size.width, rect.size.height);
    format!("({}, {})", swift_point(rect.origin), swift_point(size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::CGSize;

    #[test]
    fn a_double_keeps_a_decimal_point_on_whole_numbers() {
        assert_eq!(swift_f64(0.0), "0.0");
        assert_eq!(swift_f64(1920.0), "1920.0");
        assert_eq!(swift_f64(-25.5), "-25.5");
    }

    #[test]
    fn a_rectangle_prints_its_origin_then_its_size() {
        let rect = CGRect::new(CGPoint::new(0.0, 25.0), CGSize::new(1440.0, 900.0));
        assert_eq!(swift_rect(rect), "((0.0, 25.0), (1440.0, 900.0))");
    }
}
