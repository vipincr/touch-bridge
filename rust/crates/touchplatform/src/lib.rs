//! macOS input and output around the gesture core: the touch panel over HID, displays, windows,
//! Accessibility, and the events the driver posts.
//!
//! State machines here follow the core's time model. Nothing reads the clock or starts a timer: time
//! comes in as `now_ns` nanoseconds, and whatever is pending is exposed as a deadline for the caller.
//! The one exception is the HID source, whose callbacks IOKit delivers on a dispatch queue.
//!
//! Apple frameworks are reached through the objc2 bindings. Screen geometry is the core's
//! `ScreenPoint` and `ScreenRect` at this crate's boundaries, converted in [`geometry`]; the
//! Accessibility API's out-parameter calls are wrapped once, in [`accessibility`].

pub mod accessibility;
mod apps;
pub mod ax_probe;
pub mod cursor;
pub mod deliverer;
pub mod direct;
pub mod display;
pub mod dock;
pub mod dock_swipe;
pub mod dock_swipe_self_test;
pub mod direct_self_test;
pub mod event;
pub mod geometry;
pub mod hid_bridge;
pub mod journal;
pub mod menu_bar_probe;
pub mod notification;
pub mod pipeline;
pub mod prefs_reader;
mod runtime_symbols;
pub mod system_displays;
pub mod system_ui_self_test;
pub mod tuio_sender;
pub mod window;
