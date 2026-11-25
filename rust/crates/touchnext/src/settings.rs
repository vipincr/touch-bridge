//! Menu settings that survive restarts (UserDefaults of com.gramini.touchnext). "Enabled" and
//! "Dry Run" are per-session on purpose.

use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFBoolean, CFNumber, CFPreferencesCopyAppValue, CFString};
use objc2_foundation::{NSNumber, NSString, NSUserDefaults};
use touchcore::gestures::{OneFingerDragMode, ThreeFingerAnchor};
use touchcore::launchpad::LaunchpadDockMode;
use touchcore::policy::{DeliveryMode, FocusMode};
use touchcore::prefs::TrackpadPreferences;
use touchplatform::pipeline::PipelineSettings;

/// The menu's settings, as `UserDefaults` keeps them between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSettings {
    /// `None` is automatic.
    pub delivery: Option<DeliveryMode>,
    pub gestures_direct: bool,
    pub one_finger_scroll: bool,
    pub return_focus: bool,
    pub three_finger_drag: bool,
    pub three_finger_anchor: ThreeFingerAnchor,
    /// While on, the trackpad's settings decide three-finger drag, scroll, pinch, rotate and
    /// momentum.
    pub follow_trackpad: bool,
    /// Launchpad opens on the display with the Dock: what a pinch-in on the touch display does
    /// about it.
    pub launchpad_dock: LaunchpadDockMode,
    /// A Space switch by touch gives focus back to the app on the other display.
    pub keep_focus_on_space_switch: bool,
    /// Two-finger scrolls drive trackpad page swipes (Safari back/forward).
    pub swipe_between_pages: bool,
}

const DELIVERY: &str = "delivery";
const GESTURES_DIRECT: &str = "gesturesDirect";
const ONE_FINGER_SCROLL: &str = "oneFingerScroll";
const RETURN_FOCUS: &str = "returnFocus";
const THREE_FINGER_DRAG: &str = "threeFingerDrag";
const THREE_FINGER_ANCHOR: &str = "threeFingerAnchor";
const FOLLOW_TRACKPAD: &str = "followTrackpad";
const LAUNCHPAD_DOCK: &str = "launchpadDock";
const KEEP_FOCUS_ON_SPACE_SWITCH: &str = "keepFocusOnSpaceSwitch";
const SWIPE_BETWEEN_PAGES: &str = "swipeBetweenPages";

impl StoredSettings {
    /// The saved settings, with the defaults where nothing was saved.
    pub fn load() -> StoredSettings {
        let defaults = NSUserDefaults::standardUserDefaults();
        let text = |key: &str| defaults.stringForKey(&NSString::from_str(key)).map(|s| s.to_string());
        let flag = |key: &str| {
            defaults.objectForKey(&NSString::from_str(key)).and_then(|value| value.downcast_ref::<NSNumber>().map(|number| number.boolValue()))
        };
        StoredSettings {
            three_finger_drag: flag(THREE_FINGER_DRAG).unwrap_or_else(trackpad_three_finger_drag),
            delivery: text(DELIVERY).and_then(|value| value.parse().ok()),
            gestures_direct: flag(GESTURES_DIRECT).unwrap_or(true),
            one_finger_scroll: flag(ONE_FINGER_SCROLL).unwrap_or(true),
            return_focus: flag(RETURN_FOCUS).unwrap_or(false),
            three_finger_anchor: text(THREE_FINGER_ANCHOR).and_then(|value| value.parse().ok()).unwrap_or_default(),
            follow_trackpad: flag(FOLLOW_TRACKPAD).unwrap_or(true),
            launchpad_dock: text(LAUNCHPAD_DOCK).and_then(|value| value.parse().ok()).unwrap_or(LaunchpadDockMode::BringDockAndReturn),
            keep_focus_on_space_switch: flag(KEEP_FOCUS_ON_SPACE_SWITCH).unwrap_or(true),
            swipe_between_pages: flag(SWIPE_BETWEEN_PAGES).unwrap_or(true),
        }
    }

    /// Saves every setting.
    pub fn save(&self) {
        let defaults = NSUserDefaults::standardUserDefaults();
        let text = |key: &str, value: Option<&str>| {
            let value = value.map(NSString::from_str);
            // SAFETY: the key and the value are valid objects that live through the call.
            unsafe { defaults.setObject_forKey(value.as_deref().map(|value| value as &AnyObject), &NSString::from_str(key)) }
        };
        let flag = |key: &str, value: bool| {
            let number = NSNumber::numberWithBool(value);
            // SAFETY: the key and the value are valid objects that live through the call.
            unsafe { defaults.setObject_forKey(Some(&number), &NSString::from_str(key)) }
        };
        text(DELIVERY, self.delivery.as_ref().map(DeliveryMode::as_str));
        flag(GESTURES_DIRECT, self.gestures_direct);
        flag(ONE_FINGER_SCROLL, self.one_finger_scroll);
        flag(RETURN_FOCUS, self.return_focus);
        flag(THREE_FINGER_DRAG, self.three_finger_drag);
        text(THREE_FINGER_ANCHOR, Some(self.three_finger_anchor.as_str()));
        flag(FOLLOW_TRACKPAD, self.follow_trackpad);
        text(LAUNCHPAD_DOCK, Some(self.launchpad_dock.as_str()));
        flag(KEEP_FOCUS_ON_SPACE_SWITCH, self.keep_focus_on_space_switch);
        flag(SWIPE_BETWEEN_PAGES, self.swipe_between_pages);
    }

    /// The two lines a run opens its log with: the trackpad's settings and whether they are
    /// followed, then the menu's.
    pub fn log_lines(&self, trackpad: &TrackpadPreferences) -> [String; 2] {
        let followed = if self.follow_trackpad { " (followed)" } else { " (not followed)" };
        let anchor = self.three_finger_anchor.as_str();
        let three_finger = if self.three_finger_drag { format!("on ({anchor})") } else { "off".to_owned() };
        [
            format!("trackpad settings: {}{followed}", trackpad.summary()),
            format!(
                "settings: delivery {}, one-finger drag {}, three-finger drag {three_finger}, pinch & rotate direct {}, return focus {}, launchpad {}, keep focus on space switch {}, swipe between pages {}",
                self.delivery.as_ref().map(DeliveryMode::as_str).unwrap_or("automatic"),
                if self.one_finger_scroll { "scrolls" } else { "clicks-and-drags" },
                self.gestures_direct,
                self.return_focus,
                self.launchpad_dock.as_str(),
                self.keep_focus_on_space_switch,
                self.swipe_between_pages,
            ),
        ]
    }

    /// Copies the stored choices into the pipeline's live settings.
    pub fn apply(&self, to: &mut PipelineSettings, trackpad: &TrackpadPreferences) {
        to.policy.forced = self.delivery;
        to.policy.gestures_direct = self.gestures_direct;
        to.one_finger_drag = if self.one_finger_scroll { OneFingerDragMode::Scroll } else { OneFingerDragMode::ClickAndDrag };
        to.focus = if self.return_focus { FocusMode::ReturnToPrevious } else { FocusMode::FollowTouch };
        to.three_finger_drag = self.three_finger_drag;
        to.three_finger_anchor = self.three_finger_anchor;
        to.launchpad_dock = self.launchpad_dock;
        to.keep_focus_on_space_switch = self.keep_focus_on_space_switch;
        to.swipe_between_pages = self.swipe_between_pages;
        to.trackpad = self.follow_trackpad.then_some(*trackpad);
    }
}

/// The trackpad's "Three-finger drag" setting (System Settings → Accessibility → Pointer Control),
/// used until the menu setting has been changed once. On if the key is missing.
fn trackpad_three_finger_drag() -> bool {
    let value = CFPreferencesCopyAppValue(
        &CFString::from_str("TrackpadThreeFingerDrag"),
        &CFString::from_str("com.apple.AppleMultitouchTrackpad"),
    );
    if let Some(value) = value {
        if let Some(flag) = value.downcast_ref::<CFBoolean>() {
            return flag.as_bool();
        }
        if let Some(number) = value.downcast_ref::<CFNumber>() {
            return number.as_i64().is_some_and(|number| number != 0);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line the log opens with: every menu setting, in this order, with the trackpad's line in
    /// front saying whether they are followed.
    #[test]
    fn the_log_opens_with_the_trackpad_s_settings_and_the_menu_s() {
        let followed = StoredSettings {
            delivery: None,
            gestures_direct: true,
            one_finger_scroll: true,
            return_focus: false,
            three_finger_drag: true,
            three_finger_anchor: ThreeFingerAnchor::Centroid,
            follow_trackpad: true,
            launchpad_dock: LaunchpadDockMode::BringDockAndReturn,
            keep_focus_on_space_switch: true,
            swipe_between_pages: true,
        };
        let mut changed = followed.clone();
        changed.delivery = Some(DeliveryMode::Direct);
        changed.one_finger_scroll = false;
        changed.follow_trackpad = false;
        changed.three_finger_drag = false;
        changed.three_finger_anchor = ThreeFingerAnchor::FirstFinger;
        changed.launchpad_dock = LaunchpadDockMode::Leave;

        let trackpad = TrackpadPreferences::default();
        let [line, menu] = followed.log_lines(&trackpad);
        assert_eq!(line, format!("trackpad settings: {} (followed)", trackpad.summary()));
        assert_eq!(
            menu,
            "settings: delivery automatic, one-finger drag scrolls, three-finger drag on (centroid), pinch & rotate direct true, return focus false, launchpad bringDockAndReturn, keep focus on space switch true, swipe between pages true"
        );
        let [line, menu] = changed.log_lines(&trackpad);
        assert_eq!(line, format!("trackpad settings: {} (not followed)", trackpad.summary()));
        assert_eq!(
            menu,
            "settings: delivery direct, one-finger drag clicks-and-drags, three-finger drag off, pinch & rotate direct true, return focus false, launchpad leave, keep focus on space switch true, swipe between pages true"
        );
    }
}
