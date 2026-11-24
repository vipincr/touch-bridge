//! touchnext: the touch-panel driver daemon.
//!
//! Seizes the panel's HID interfaces, performs the digitizer-mode unlock, and
//! either probes the raw stream (`--probe <seconds>`, the default) or forwards
//! every finger as TUIO 1.1 over UDP (`--tuio`, to 127.0.0.1:3333 by default).
//! On macOS this daemon coexists with the Swift TouchBridge app: only one of
//! them can hold the panel, so quit the app before running the daemon.
//!
//! The loop blocks on the report channel, so an untouched panel costs nothing;
//! TUIO's idle keepalive is the only timer.

use std::collections::BTreeMap;
use std::net::UdpSocket;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use touchcore::filter::SingleTouchFilter;
use touchcore::mode::{Action, PanelModeWatch};
use touchcore::report::{parse, usb, Source};
use touchcore::tracker::{ContactEvent, ContactTracker, Phase};
use touchcore::tuio::{self, PanelPoint};
use touchhid::Driver;

/// JN-MD-IPST105FP active area (10.5-inch 3:2, 1920×1280), for mm thresholds.
const PANEL_WIDTH_MM: f64 = 221.9;
const PANEL_HEIGHT_MM: f64 = 147.9;
const LIFT_TIMEOUT_NS: u64 = 100_000_000;
const TUIO_IDLE: Duration = Duration::from_secs(1);

fn main() {
    let mut mode = String::from("probe");
    let mut seconds = 20u64;
    let mut address = String::from("127.0.0.1:3333");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tuio" => mode = String::from("tuio"),
            "--probe" => {
                mode = String::from("probe");
                if let Some(next) = args.next() {
                    seconds = next.parse().unwrap_or(20);
                }
            }
            "--address" => {
                if let Some(next) = args.next() {
                    address = next;
                }
            }
            _ => {}
        }
    }

    eprintln!("touchnext: seizing the WCH panel (VID 0x{:04X} PID 0x{:04X})", usb::WCH_VID, usb::WCH_PID);
    let (_driver, reports) = match Driver::start(usb::WCH_VID, usb::WCH_PID) {
        Ok(started) => started,
        Err(error) => {
            eprintln!("touchnext: {error} (is another driver, or the TouchBridge app, holding the panel?)");
            std::process::exit(1);
        }
    };

    let tuio_socket = (mode == "tuio").then(|| UdpSocket::bind("0.0.0.0:0").unwrap());
    let started = Instant::now();
    let mut filter = SingleTouchFilter::new(25.0);
    let mut tracker = ContactTracker::new(LIFT_TIMEOUT_NS);
    let mut mode_watch = PanelModeWatch::new();
    let mut sessions = tuio::Sessions::new();
    let mut events: Vec<ContactEvent> = Vec::new();
    let mut contacts: BTreeMap<u8, PanelPoint> = BTreeMap::new();
    let mut wire = Vec::new();
    let mut reports_seen = 0u64;
    let mut digitizer_reports = 0u64;
    let mut mouse_reports = 0u64;
    let mut max_contacts = 0usize;
    let mut sequence = 0i32;

    let deadline = (mode == "probe").then(|| started + Duration::from_secs(seconds));
    let mut heartbeat = started;
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        // Never block past the probe deadline, and heartbeat so "no fingers"
        // reads differently from "no data".
        let timeout = match deadline {
            Some(d) => Some(d.saturating_duration_since(Instant::now())),
            None if mode == "tuio" && contacts.is_empty() => Some(TUIO_IDLE),
            None => Some(Duration::from_secs(60)),
        }
        .filter(|t| !t.is_zero())
        .unwrap_or(Duration::from_millis(1));
        let raw = match reports.recv_timeout(timeout) {
            Ok(raw) => Some(raw),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if mode == "probe" && heartbeat.elapsed() >= Duration::from_secs(5) {
            heartbeat = Instant::now();
            eprintln!("touchnext: {:#} s in — {reports_seen} reports so far", heartbeat.duration_since(started).as_secs_f32());
        }
        let now_ns = started.elapsed().as_nanos() as u64;
        if let Some(raw) = raw {
            reports_seen += 1;
            if let Some(frame) = parse(&raw.bytes[..raw.len]) {
                match frame.source {
                    Source::Digitizer => digitizer_reports += 1,
                    Source::Mouse => {
                        mouse_reports += 1;
                        // A WCH panel streams single touch on its mouse interface until the
                        // unlock lands; that is its normal input then, not a fault.
                        mode_watch.mouse_mode_is_normal = true;
                    }
                }
                if let Some(Action::MultitouchRestored) = mode_watch.observe(frame.source, now_ns) {
                    mode_watch.mouse_mode_is_normal = false;
                    eprintln!("touchnext: panel sends multitouch reports");
                }
                let (first, second) = filter.filter(frame, PANEL_WIDTH_MM, PANEL_HEIGHT_MM);
                for frame in [first].into_iter().chain(second) {
                    tracker.update_into(&frame, now_ns, &mut events);
                    apply(&mut contacts, &events);
                    max_contacts = max_contacts.max(contacts.len());
                    if mode == "probe" && !events.is_empty() {
                        let fingers: Vec<String> = contacts.values().map(|p| format!("({:.2}, {:.2})", p.x, p.y)).collect();
                        eprintln!("touchnext: {} finger(s): {}", fingers.len(), fingers.join(" "));
                    }
                }
            }
        }
        tracker.expire_into(now_ns, &mut events);
        apply(&mut contacts, &events);
        if mode == "tuio" {
            let cursors = sessions.update(&contacts, now_ns);
            sequence = sequence.wrapping_add(1).max(1);
            tuio::frame_into(&cursors, sequence, "touchnext", &mut wire);
            if let Some(socket) = &tuio_socket {
                let _ = socket.send_to(&wire, &address);
            }
        }
    }

    if mode == "probe" {
        eprintln!(
            "touchnext: probe done in {} s — {} reports ({} digitizer, {} mouse), max {} finger(s) at once",
            started.elapsed().as_secs(),
            reports_seen,
            digitizer_reports,
            mouse_reports,
            max_contacts
        );
    }
}

fn apply(contacts: &mut BTreeMap<u8, PanelPoint>, events: &[ContactEvent]) {
    for event in events {
        match event.phase {
            Phase::Ended => {
                contacts.remove(&event.id);
            }
            Phase::Began | Phase::Moved => {
                contacts.insert(event.id, PanelPoint { x: event.x, y: event.y });
            }
        }
    }
}
