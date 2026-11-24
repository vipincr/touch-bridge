//! Touch-panel HID access.
//!
//! One backend per host, selected at compile time. The backend seizes the
//! panel's HID interfaces (so the host's own cursor emulation stops), performs
//! the panel's mode unlock where one is needed, and delivers raw input reports
//! to a channel. Everything above this crate is portable.

/// One raw input report (report ID at index 0) and its length.
#[derive(Debug, Clone, Copy)]
pub struct RawReport {
    pub bytes: [u8; 128],
    pub len: usize,
}

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use macos::Driver;

/// Placeholder until the hidraw (Linux) and WinHID (Windows) backends land;
/// both platforms' stacks handle HID multitouch natively, so their backends
/// exist for the TUIO/driver use cases, not for basic touch.
#[cfg(not(target_os = "macos"))]
pub mod stub;

#[cfg(not(target_os = "macos"))]
pub use stub::Driver;
