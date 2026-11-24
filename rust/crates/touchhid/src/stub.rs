//! Placeholder backend for hosts whose stacks handle HID multitouch natively.
//! The Linux (hidraw) and Windows (HID) backends land with their daemons.

use std::sync::mpsc::{channel, Receiver};

use super::RawReport;

pub struct Driver;

impl Driver {
    pub fn start(_vendor_id: u16, _product_id: u16) -> Result<(Driver, Receiver<RawReport>), String> {
        Err(String::from("no touchhid backend for this host yet"))
    }
}
