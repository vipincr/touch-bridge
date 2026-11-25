//! Sends TUIO frames over UDP to apps on this Mac.
//!
//! 127.0.0.1, port 3333, TUIO's default. Nothing listening? The datagrams are simply dropped, and
//! a socket that could not be opened leaves the sender inert.

use std::net::{Ipv4Addr, UdpSocket};

/// Sends TUIO frames to one UDP port on the loopback interface.
#[derive(Debug)]
pub struct TUIOSender {
    /// Bound and connected; `None` when the system refused to open a socket.
    socket: Option<UdpSocket>,
}

impl TUIOSender {
    /// TUIO's default port.
    pub const PORT: u16 = 3333;

    /// A sender to `port` on 127.0.0.1. The socket binds any local port; when it cannot be opened,
    /// [`TUIOSender::send`] does nothing.
    pub fn new(port: u16) -> Self {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .and_then(|socket| socket.connect((Ipv4Addr::LOCALHOST, port)).map(|_| socket))
            .ok();
        Self { socket }
    }

    /// Sends one datagram. A datagram nobody receives is not an error.
    pub fn send(&self, bytes: &[u8]) {
        if let Some(socket) = &self.socket {
            let _ = socket.send(bytes);
        }
    }
}
