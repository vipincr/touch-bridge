//! TouchNext, the driver. A menu-bar app that turns a USB touch panel into trackpad-style input on
//! macOS.
//!
//! The modules below are in place; the application itself (the menu bar, the settings, the probe and
//! the event sink) is ported from Swift in the commits that follow, and `main` with it.

#[allow(dead_code)]
mod log;
#[allow(dead_code)]
mod login_item;
#[allow(dead_code)]
mod single_instance;

/// The user's home directory, where the log, the lock file and the launch agent live.
#[allow(dead_code)]
fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("/"))
}

fn main() {}
