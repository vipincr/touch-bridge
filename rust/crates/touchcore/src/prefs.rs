//! The Mac's trackpad gesture settings (defaults domain
//! com.apple.AppleMultitouchTrackpad), so the touchscreen can behave like the
//! trackpad. Gestures TouchNext doesn't implement yet are read too.

use std::collections::HashMap;

use crate::gestures::GestureConfig;

/// One value as `defaults` stores it: a boolean flag, an integer mode, or a double.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PrefValue {
    Bool(bool),
    Int(i64),
    Float(f64),
}

/// The trackpad settings the touchscreen follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackpadPreferences {
    /// macOS default: off.
    pub three_finger_drag: bool,
    pub scroll: bool,
    pub momentum: bool,
    pub pinch: bool,
    pub rotate: bool,
    /// Four fingers up / down. Not implemented yet (docs/backlog.md); read so
    /// they can follow the settings when they are.
    pub mission_control_swipe: bool,
    /// Four fingers left / right.
    pub spaces_swipe: bool,
    /// Thumb + three/four fingers.
    pub launchpad_desktop_pinch: bool,
    /// Two fingers from the right edge.
    pub notification_center_swipe: bool,
    /// Three-finger tap.
    pub look_up_tap: bool,
    /// Two-finger double tap.
    pub smart_zoom: bool,
}

impl Default for TrackpadPreferences {
    /// The macOS defaults for every key.
    fn default() -> Self {
        Self {
            three_finger_drag: false,
            scroll: true,
            momentum: true,
            pinch: true,
            rotate: true,
            mission_control_swipe: true,
            spaces_swipe: true,
            launchpad_desktop_pinch: true,
            notification_center_swipe: true,
            look_up_tap: true,
            smart_zoom: true,
        }
    }
}

impl TrackpadPreferences {
    pub fn new() -> Self {
        Self::default()
    }

    /// The keys read from the trackpad's defaults domain.
    pub const KEYS: &[&str] = &[
        "TrackpadThreeFingerDrag",
        "TrackpadScroll",
        "TrackpadMomentumScroll",
        "TrackpadPinch",
        "TrackpadRotate",
        "TrackpadFourFingerVertSwipeGesture",
        "TrackpadFourFingerHorizSwipeGesture",
        "TrackpadFourFingerPinchGesture",
        "TrackpadFiveFingerPinchGesture",
        "TrackpadTwoFingerFromRightEdgeSwipeGesture",
        "TrackpadThreeFingerTapGesture",
        "TrackpadTwoFingerDoubleTapGesture",
    ];

    /// Reads the defaults domain. Values are 0 (off) or a non-zero mode/flag
    /// (on); missing keys keep the macOS default.
    pub fn from_values(values: &HashMap<String, PrefValue>) -> Self {
        let on = |key: &str, fallback: bool| match values.get(key) {
            Some(PrefValue::Bool(b)) => *b,
            Some(PrefValue::Int(i)) => *i != 0,
            Some(PrefValue::Float(d)) => *d != 0.0,
            None => fallback,
        };
        Self {
            three_finger_drag: on("TrackpadThreeFingerDrag", false),
            scroll: on("TrackpadScroll", true),
            momentum: on("TrackpadMomentumScroll", true),
            pinch: on("TrackpadPinch", true),
            rotate: on("TrackpadRotate", true),
            mission_control_swipe: on("TrackpadFourFingerVertSwipeGesture", true),
            spaces_swipe: on("TrackpadFourFingerHorizSwipeGesture", true),
            launchpad_desktop_pinch: on("TrackpadFourFingerPinchGesture", true) || on("TrackpadFiveFingerPinchGesture", false),
            notification_center_swipe: on("TrackpadTwoFingerFromRightEdgeSwipeGesture", true),
            look_up_tap: on("TrackpadThreeFingerTapGesture", true),
            smart_zoom: on("TrackpadTwoFingerDoubleTapGesture", true),
        }
    }

    /// Writes the followed settings into the recognizer's configuration.
    pub fn apply(&self, config: &mut GestureConfig) {
        config.three_finger_drag = self.three_finger_drag;
        config.scroll_enabled = self.scroll;
        config.pinch_enabled = self.pinch;
        config.rotate_enabled = self.rotate;
        config.four_finger_vertical_swipe = self.mission_control_swipe;
        config.four_finger_horizontal_swipe = self.spaces_swipe;
        config.four_finger_pinch = self.launchpad_desktop_pinch;
        config.notification_center_swipe = self.notification_center_swipe;
    }

    /// The settings TouchNext follows, in menu order. One entry per menu line
    /// (owner: the single summary line made the menu too wide).
    pub fn settings(&self) -> Vec<(&'static str, bool)> {
        vec![
            ("3-finger drag", self.three_finger_drag),
            ("scroll", self.scroll),
            ("momentum", self.momentum),
            ("pinch", self.pinch),
            ("rotate", self.rotate),
            ("Mission Control", self.mission_control_swipe),
            ("Spaces", self.spaces_swipe),
            ("Launchpad/Desktop", self.launchpad_desktop_pinch),
            ("Notification Center", self.notification_center_swipe),
        ]
    }

    /// One line for the log.
    pub fn summary(&self) -> String {
        self.settings()
            .iter()
            .map(|(name, on)| format!("{name} {}", if *on { "on" } else { "off" }))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's values from `defaults read com.apple.AppleMultitouchTrackpad` (2026-10-09).
    fn owners_trackpad() -> HashMap<String, PrefValue> {
        HashMap::from([
            ("TrackpadThreeFingerDrag".to_string(), PrefValue::Int(1)),
            ("TrackpadScroll".to_string(), PrefValue::Int(1)),
            ("TrackpadMomentumScroll".to_string(), PrefValue::Int(1)),
            ("TrackpadPinch".to_string(), PrefValue::Int(1)),
            ("TrackpadRotate".to_string(), PrefValue::Int(1)),
            ("TrackpadFourFingerVertSwipeGesture".to_string(), PrefValue::Int(2)),
            ("TrackpadFourFingerHorizSwipeGesture".to_string(), PrefValue::Int(2)),
            ("TrackpadFourFingerPinchGesture".to_string(), PrefValue::Int(2)),
            ("TrackpadFiveFingerPinchGesture".to_string(), PrefValue::Int(2)),
            ("TrackpadTwoFingerFromRightEdgeSwipeGesture".to_string(), PrefValue::Int(3)),
            ("TrackpadThreeFingerTapGesture".to_string(), PrefValue::Int(2)),
            ("TrackpadTwoFingerDoubleTapGesture".to_string(), PrefValue::Int(1)),
        ])
    }

    #[test]
    fn reads_the_owners_trackpad_settings() {
        let prefs = TrackpadPreferences::from_values(&owners_trackpad());
        assert!(prefs.three_finger_drag && prefs.scroll && prefs.momentum && prefs.pinch && prefs.rotate);
        assert!(prefs.mission_control_swipe && prefs.spaces_swipe && prefs.launchpad_desktop_pinch && prefs.notification_center_swipe);
        assert!(prefs.look_up_tap && prefs.smart_zoom);
    }

    #[test]
    fn zero_means_off() {
        let values = HashMap::from([
            ("TrackpadPinch".to_string(), PrefValue::Int(0)),
            ("TrackpadThreeFingerDrag".to_string(), PrefValue::Bool(false)),
            ("TrackpadTwoFingerFromRightEdgeSwipeGesture".to_string(), PrefValue::Int(0)),
        ]);
        let prefs = TrackpadPreferences::from_values(&values);
        assert!(!prefs.pinch);
        assert!(!prefs.three_finger_drag);
        assert!(!prefs.notification_center_swipe);
    }

    #[test]
    fn missing_keys_use_macos_defaults() {
        let prefs = TrackpadPreferences::from_values(&HashMap::new());
        assert!(!prefs.three_finger_drag, "three-finger drag is off by default on macOS");
        assert!(prefs.scroll && prefs.momentum && prefs.pinch && prefs.rotate);
    }

    #[test]
    fn applies_to_the_gesture_configuration() {
        let values = HashMap::from([
            ("TrackpadThreeFingerDrag".to_string(), PrefValue::Int(0)),
            ("TrackpadPinch".to_string(), PrefValue::Int(0)),
            ("TrackpadRotate".to_string(), PrefValue::Int(1)),
            ("TrackpadScroll".to_string(), PrefValue::Int(0)),
        ]);
        let mut config = GestureConfig::default();
        TrackpadPreferences::from_values(&values).apply(&mut config);
        assert!(!config.three_finger_drag);
        assert!(!config.pinch_enabled);
        assert!(config.rotate_enabled);
        assert!(!config.scroll_enabled);
    }

    // The menu lists the followed settings one per line (owner: the single summary line made the menu too wide).
    #[test]
    fn the_followed_settings_are_listed_one_per_line() {
        let values = HashMap::from([("TrackpadPinch".to_string(), PrefValue::Int(0))]);
        let prefs = TrackpadPreferences::from_values(&values);
        assert_eq!(
            prefs.settings().iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["3-finger drag", "scroll", "momentum", "pinch", "rotate", "Mission Control", "Spaces", "Launchpad/Desktop", "Notification Center"]
        );
        assert_eq!(prefs.settings().iter().find(|(name, _)| *name == "pinch").map(|(_, on)| *on), Some(false));
        assert!(
            prefs.summary().starts_with("3-finger drag off · scroll on · momentum on · pinch off"),
            "the log line stays as it was"
        );
    }
}
