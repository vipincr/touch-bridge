//! Delivery policy: whether a gesture reaches an app posted straight to it
//! (the cursor never moves) or by briefly moving the hidden cursor to the
//! finger and posting through the window server. One decision per gesture.

use std::str::FromStr;

use crate::mapper::{ScreenPoint, ScreenRect};

/// How a gesture reaches an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryMode {
    /// Posted straight to the app's window; the cursor never moves.
    Direct,
    /// The hidden cursor is moved to the finger and the gesture posted through the window server.
    Warp,
}

impl DeliveryMode {
    /// The Swift raw value, under which settings store the mode.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Warp => "warp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseDeliveryModeError;

impl FromStr for DeliveryMode {
    type Err = ParseDeliveryModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "direct" => Ok(Self::Direct),
            "warp" => Ok(Self::Warp),
            _ => Err(ParseDeliveryModeError),
        }
    }
}

/// Which gesture is being delivered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureKind {
    Tap,
    RightClick,
    /// A drag; `hold` is true when it started with the press-and-hold that
    /// means drag-and-drop or drawing, which always goes through the window server.
    Drag { hold: bool },
    Scroll,
    Magnify,
    Rotate,
}

/// What happens to keyboard focus after a touch activated an app (menu switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusMode {
    /// The touched app stays active, like a normal click.
    FollowTouch,
    /// The previously active app is re-activated shortly after the touch.
    ReturnToPrevious,
}

impl FocusMode {
    /// The Swift raw value, under which settings store the mode.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::FollowTouch => "followTouch",
            Self::ReturnToPrevious => "returnToPrevious",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseFocusModeError;

impl FromStr for FocusMode {
    type Err = ParseFocusModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "followTouch" => Ok(Self::FollowTouch),
            "returnToPrevious" => Ok(Self::ReturnToPrevious),
            _ => Err(ParseFocusModeError),
        }
    }
}

/// An on-screen window as reported by CGWindowListCopyWindowInfo (bounds in global points).
#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub number: i64,
    pub pid: i32,
    pub owner_name: String,
    pub layer: i64,
    pub bounds: ScreenRect,
    pub alpha: f64,
}

impl WindowInfo {
    fn contains(&self, p: ScreenPoint) -> bool {
        p.x >= self.bounds.x
            && p.x < self.bounds.x + self.bounds.width
            && p.y >= self.bounds.y
            && p.y < self.bounds.y + self.bounds.height
    }
}

/// Window picking for delivery.
pub struct WindowHitTester;

impl WindowHitTester {
    /// The CGWindowLayer of popup menus, whose being open blocks Direct delivery.
    pub const POPUP_MENU_LAYER: i64 = 101;

    /// The frontmost window under the point. `windows` must be front-to-back,
    /// as CGWindowListCopyWindowInfo returns it; the driver's own windows are
    /// excluded by pid, as are invisible ones (alpha 0).
    pub fn top_window(point: ScreenPoint, windows: &[WindowInfo], excluding_pid: i32) -> Option<&WindowInfo> {
        windows
            .iter()
            .find(|w| w.pid != excluding_pid && w.alpha > 0.0 && w.contains(point))
    }

    /// Whether any window in the list is a popup menu.
    pub fn popup_menu_visible(windows: &[WindowInfo]) -> bool {
        windows.iter().any(|w| w.layer == Self::POPUP_MENU_LAYER)
    }
}

/// Everything the policy weighs, gathered by the platform before the gesture is delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryContext {
    pub kind: GestureKind,
    pub window: Option<WindowInfo>,
    pub point: ScreenPoint,
    pub popup_menu_visible: bool,
    pub app_is_chromium: bool,
    pub app_override: Option<DeliveryMode>,
    /// Whether this drag moves the window (decided by WindowDragRules from Accessibility).
    pub starts_window_drag: bool,
}

impl DeliveryContext {
    /// Swift's parameters, in Swift's order (`startsWindowDrag` defaults to false there).
    pub fn new(
        kind: GestureKind,
        window: Option<WindowInfo>,
        point: ScreenPoint,
        popup_menu_visible: bool,
        app_is_chromium: bool,
        app_override: Option<DeliveryMode>,
        starts_window_drag: bool,
    ) -> Self {
        Self { kind, window, point, popup_menu_visible, app_is_chromium, app_override, starts_window_drag }
    }
}

/// Picks Direct or Warp once per gesture (spec: "Delivery policy").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryPolicy {
    /// Menu switch for compatibility testing: forces one mode for everything.
    pub forced: Option<DeliveryMode>,
    /// Whether pinch and rotate may go Direct (decided by the M1 compatibility matrix).
    pub gestures_direct: bool,
}

impl Default for DeliveryPolicy {
    fn default() -> Self {
        Self { forced: None, gestures_direct: true }
    }
}

impl DeliveryPolicy {
    /// Swift's defaults: nothing forced, pinch and rotate allowed Direct.
    pub fn new() -> Self {
        Self::default()
    }

    /// Owner apps whose windows are system UI, never Direct targets.
    const SYSTEM_OWNERS: [&str; 9] = [
        "Dock",
        "SystemUIServer",
        "Control Center",
        "ControlCenter",
        "Notification Center",
        "NotificationCenter",
        "Spotlight",
        "Window Server",
        "WindowServer",
    ];
    /// The CGWindowLayers Direct delivery targets: normal and floating windows.
    const DIRECT_LAYERS: [i64; 2] = [0, 3];

    /// Whether the target app must be active before the gesture is delivered.
    /// Measured in the Event Sink: on an inactive app, Direct clicks are
    /// swallowed (first-click rule) and pinches dropped, while right-clicks and
    /// scrolls arrive.
    pub fn needs_activation(kind: GestureKind) -> bool {
        !matches!(kind, GestureKind::Scroll | GestureKind::RightClick)
    }

    /// The mode this gesture is delivered in.
    pub fn decide(&self, c: &DeliveryContext) -> DeliveryMode {
        if let Some(forced) = self.forced {
            return forced;
        }
        if c.kind == (GestureKind::Drag { hold: true }) {
            return DeliveryMode::Warp;
        }
        let Some(window) = &c.window else {
            return DeliveryMode::Warp; // Direct needs a window to post to
        };
        if let Some(app_override) = c.app_override {
            return app_override;
        }
        if c.app_is_chromium
            || Self::SYSTEM_OWNERS.contains(&window.owner_name.as_str())
            || !Self::DIRECT_LAYERS.contains(&window.layer)
            || c.popup_menu_visible
        {
            return DeliveryMode::Warp;
        }
        if c.kind == (GestureKind::Drag { hold: false }) && c.starts_window_drag {
            return DeliveryMode::Warp;
        }
        if (c.kind == GestureKind::Magnify || c.kind == GestureKind::Rotate) && !self.gestures_direct {
            return DeliveryMode::Warp;
        }
        DeliveryMode::Direct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window with every field given: the nine are what a `WindowInfo` holds.
    #[allow(clippy::too_many_arguments)]
    fn window(
        number: i64,
        pid: i32,
        owner: &str,
        layer: i64,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        alpha: f64,
    ) -> WindowInfo {
        WindowInfo {
            number,
            pid,
            owner_name: owner.to_string(),
            layer,
            bounds: ScreenRect { x, y, width: w, height: h },
            alpha,
        }
    }

    /// Swift's `window(_:)` helper with its default arguments.
    fn window1(number: i64) -> WindowInfo {
        window(number, 100, "TextEdit", 0, 1800.0, 100.0, 800.0, 600.0, 1.0)
    }

    const INSIDE: ScreenPoint = ScreenPoint { x: 2000.0, y: 400.0 };

    // hit testing (window list is front-to-back, as CGWindowListCopyWindowInfo returns it)

    #[test]
    fn hit_test_returns_the_frontmost_window_under_the_point() {
        let list = [
            window(1, 100, "TextEdit", 0, 0.0, 0.0, 100.0, 100.0, 1.0),
            window1(2),
            window1(3),
        ];
        assert_eq!(WindowHitTester::top_window(INSIDE, &list, 999).map(|w| w.number), Some(2));
    }

    #[test]
    fn hit_test_skips_our_own_and_invisible_windows() {
        let list = [
            window(1, 999, "TextEdit", 0, 1800.0, 100.0, 800.0, 600.0, 1.0),
            window(2, 100, "TextEdit", 0, 1800.0, 100.0, 800.0, 600.0, 0.0),
            window1(3),
        ];
        assert_eq!(WindowHitTester::top_window(INSIDE, &list, 999).map(|w| w.number), Some(3));
    }

    #[test]
    fn hit_test_finds_nothing_over_empty_space() {
        let list = [window1(1)];
        assert_eq!(
            WindowHitTester::top_window(ScreenPoint { x: 5000.0, y: 5000.0 }, &list, 999),
            None
        );
    }

    #[test]
    fn popup_menus_are_detected_by_their_window_layer() {
        let list = [window1(1), window(2, 100, "TextEdit", 101, 1800.0, 100.0, 800.0, 600.0, 1.0)];
        assert!(WindowHitTester::popup_menu_visible(&list));
        assert!(!WindowHitTester::popup_menu_visible(&[window1(1)]));
    }

    // policy

    /// Swift's `context(_:)` helper: a tap on a TextEdit window at `INSIDE`,
    /// adjusted per test like Swift's default arguments adjust it.
    fn context(kind: GestureKind, adjust: impl FnOnce(&mut DeliveryContext)) -> DeliveryContext {
        let mut c = DeliveryContext::new(kind, Some(window1(1)), INSIDE, false, false, None, false);
        adjust(&mut c);
        c
    }

    fn plain(kind: GestureKind) -> DeliveryContext {
        context(kind, |_| {})
    }

    #[test]
    fn ordinary_gestures_on_an_app_window_go_direct() {
        let policy = DeliveryPolicy::new();
        for kind in [
            GestureKind::Tap,
            GestureKind::RightClick,
            GestureKind::Drag { hold: false },
            GestureKind::Scroll,
            GestureKind::Magnify,
            GestureKind::Rotate,
        ] {
            assert_eq!(policy.decide(&plain(kind)), DeliveryMode::Direct);
        }
    }

    #[test]
    fn hold_drag_always_warps_even_with_a_direct_override() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Drag { hold: true }, |c| c.app_override = Some(DeliveryMode::Direct))),
            DeliveryMode::Warp
        );
    }

    #[test]
    fn per_app_override_wins_over_automatic_rules() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.app_override = Some(DeliveryMode::Warp))),
            DeliveryMode::Warp
        );
        assert_eq!(
            DeliveryPolicy::new()
                .decide(&context(GestureKind::Tap, |c| {
                    c.app_is_chromium = true;
                    c.app_override = Some(DeliveryMode::Direct);
                })),
            DeliveryMode::Direct
        );
    }

    #[test]
    fn chromium_based_apps_warp() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.app_is_chromium = true)),
            DeliveryMode::Warp
        );
    }

    #[test]
    fn system_ui_warps() {
        for owner in ["Dock", "SystemUIServer", "Control Center", "Notification Center", "Spotlight", "Window Server"] {
            let c = context(GestureKind::Tap, |c| c.window = Some(window(1, 100, owner, 0, 1800.0, 100.0, 800.0, 600.0, 1.0)));
            assert_eq!(DeliveryPolicy::new().decide(&c), DeliveryMode::Warp, "{owner}");
        }
    }

    #[test]
    fn only_normal_and_floating_layers_go_direct() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.window = Some(window(1, 100, "TextEdit", 3, 1800.0, 100.0, 800.0, 600.0, 1.0)))),
            DeliveryMode::Direct
        );
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.window = Some(window(1, 100, "TextEdit", 25, 1800.0, 100.0, 800.0, 600.0, 1.0)))),
            DeliveryMode::Warp
        );
    }

    #[test]
    fn an_open_popup_menu_or_no_window_warps() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.popup_menu_visible = true)),
            DeliveryMode::Warp
        );
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.window = None)),
            DeliveryMode::Warp
        );
    }

    // Window moves are performed by the window server following the real cursor, so they need Warp.
    // Whether a drag moves the window is decided by WindowDragRules (toolbar, title bar, title text).
    #[test]
    fn drags_that_move_a_window_warp_but_other_drags_go_direct() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Drag { hold: false }, |c| c.starts_window_drag = true)),
            DeliveryMode::Warp
        );
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| c.starts_window_drag = true)),
            DeliveryMode::Direct,
            "a tap on a title bar or toolbar is still a click"
        );
        assert_eq!(DeliveryPolicy::new().decide(&plain(GestureKind::Drag { hold: false })), DeliveryMode::Direct);
        // The old fixed 28 pt band no longer decides on its own.
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Drag { hold: false }, |c| c.point = ScreenPoint { x: 2000.0, y: 110.0 })),
            DeliveryMode::Direct
        );
    }

    #[test]
    fn pinch_and_rotate_follow_the_gestures_direct_setting() {
        let mut policy = DeliveryPolicy::new();
        policy.gestures_direct = false;
        assert_eq!(policy.decide(&plain(GestureKind::Magnify)), DeliveryMode::Warp);
        assert_eq!(policy.decide(&plain(GestureKind::Rotate)), DeliveryMode::Warp);
        assert_eq!(policy.decide(&plain(GestureKind::Scroll)), DeliveryMode::Direct);
    }

    #[test]
    fn a_forced_mode_overrides_everything() {
        let mut policy = DeliveryPolicy::new();
        policy.forced = Some(DeliveryMode::Direct);
        assert_eq!(
            policy.decide(&context(GestureKind::Drag { hold: true }, |c| {
                c.window = None;
                c.popup_menu_visible = true;
            })),
            DeliveryMode::Direct
        );
        policy.forced = Some(DeliveryMode::Warp);
        assert_eq!(policy.decide(&plain(GestureKind::Tap)), DeliveryMode::Warp);
    }

    #[test]
    fn a_direct_override_cannot_apply_without_a_window_to_target() {
        assert_eq!(
            DeliveryPolicy::new().decide(&context(GestureKind::Tap, |c| {
                c.window = None;
                c.app_override = Some(DeliveryMode::Direct);
            })),
            DeliveryMode::Warp
        );
    }

    // Measured in the Event Sink: on an inactive app, Direct clicks are swallowed (first-click rule)
    // and pinches dropped, while right-clicks and scrolls arrive.
    #[test]
    fn gestures_that_need_the_app_active_first() {
        assert!(DeliveryPolicy::needs_activation(GestureKind::Tap));
        assert!(DeliveryPolicy::needs_activation(GestureKind::Drag { hold: false }));
        assert!(DeliveryPolicy::needs_activation(GestureKind::Drag { hold: true }));
        assert!(DeliveryPolicy::needs_activation(GestureKind::Magnify));
        assert!(DeliveryPolicy::needs_activation(GestureKind::Rotate));
        assert!(!DeliveryPolicy::needs_activation(GestureKind::Scroll));
        assert!(!DeliveryPolicy::needs_activation(GestureKind::RightClick));
    }
}
