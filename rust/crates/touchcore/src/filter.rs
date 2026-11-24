//! A single-contact stream reports one contact. When a second finger lands,
//! that contact teleports between the fingers frame to frame, and a flapping
//! controller can jump coordinates on its own. A jump this large is not a
//! finger moving: split it into a lift and a fresh touch, so the gesture under
//! way ends cleanly instead of dragging across the screen.

use crate::report::{Contact, Contacts, Source, TouchFrame};

#[derive(Debug)]
pub struct SingleTouchFilter {
    pub max_jump_mm: f64,
    last: Option<Contact>,
}

impl SingleTouchFilter {
    pub fn new(max_jump_mm: f64) -> Self {
        Self { max_jump_mm, last: None }
    }

    /// The frames to feed the tracker instead of `frame`: either the frame
    /// itself, or a lift followed by the frame when the contact teleported.
    /// Digitizer frames always pass through: multiple contacts are real, and
    /// contact ids already track lifecycles.
    pub fn filter(&mut self, frame: TouchFrame, panel_width_mm: f64, panel_height_mm: f64) -> (TouchFrame, Option<TouchFrame>) {
        if frame.source != Source::Mouse {
            return (frame, None);
        }
        let Some(contact) = frame.contacts.first() else {
            self.last = None;
            return (frame, None);
        };
        let previous = self.last;
        self.last = Some(contact);
        let Some(previous) = previous else {
            return (frame, None);
        };
        if !previous.touching || !contact.touching {
            return (frame, None);
        }
        let jump_mm =
            ((contact.x - previous.x) * panel_width_mm).hypot((contact.y - previous.y) * panel_height_mm);
        if jump_mm <= self.max_jump_mm {
            return (frame, None);
        }
        let lift = TouchFrame {
            source: Source::Mouse,
            contacts: Contacts::from_iter([Contact::new(previous.id, false, previous.x, previous.y)]),
            scan_time: frame.scan_time,
        };
        (lift, Some(frame))
    }
}
