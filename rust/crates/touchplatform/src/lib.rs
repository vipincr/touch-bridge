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
//!
//! The diagnostics (the self-tests and the probes) exist only in a build with the `debug` feature.

/// Where the log lines of a type go. A build with the log file sets one; the types that have one
/// build a line only then.
pub type LogSink = Box<dyn Fn(&str) + Send>;

pub mod accessibility;
mod apps;
#[cfg(feature = "debug")]
pub mod ax_probe;
pub mod cursor;
pub mod deliverer;
pub mod direct;
#[cfg(feature = "debug")]
pub mod direct_self_test;
pub mod display;
pub mod dock;
pub mod dock_swipe;
#[cfg(feature = "debug")]
pub mod dock_swipe_self_test;
pub mod event;
pub mod geometry;
pub mod hid_bridge;
pub mod journal;
#[cfg(feature = "debug")]
pub mod menu_bar_probe;
pub mod notification;
pub mod pipeline;
pub mod prefs_reader;
pub mod runtime_symbols;
pub mod swift_text;
pub mod system_displays;
#[cfg(feature = "debug")]
pub mod system_ui_self_test;
pub mod tuio_sender;
pub mod window;
