//! The panel can report either multitouch digitizer frames or single-touch
//! "mouse" frames. An ILITEK panel reverts to mouse frames after losing power
//! until the host reads its feature reports (as Linux's hid-multitouch does),
//! so this decides when to ask, and notices when multitouch returns. A WCH
//! panel streams mouse frames until its Input Mode unlock lands, so
//! `mouse_mode_is_normal` makes those frames ordinary input instead of a fault
//! to recover from.

use crate::report::Source;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Read the panel's feature reports to switch it to multitouch.
    RequestMultitouch,
    /// Multitouch frames arrive again after mouse frames.
    MultitouchRestored,
}

const RETRY_INTERVAL_NS: u64 = 5_000_000_000;

#[derive(Debug)]
pub struct PanelModeWatch {
    /// True when mouse frames are the panel's normal output, not a degraded state.
    pub mouse_mode_is_normal: bool,
    last_request_ns: Option<u64>,
    saw_mouse: bool,
}

impl PanelModeWatch {
    pub fn new() -> Self {
        Self { mouse_mode_is_normal: false, last_request_ns: None, saw_mouse: false }
    }

    pub fn observe(&mut self, source: Source, now_ns: u64) -> Option<Action> {
        match source {
            Source::Mouse => {
                self.saw_mouse = true;
                if self.mouse_mode_is_normal {
                    return None;
                }
                if let Some(last) = self.last_request_ns {
                    if now_ns - last < RETRY_INTERVAL_NS {
                        return None;
                    }
                }
                self.last_request_ns = Some(now_ns);
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
