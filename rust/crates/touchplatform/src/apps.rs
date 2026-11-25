//! Running applications, found the way every module needs them: by bundle identifier or by pid.

use objc2::rc::Retained;
use objc2::msg_send;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

/// The first running instance of the app whose bundle identifier is `bundle_id`.
pub fn with_bundle_id(bundle_id: &str) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id)).firstObject()
}

/// The app that runs as `pid`, while it still runs.
/// Every running app with this bundle identifier except this process: for a diagnostic, the
/// installed copy.
#[cfg(feature = "debug")]
pub fn others_with_bundle_id(bundle_id: &str) -> Vec<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id))
        .into_iter()
        .filter(|app| app.processIdentifier() != std::process::id() as i32)
        .collect()
}

pub fn with_pid(pid: i32) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
}

/// The app that owns the keyboard and the active menu bar.
pub fn frontmost() -> Option<Retained<NSRunningApplication>> {
    NSWorkspace::sharedWorkspace().frontmostApplication()
}

/// Asks the system to make `app` frontmost, the way the trackpad's settings and the Dock do
/// (`activate`, macOS 14, which the binding lacks). True when the system took the request.
pub fn activate(app: &NSRunningApplication) -> bool {
    // SAFETY: `activate` takes no arguments and returns a BOOL; the app's minimum system version,
    // macOS 14, is the one that introduced it.
    unsafe { msg_send![app, activate] }
}
