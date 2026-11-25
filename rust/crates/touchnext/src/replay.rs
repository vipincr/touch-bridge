//! `--replay <recording>`: runs a probe recording through the real pipeline in dry run, at the speed
//! it was recorded, with the saved menu settings, and prints every line the pipeline reports.
//!
//! Nothing is posted and the panel is not touched, so this runs next to the installed copy. It is
//! how a gesture problem is measured from a recording before a rule is designed for it.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;
use touchcore::gestures::PanelPoint;
use touchhid::clock;
use touchhid::{PanelEvent, RawReport};
use touchplatform::pipeline::{
    locate_touch_display, ContactsHook, InputPipeline, PipelineHooks, PipelineSettings,
};
use touchplatform::prefs_reader::TrackpadPreferencesReader;

use crate::menu_bar::touch_count_title;
use crate::settings::StoredSettings;

/// One line of a recording, as the probe writes it. The decoded contacts in it are not read back:
/// the pipeline decodes the bytes itself, as it does live.
#[derive(Deserialize)]
struct RecordedReport {
    t_ns: u64,
    hex: String,
}

/// How long the deadlines the recording leaves behind are given to run out. The longest drag grace
/// is 1.2 s.
const TAIL: Duration = Duration::from_millis(3_000);

/// Replays the recording at `path` in dry run and prints what the pipeline does with it. With
/// `frames`, every decoded frame is printed too, one line a finger: `frame <id> <x> <y>`.
pub fn run(path: &str, frames: bool) -> Result<(), String> {
    let reports = read(Path::new(path))?;
    if reports.is_empty() {
        return Err(format!("{path}: no reports"));
    }
    let count = reports.len();
    // The recording maps onto the touch display, as it did when it was made.
    let display = PipelineSettings::default();
    if locate_touch_display(display.display_vendor, display.display_model).is_none() {
        return Err("the touch display is not found, and the recording maps onto it".to_owned());
    }

    // The pipeline's deadlines read this run's clock, so the recording's stamps move onto it. The
    // base is read before anything else, so that no stamp is ahead of the clock when its report
    // is fed.
    let base_ns = clock::now_ns();
    let started = Instant::now();
    let pipeline = InputPipeline::new(PipelineHooks {
        status: Some(Box::new(move |line| emit(started, line))),
        contacts: frames.then(|| {
            Box::new(move |fingers: &BTreeMap<u8, PanelPoint>| {
                for (id, point) in fingers {
                    emit(started, &format!("frame {id} {:.4} {:.4}", point.x, point.y));
                }
            }) as ContactsHook
        }),
        touch_count: Some(Box::new(move |now, most| emit(started, &touch_count_title(now, most)))),
        report: None,
    });
    // The saved menu settings, as the app applies them, in dry run: nothing is posted.
    let stored = StoredSettings::load();
    let trackpad = TrackpadPreferencesReader::read();
    for line in stored.log_lines(&trackpad) {
        emit(started, &line);
    }
    pipeline.update(move |settings| {
        stored.apply(settings, &trackpad);
        settings.dry_run = true;
    });

    let first_ns = reports[0].0;
    for (t_ns, bytes) in reports {
        let due = started + Duration::from_nanos(t_ns - first_ns);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        pipeline.feed(PanelEvent::Report(RawReport { timestamp_ns: base_ns + (t_ns - first_ns), bytes }));
    }
    std::thread::sleep(TAIL);
    emit(started, &format!("replayed {count} reports from {path}"));
    drop(pipeline);
    Ok(())
}

/// The recording's reports: when each arrived (nanoseconds of the probe run) and its bytes.
fn read(path: &Path) -> Result<Vec<(u64, Vec<u8>)>, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut reports = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| format!("{}: {error}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let record: RecordedReport = serde_json::from_str(&line).map_err(|error| format!("{}: {error}", path.display()))?;
        let bytes: Result<Vec<u8>, _> = (0..record.hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&record.hex[index..index + 2], 16))
            .collect();
        let bytes = bytes.map_err(|error| format!("{}: not the probe's hex: {error}", path.display()))?;
        reports.push((record.t_ns, bytes));
    }
    Ok(reports)
}

/// One line of the report, stamped with the time into the run.
fn emit(started: Instant, line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{:9.3} {line}", started.elapsed().as_secs_f32());
}
