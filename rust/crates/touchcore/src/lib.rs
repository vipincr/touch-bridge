//! Portable touch-panel core, ported from TouchBridge's TouchCore (Swift).
//!
//! Pure Rust, no OS dependencies: HID report decoders for the WCH and ILITEK
//! controller families, contact lifecycle tracking, the single-touch teleport
//! filter, the panel mode watch, coordinate mapping and TUIO 1.1 framing.
//! Everything above this layer (gesture recognition, event synthesis) is
//! per-platform: the macOS Swift app on Apple hosts, native stacks elsewhere.

pub mod filter;
pub mod mapper;
pub mod mode;
pub mod report;
pub mod tracker;
pub mod tuio;
