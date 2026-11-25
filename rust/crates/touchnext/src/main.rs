//! TouchNext, the driver. A menu-bar app that turns a USB touch panel into trackpad-style input on
//! macOS.
//!
//! An argument naming a diagnostic runs it and exits; anything else starts the menu-bar app. The
//! diagnostics and the log file exist only in a build with the `debug` feature.

#[cfg(feature = "debug")]
mod event_sink;
mod log;
mod login_item;
mod menu_bar;
#[cfg(feature = "debug")]
mod probe;
#[cfg(feature = "debug")]
mod replay;
mod settings;
mod single_instance;

use std::path::PathBuf;

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;

/// The arguments that name a diagnostic.
const PROBE: &str = "--probe";
const SYSTEM_UI_SELFTEST: &str = "--system-ui-selftest";
const DOCK_SWIPE_SELFTEST: &str = "--dockswipe-selftest";
const RESTART_SELFTEST: &str = "--restart-selftest";
const AX_PROBE: &str = "--ax-probe";
const DIRECT_SELFTEST: &str = "--direct-selftest";
const REPLAY: &str = "--replay";
const EVENT_SINK: &str = "--event-sink";
const DIAGNOSTICS: [&str; 8] =
    [PROBE, SYSTEM_UI_SELFTEST, DOCK_SWIPE_SELFTEST, RESTART_SELFTEST, AX_PROBE, DIRECT_SELFTEST, REPLAY, EVENT_SINK];

/// The user's home directory, where the lock file and the launch agent live.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Where the logs live. A build with the `debug` feature writes them, and the launch agent puts the
/// app's output there.
fn logs_directory() -> PathBuf {
    home_dir().join("Library/Logs/TouchNext")
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if let Some(flag) = arguments.iter().find(|argument| DIAGNOSTICS.contains(&argument.as_str())) {
        diagnostic(flag, &arguments);
    }

    // The signals are blocked before the pipeline's thread starts, so that it inherits the mask.
    menu_bar::watch_termination_signals();
    let mtm = MainThreadMarker::new().expect("TouchNext starts on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let delegate = menu_bar::delegate(mtm);
    app.setDelegate(Some(&delegate));
    app.run();
}

/// Runs the diagnostic `flag` names and exits. The Event Sink is the one that stays until its
/// window is closed.
#[cfg(feature = "debug")]
fn diagnostic(flag: &str, arguments: &[String]) {
    use std::sync::Arc;
    use std::time::Duration;

    use touchhid::{HidTouchSource, PanelEvent};
    use touchplatform::ax_probe::AXProbe;
    use touchplatform::direct_self_test::DirectSelfTest;
    use touchplatform::dock_swipe_self_test::DockSwipeSelfTest;
    use touchplatform::system_ui_self_test::SystemUISelfTest;

    use crate::log::{log, FileLog};

    // The argument after `flag`, when there is one.
    let value_after = |flag: &str| {
        arguments.iter().position(|argument| argument == flag).and_then(|index| arguments.get(index + 1))
    };
    match flag {
        PROBE => {
            let seconds = value_after(PROBE).and_then(|value| value.parse::<f64>().ok()).unwrap_or(90.0);
            probe::run_probe(seconds);
        }
        SYSTEM_UI_SELFTEST => {
            let log = FileLog::new("systemui");
            SystemUISelfTest::run(&mut |line| log!(log, "{line}"));
        }
        DOCK_SWIPE_SELFTEST => {
            let log = FileLog::new("selftest");
            DockSwipeSelfTest::run(&mut |line| log!(log, "{line}"));
        }
        RESTART_SELFTEST => {
            // Reproduces "Disable → Enable": start, stop, start the HID source again in one process.
            let log = Arc::new(FileLog::new("selftest"));
            let start = |source: &mut HidTouchSource| {
                let log = Arc::clone(&log);
                source.start(move |event| {
                    if let PanelEvent::Status(status) = event {
                        log!(log, "hid: {status}");
                    }
                });
            };
            let mut source = HidTouchSource::new();
            start(&mut source);
            std::thread::sleep(Duration::from_millis(500));
            source.stop();
            log!(log, "stopped; starting again");
            start(&mut source);
            std::thread::sleep(Duration::from_millis(500));
            source.stop();
            log!(log, "restart survived");
        }
        AX_PROBE => {
            let log = FileLog::new("axprobe");
            let mtm = MainThreadMarker::new().expect("the main thread");
            AXProbe::run(mtm, &mut |line| log!(log, "{line}"));
        }
        DIRECT_SELFTEST => {
            let log = FileLog::new("selftest");
            DirectSelfTest::run(&mut |line| log!(log, "{line}"));
        }
        REPLAY => {
            let Some(recording) = value_after(REPLAY) else {
                eprintln!("usage: TouchNext --replay <recording.jsonl> [--frames]");
                std::process::exit(2);
            };
            let frames = arguments.iter().any(|argument| argument == "--frames");
            if let Err(error) = replay::run(recording, frames) {
                eprintln!("TouchNext: {error}");
                std::process::exit(1);
            }
        }
        EVENT_SINK => {
            let mtm = MainThreadMarker::new().expect("TouchNext starts on the main thread");
            let app = NSApplication::sharedApplication(mtm);
            app.setDelegate(Some(&event_sink::delegate(mtm, Arc::new(FileLog::new("eventsink")))));
            app.run();
        }
        // The caller passes one of `DIAGNOSTICS`.
        _ => unreachable!("not a diagnostic: {flag}"),
    }
    std::process::exit(0);
}

/// A build without the `debug` feature has no diagnostics.
#[cfg(not(feature = "debug"))]
fn diagnostic(flag: &str, _arguments: &[String]) {
    eprintln!("TouchNext: {flag} is a diagnostic, and this build has none. Build it with ./scripts/bundle.sh --debug.");
    std::process::exit(2);
}
