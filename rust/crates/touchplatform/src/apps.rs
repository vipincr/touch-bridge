//! Running applications, found the way every module needs them: by bundle identifier or by pid.

use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

/// The first running instance of the app whose bundle identifier is `bundle_id`.
pub fn with_bundle_id(bundle_id: &str) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id)).firstObject()
}

/// The app that runs as `pid`, while it still runs.
pub fn with_pid(pid: i32) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
}

/// The app that owns the keyboard and the active menu bar.
pub fn frontmost() -> Option<Retained<NSRunningApplication>> {
    NSWorkspace::sharedWorkspace().frontmostApplication()
}
