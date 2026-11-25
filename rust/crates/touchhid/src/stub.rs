//! Placeholder backend for hosts whose stacks handle HID multitouch natively. The Linux (hidraw) and
//! Windows (HID) backends land with their daemons.

use crate::{PanelEvent, PanelSink};

/// The non-macOS source: it never matches a panel, so its sink only hears the two lifecycle lines,
/// and every query answers "nothing attached".
pub struct HidTouchSource {
    sink: Option<PanelSink>,
}

impl Default for HidTouchSource {
    fn default() -> Self {
        Self::new()
    }
}

impl HidTouchSource {
    /// Creates the placeholder source. Nothing happens until `start`.
    pub fn new() -> Self {
        Self { sink: None }
    }

    /// Always matches nothing: the sink hears that matching started and nothing else.
    pub fn start(&mut self, sink: impl Fn(PanelEvent) + Send + 'static) {
        if self.sink.is_some() {
            return;
        }
        let mut sink: PanelSink = Box::new(sink);
        sink(PanelEvent::Status(
            "matching started (this host has no HID backend yet)".to_owned(),
        ));
        self.sink = Some(sink);
    }

    /// Reports the panel released, as the macOS backend does.
    pub fn stop(&mut self) {
        if let Some(mut sink) = self.sink.take() {
            sink(PanelEvent::Status("stopped; panel released".to_owned()));
        }
    }

    /// No panel is ever attached here.
    pub fn has_matched_device(&self) -> bool {
        false
    }

    /// No device to read from.
    pub fn read_feature_report(&self, _id: u8, _length: usize) -> Option<Vec<u8>> {
        None
    }

    /// No device to configure.
    pub fn request_multitouch_mode(&self) {}
}
