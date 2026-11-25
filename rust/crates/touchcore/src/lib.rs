//! The portable half of TouchNext: everything between a panel's HID reports and the gestures they
//! mean. No operating-system calls and no unsafe code. Time is passed in as nanoseconds, so every type
//! here behaves the same on any platform and in any test.
//!
//! The macOS half around it lives in `touchplatform` (displays, windows, posted events) and `touchhid`
//! (the panel itself).

pub mod addressing;
pub mod dock_hold;
pub mod drag_rules;
pub mod filter;
pub mod focus;
pub mod gestures;
pub mod launchpad;
pub mod mapper;
pub mod menubar;
pub mod mode;
pub mod momentum;
pub mod policy;
pub mod prefs;
pub mod probe;
pub mod report;
pub mod traits;
pub mod tracker;
pub mod tuio;
