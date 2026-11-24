//! Turns successive frames into began/moved/ended events per contact id.
//! A contact ends when its tip lifts, when it disappears from a frame, or when
//! no frame arrives for `lift_timeout_ns` (call [`ContactTracker::expire_into`]
//! from a timer to catch a lost lift report). Events are written into a
//! caller-owned buffer, so steady-state tracking allocates nothing.

use std::collections::BTreeMap;

use crate::report::TouchFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Began,
    Moved,
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContactEvent {
    pub id: u8,
    pub phase: Phase,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Default)]
pub struct ContactTracker {
    active: BTreeMap<u8, (f64, f64)>,
    last_frame_ns: u64,
    lift_timeout_ns: u64,
}

impl ContactTracker {
    pub fn new(lift_timeout_ns: u64) -> Self {
        Self { active: BTreeMap::new(), last_frame_ns: 0, lift_timeout_ns }
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    pub fn update_into(&mut self, frame: &TouchFrame, now_ns: u64, out: &mut Vec<ContactEvent>) {
        out.clear();
        self.last_frame_ns = now_ns;
        let touching: Vec<u8> = frame.contacts.iter().filter(|c| c.touching).map(|c| c.id).collect();

        for contact in frame.contacts.iter().filter(|c| !c.touching) {
            if self.active.remove(&contact.id).is_some() {
                out.push(ContactEvent { id: contact.id, phase: Phase::Ended, x: contact.x, y: contact.y });
            }
        }
        let mut vanished = Vec::new();
        for (&id, &(x, y)) in &self.active {
            if !touching.contains(&id) {
                out.push(ContactEvent { id, phase: Phase::Ended, x, y });
                vanished.push(id);
            }
        }
        for id in vanished {
            self.active.remove(&id);
        }

        for contact in frame.contacts.iter().filter(|c| c.touching) {
            match self.active.get(&contact.id) {
                Some(&previous) if previous != (contact.x, contact.y) => {
                    out.push(ContactEvent { id: contact.id, phase: Phase::Moved, x: contact.x, y: contact.y });
                }
                None => {
                    out.push(ContactEvent { id: contact.id, phase: Phase::Began, x: contact.x, y: contact.y });
                }
                _ => {}
            }
            self.active.insert(contact.id, (contact.x, contact.y));
        }
    }

    pub fn expire_into(&mut self, now_ns: u64, out: &mut Vec<ContactEvent>) {
        out.clear();
        if self.active.is_empty() || now_ns <= self.last_frame_ns || now_ns - self.last_frame_ns <= self.lift_timeout_ns {
            return;
        }
        for (&id, &(x, y)) in &self.active {
            out.push(ContactEvent { id, phase: Phase::Ended, x, y });
        }
        self.active.clear();
    }
}
