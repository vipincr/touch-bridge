//! The M1 probe: seizes the panel for a fixed time, records every raw report as JSON lines, and
//! writes a summary that answers the multitouch gate. Exits on its own, which releases the panel.
//!
//! The recording is the format every analysis script and the replay tests read, so it stays byte
//! for byte: one JSON object per line, `{"t_ns":…,"rid":…,"len":…,"hex":…}`, then `contacts` and
//! `scan` when the report decodes as a frame.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2::MainThreadMarker;
use objc2_application_services::AXIsProcessTrustedWithOptions;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString};
use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestType};
use touchcore::probe::ProbeStats;
use touchcore::report::ReportParser;
use touchhid::{HidTouchSource, PanelEvent};
use touchplatform::display::DisplayLocator;
use touchplatform::swift_text::{swift_f64, swift_rect};


/// Set by SIGINT and SIGTERM; the probe's loop checks it between sleeps.
static STOP: AtomicBool = AtomicBool::new(false);

/// The feature reports the probe reads a second in: report id, expected length.
const FEATURE_REPORTS: [(u8, usize); 2] = [(2, 2), (6, 257)];

/// Seizes the panel for `seconds`, records every raw report, writes the summary, and exits the
/// process (status 0). Never returns.
pub fn run_probe(seconds: f64) -> ! {
    let started = Instant::now();
    let stamp = stamp();
    let recorder = Arc::new(ProbeRecorder::new(crate::logs_directory(), &stamp));
    // The probe runs on the main thread before any NSApplication exists; AppKit's screens answer
    // without an app.
    let mtm = MainThreadMarker::new().expect("the probe runs on the main thread");
    recorder.log(&format!(
        "TouchNext probe, {} s; raw reports → {}",
        seconds as i64,
        recorder.reports_path.display(),
    ));
    check_permissions(&recorder);
    log_displays(mtm, &recorder);

    // SIGINT and SIGTERM only set the stop flag; the loop below does the shutdown.
    let handler = handle_signal as extern "C" fn(libc::c_int);
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: the handler only stores into the static flag and calls nothing else.
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }

    let mut source = HidTouchSource::new();
    let queue_recorder = Arc::clone(&recorder);
    // The sink runs on the HID queue: it only writes files, never calls back into the source.
    source.start(Box::new(move |event| match event {
        PanelEvent::Status(status) => queue_recorder.log(&status),
        PanelEvent::Report(report) => queue_recorder.record(report.timestamp_ns, &report.bytes),
    }));

    let duration = Duration::from_secs_f64(seconds.max(0.0));
    let mut feature_reports: Vec<String> = Vec::new();
    let mut feature_read = false;
    let mut device_checked = false;
    let mut next_summary = started + Duration::from_secs(5);
    loop {
        if STOP.load(Ordering::Relaxed) || started.elapsed() >= duration {
            finish(&mut source, &recorder, &feature_reports, started);
        }
        if !feature_read && started.elapsed() >= Duration::from_secs(1) {
            read_feature_reports(&mut source, &recorder, &mut feature_reports);
            feature_read = true;
        }
        if !device_checked && started.elapsed() >= Duration::from_secs(3) {
            if !source.has_matched_device() {
                recorder.log(
                    "no HID device for the panel — another driver may own it at USB level (e.g. UPDD). Check: ioreg -r -n ILITEK-TOUCH | grep UsbExclusiveOwner",
                );
            }
            device_checked = true;
        }
        if Instant::now() >= next_summary {
            recorder.log(&summary(&recorder, false, started, &feature_reports));
            next_summary += Duration::from_secs(5);
        }
        // Wake when the next timer falls due, but often enough to notice a signal promptly.
        let now = Instant::now();
        let due = [
            (!feature_read).then_some(started + Duration::from_secs(1)),
            (!device_checked).then_some(started + Duration::from_secs(3)),
            Some(next_summary),
            Some(started + duration),
        ]
        .into_iter()
        .flatten()
        .find(|due| *due > now)
        .unwrap_or(now + Duration::from_millis(100));
        std::thread::sleep((due - now).min(Duration::from_millis(100)));
    }
}

extern "C" fn handle_signal(_signal: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// The two permissions, asked for with the system prompts, as the Swift probe logged them.
fn check_permissions(recorder: &ProbeRecorder) {
    // "AXTrustedCheckOptionPrompt" is the value of kAXTrustedCheckOptionPrompt; passing it asks the
    // system to show its prompt when the permission is missing.
    let options = CFDictionary::<CFString, CFBoolean>::from_slices(
        &[&CFString::from_str("AXTrustedCheckOptionPrompt")],
        &[CFBoolean::new(true)],
    );
    // SAFETY: the cast only forgets the dictionary's key and value types.
    let options: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(options) };
    // SAFETY: the dictionary is live for the length of the call.
    let trusted = unsafe { AXIsProcessTrustedWithOptions(Some(&options)) };
    recorder.log(if trusted {
        "Accessibility: granted"
    } else {
        "Accessibility: NOT granted (a prompt was shown; grant it, then run the probe again)"
    });
    let listen = IOHIDCheckAccess(IOHIDRequestType::ListenEvent);
    recorder.log(match listen {
        IOHIDAccessType::Granted => "Input Monitoring: granted",
        IOHIDAccessType::Denied => "Input Monitoring: denied",
        _ => "Input Monitoring: not decided",
    });
}

/// Every online display, as the probe's log opens with.
fn log_displays(mtm: MainThreadMarker, recorder: &ProbeRecorder) {
    let names = DisplayLocator::screen_names(mtm);
    for d in DisplayLocator::online_displays(&names) {
        recorder.log(&format!(
            "display {} \"{}\" vendor={} model={} serial={} bounds={} rotation={}{}",
            d.id,
            d.name,
            d.vendor,
            d.model,
            d.serial,
            swift_rect(d.bounds),
            swift_f64(d.rotation),
            if d.is_main { " main" } else { "" },
        ));
    }
}

/// Reads the two feature reports the panel answers with, once, a second in.
fn read_feature_reports(
    source: &mut HidTouchSource,
    recorder: &ProbeRecorder,
    feature_reports: &mut Vec<String>,
) {
    for (id, length) in FEATURE_REPORTS {
        if let Some(bytes) = source.read_feature_report(id, length) {
            let line = format!("feature RID {id} ({} bytes): {}", bytes.len(), hex(&bytes));
            feature_reports.push(line.clone());
            recorder.log(&line);
        }
    }
}

/// Stops the source, writes the final summary, and exits the process, which releases the panel.
fn finish(
    source: &mut HidTouchSource,
    recorder: &ProbeRecorder,
    feature_reports: &[String],
    started: Instant,
) -> ! {
    source.stop();
    let text = summary(recorder, true, started, feature_reports);
    recorder.log(&text);
    recorder.write_summary(&text);
    std::process::exit(0);
}

/// The statistics, as the Swift probe printed them.
fn summary(
    recorder: &ProbeRecorder,
    final_summary: bool,
    started: Instant,
    feature_reports: &[String],
) -> String {
    let elapsed = started.elapsed().as_secs();
    let mut lines = recorder.with_stats(|stats| {
        let by_id = stats
            .reports_by_id()
            .iter()
            .map(|(id, count)| format!("RID {id}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let interval = stats
            .median_interval_ms()
            .map(|ms| format!("{ms:.2} ms (~{:.0} Hz)", 1000.0 / ms))
            .unwrap_or_else(|| "n/a".to_string());
        let ids = stats
            .distinct_contact_ids()
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        vec![
            format!(
                "{} summary after {} s",
                if final_summary { "FINAL" } else { "progress" },
                elapsed,
            ),
            format!(
                "  reports: {}",
                if by_id.is_empty() {
                    "none yet — touch the screen".to_string()
                } else {
                    by_id
                },
            ),
            format!(
                "  RID 4 max simultaneous contacts: {} → multitouch gate {}",
                stats.max_simultaneous_contacts(),
                if stats.multitouch_gate_passed() { "PASS" } else { "not passed yet" },
            ),
            format!("  distinct contact ids: [{ids}]"),
            format!("  median report interval: {interval}"),
            format!(
                "  median scan-time step: {}",
                stats
                    .median_scan_time_step()
                    .map(|step| step.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
            ),
        ]
    });
    if final_summary {
        lines.extend(feature_reports.iter().map(|line| format!("  {line}")));
    }
    lines.join("\n")
}

/// Owns the probe's files and statistics. Reports are recorded on the HID queue; the statistics
/// are read under the same lock.
struct ProbeRecorder {
    reports_path: PathBuf,
    log_path: PathBuf,
    summary_path: PathBuf,
    reports: Option<File>,
    stats: Mutex<ProbeStats>,
}

impl ProbeRecorder {
    /// Creates the log directory and the report and log files; the summary file is written at the
    /// end, as the Swift probe wrote it.
    fn new(directory: PathBuf, stamp: &str) -> Self {
        let _ = std::fs::create_dir_all(&directory);
        let reports_path = directory.join(format!("probe-{stamp}.jsonl"));
        let log_path = directory.join(format!("probe-{stamp}.log"));
        let summary_path = directory.join(format!("probe-{stamp}-summary.txt"));
        let reports = File::create(&reports_path).ok();
        let _ = File::create(&log_path);
        ProbeRecorder {
            reports_path,
            log_path,
            summary_path,
            reports,
            stats: Mutex::new(ProbeStats::new()),
        }
    }

    /// One raw report: appends the JSON line to the recording and feeds the statistics.
    fn record(&self, timestamp_ns: u64, bytes: &[u8]) {
        let frame = ReportParser::parse(bytes);
        let report_id = bytes.first().copied().unwrap_or(0);
        {
            let mut stats = self.stats.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            stats.record(timestamp_ns, report_id, frame.as_ref());
        }
        let mut line = format!(
            "{{\"t_ns\":{},\"rid\":{},\"len\":{},\"hex\":\"{}\"",
            timestamp_ns,
            report_id,
            bytes.len(),
            hex(bytes),
        );
        if let Some(frame) = &frame {
            let contacts = frame
                .contacts
                .iter()
                .map(|contact| {
                    format!(
                        "{{\"id\":{},\"tip\":{},\"x\":{:.5},\"y\":{:.5}}}",
                        contact.id, contact.touching, contact.x, contact.y,
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            line.push_str(&format!(",\"contacts\":[{contacts}]"));
            if let Some(scan) = frame.scan_time {
                line.push_str(&format!(",\"scan\":{scan}"));
            }
        }
        line.push_str("}\n");
        if let Some(reports) = &self.reports {
            let mut reports = reports;
            let _ = reports.write_all(line.as_bytes());
        }
    }

    /// Reads the statistics under their lock.
    fn with_stats<R>(&self, read: impl FnOnce(&ProbeStats) -> R) -> R {
        let stats = self.stats.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        read(&stats)
    }

    /// One stamped line to standard error and the probe's log file.
    fn log(&self, message: &str) {
        let line = format!("{} {message}\n", clock_stamp());
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Ok(mut handle) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = handle.write_all(line.as_bytes());
        }
    }

    /// The final summary, as the analysis scripts read it back.
    fn write_summary(&self, text: &str) {
        let _ = std::fs::write(&self.summary_path, text);
    }
}

/// The bytes as lowercase hex, two digits per byte.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A stamp of the local clock, `yyyyMMdd-HHmmss`, as the probe's file names carry it.
fn stamp() -> String {
    let parts = local_time();
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second,
    )
}

/// The local time of day as `HH:MM:SS.mmm`, the format every TouchNext log line starts with.
fn clock_stamp() -> String {
    let parts = local_time();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        parts.hour, parts.minute, parts.second, parts.millisecond,
    )
}

/// The local date and time, through `localtime_r` as [`crate::log`] reads it.
fn local_time() -> LocalTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes a whole `tm` into the zeroed space and reads only the time zone
    // database.
    unsafe {
        libc::localtime_r(&(now.as_secs() as libc::time_t), &mut parts);
    }
    LocalTime {
        year: parts.tm_year + 1900,
        month: (parts.tm_mon + 1) as u8,
        day: parts.tm_mday as u8,
        hour: parts.tm_hour as u8,
        minute: parts.tm_min as u8,
        second: parts.tm_sec as u8,
        millisecond: now.subsec_millis(),
    }
}

/// The fields of one local instant the stamps use.
struct LocalTime {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    millisecond: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_two_digits_per_byte() {
        assert_eq!(hex(&[0x0D, 0xAB, 0x07]), "0dab07");
        assert_eq!(hex(&[]), "");
    }
}
