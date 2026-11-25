//! Scroll momentum after a two-finger flick: per-frame deltas (points) that
//! decay geometrically.

/// One flick's momentum, consumed one frame at a time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Momentum {
    vx: f64,
    vy: f64,
    frame_interval: f64,
    decay: f64,
    min_speed: f64,
}

impl Momentum {
    /// Velocities in points per second, with Swift's defaults: a 1/120 s frame
    /// interval, a decay of 0.95 per frame, and a stop below 20 points/s.
    pub fn new(velocity_x: f64, velocity_y: f64) -> Self {
        Self::with_parameters(velocity_x, velocity_y, 1.0 / 120.0, 0.95, 20.0)
    }

    /// Velocities in points per second, with the caller's frame interval,
    /// decay and minimum speed.
    pub fn with_parameters(velocity_x: f64, velocity_y: f64, frame_interval: f64, decay: f64, min_speed: f64) -> Self {
        Self { vx: velocity_x, vy: velocity_y, frame_interval, decay, min_speed }
    }

    /// The next frame's delta, or `None` once the motion has died out.
    pub fn next(&mut self) -> Option<(f64, f64)> {
        if self.vx.hypot(self.vy) < self.min_speed {
            return None;
        }
        let step = (self.vx * self.frame_interval, self.vy * self.frame_interval);
        self.vx *= self.decay;
        self.vy *= self.decay;
        Some(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_lift_has_no_momentum() {
        let mut m = Momentum::new(5.0, -5.0);
        assert_eq!(m.next(), None);
    }

    #[test]
    fn first_step_is_velocity_times_frame_interval() {
        let mut m = Momentum::with_parameters(0.0, 1200.0, 1.0 / 120.0, 0.95, 20.0);
        let step = m.next().expect("a 1200 points/s flick has momentum");
        assert!(step.0.abs() < 1e-9);
        assert!((step.1 - 10.0).abs() < 1e-9);
    }

    #[test]
    fn steps_decay_and_eventually_stop() {
        let mut m = Momentum::with_parameters(3000.0, 0.0, 1.0 / 120.0, 0.95, 20.0);
        let mut steps: Vec<f64> = Vec::new();
        while let Some(step) = m.next() {
            steps.push(step.0);
        }
        assert!(steps.len() > 10 && steps.len() < 200);
        assert!(steps.windows(2).all(|w| w[0] > w[1]));
        let total: f64 = steps.iter().sum();
        assert!(total < 3000.0 / 120.0 / (1.0 - 0.95)); // bounded by the geometric series
    }
}
