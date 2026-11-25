//! Running applications, found the way every module needs them: by bundle identifier or by pid.

use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;

/// The first running instance of the app whose bundle identifier is `bundle_id`.
pub fn with_bundle_id(bundle_id: &str) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id)).firstObject()
}

/// Every running app with this bundle identifier except this process: for a diagnostic, the
/// installed copy.
#[cfg(feature = "debug")]
pub fn others_with_bundle_id(bundle_id: &str) -> Vec<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id))
        .into_iter()
        .filter(|app| app.processIdentifier() != std::process::id() as i32)
        .collect()
}

/// The app that runs as `pid`, while it still runs.
pub fn with_pid(pid: i32) -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
}

/// The app that owns the keyboard and the active menu bar.
pub fn frontmost() -> Option<Retained<NSRunningApplication>> {
    NSWorkspace::sharedWorkspace().frontmostApplication()
}

/// Asks the system to make `app` frontmost. True when the system took the request.
///
/// The Swift app's `activate()` compiled to this same call, `activateWithOptions:` with no options.
/// It is the selector to use: a bare `activate` does not exist on this system, and a message sent
/// to a selector an object does not answer is an exception, which ended the app on 2026-11-25
/// whenever a touch landed in a window of an app that was not frontmost.
pub fn activate(app: &NSRunningApplication) -> bool {
    app.activateWithOptions(NSApplicationActivationOptions::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call has to be one the system answers. The port once sent a bare `activate`, which this
    /// macOS does not have: the exception ended the app whenever a touch landed in a window of an
    /// app that was not frontmost. The frontmost app is asked here, which changes nothing on
    /// screen because it is active already.
    #[test]
    fn activating_asks_the_system_through_a_selector_it_has() {
        let Some(frontmost) = frontmost() else { return };
        let _ = activate(&frontmost);
    }
}
