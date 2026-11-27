//! Touch-panel HID access.
//!
//! The backend finds the panel, seizes its HID interfaces (so the host's own cursor emulation
//! stops), performs the panel's mode unlock where one is needed, and delivers raw reports and
//! status lines to the sink its consumer hands it. The rest of the driver reaches the panel through
//! this crate alone.

/// The clock the driver runs on: nanoseconds, monotonic, the same clock IOKit stamps reports with.
pub mod clock;

/// One input report. `timestamp_ns` is the time IOKit stamped it with, in nanoseconds of
/// [`clock`] (the mach timebase applied to the mach absolute time of the callback). `bytes` is the
/// report including the report ID at index 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawReport {
    pub timestamp_ns: u64,
    pub bytes: Vec<u8>,
}

/// One event from the panel source. `Status` carries the exact texts the app's log and its readers
/// depend on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelEvent {
    Report(RawReport),
    Status(String),
}

/// Where the source delivers its events.
///
/// It is called on the source's own dispatch queue, so it must only push the event onward (into a
/// channel, a file) and never call back into the source: a consumer blocked in one of the source's
/// synchronous calls would wait for the queue for ever.
pub type PanelSink = Box<dyn Fn(PanelEvent) + Send>;

pub mod macos;

pub use macos::HidTouchSource;
