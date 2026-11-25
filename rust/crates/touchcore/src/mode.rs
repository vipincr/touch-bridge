//! Which of the panel's two modes is streaming, and when to ask for the other.

use crate::report::Source;

/// The panel can report either multitouch digitizer frames or single-touch
/// "mouse" frames. Mouse frames always mean the panel is in single-touch
/// emulation: either the initial mode switch did not take or the panel slipped
/// back. So every mouse frame asks for multitouch again (rate-limited): one
/// feature-report write while the panel streams mouse input costs nothing, and
/// staying in emulation silently costs every gesture. ILITEK panels are
/// switched back by reading feature reports 2 and 6, as Linux's hid-multitouch
/// does.
#[derive(Debug)]
pub struct PanelModeWatch {
    last_request_ns: Option<u64>,
    saw_mouse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Read the panel's feature reports to switch it to multitouch.
    RequestMultitouch,
    /// Multitouch frames arrive again after mouse frames.
    MultitouchRestored,
}

impl PanelModeWatch {
    /// The minimum spacing between two multitouch requests while the panel
    /// streams mouse frames.
    pub const RETRY_INTERVAL_NS: u64 = 2_000_000_000;

    pub fn new() -> Self {
        Self { last_request_ns: None, saw_mouse: false }
    }

    pub fn observe(&mut self, source: Source, at_ns: u64) -> Option<Action> {
        match source {
            Source::Mouse => {
                self.saw_mouse = true;
                if let Some(last) = self.last_request_ns
                    && at_ns - last < Self::RETRY_INTERVAL_NS {
                        return None;
                    }
                self.last_request_ns = Some(at_ns);
                Some(Action::RequestMultitouch)
            }
            Source::Digitizer => {
                if !self.saw_mouse {
                    return None;
                }
                self.saw_mouse = false;
                Some(Action::MultitouchRestored)
            }
        }
    }
}

impl Default for PanelModeWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = 1_000_000_000;

    #[test]
    fn a_mouse_report_asks_the_panel_for_multitouch() {
        let mut watch = PanelModeWatch::default();
        assert_eq!(watch.observe(Source::Mouse, 0), Some(Action::RequestMultitouch));
    }

    #[test]
    fn requests_are_rate_limited() {
        let mut watch = PanelModeWatch::default();
        let _ = watch.observe(Source::Mouse, 0);
        assert_eq!(watch.observe(Source::Mouse, S), None);
        assert_eq!(watch.observe(Source::Mouse, 6 * S), Some(Action::RequestMultitouch));
    }

    #[test]
    fn the_switch_back_to_multitouch_is_reported_once() {
        let mut watch = PanelModeWatch::default();
        let _ = watch.observe(Source::Mouse, 0);
        assert_eq!(watch.observe(Source::Digitizer, S), Some(Action::MultitouchRestored));
        assert_eq!(watch.observe(Source::Digitizer, 2 * S), None);
    }

    #[test]
    fn multitouch_from_the_start_needs_nothing() {
        let mut watch = PanelModeWatch::default();
        assert_eq!(watch.observe(Source::Digitizer, 0), None);
        assert_eq!(watch.observe(Source::Digitizer, S), None);
    }

    #[test]
    fn a_slip_after_restoration_asks_again() {
        let mut watch = PanelModeWatch::default();
        assert_eq!(watch.observe(Source::Mouse, 0), Some(Action::RequestMultitouch));
        assert_eq!(watch.observe(Source::Digitizer, S), Some(Action::MultitouchRestored));
        // The panel slips back mid-session: the next mouse frame asks again
        // without delay.
        assert_eq!(watch.observe(Source::Mouse, 20 * S), Some(Action::RequestMultitouch));
    }

    #[test]
    fn mouse_frames_keep_asking_while_the_panel_stays_in_emulation() {
        let mut watch = PanelModeWatch::default();
        assert_eq!(watch.observe(Source::Mouse, 0), Some(Action::RequestMultitouch));
        assert_eq!(watch.observe(Source::Mouse, 5 * S), Some(Action::RequestMultitouch));
        assert_eq!(watch.observe(Source::Mouse, 10 * S), Some(Action::RequestMultitouch));
    }
}
