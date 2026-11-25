//! Aggregates what the M1 probe needs to decide its gates: which report IDs
//! arrive, how many fingers the digitizer reports at once, the report rate and
//! the scan-time step.

use std::collections::{BTreeMap, BTreeSet};

use crate::report::TouchFrame;

/// Aggregates what the M1 probe needs to decide its gates: which report IDs
/// arrive, how many fingers the digitizer reports at once, the report rate and
/// the scan-time step.
#[derive(Debug, Default)]
pub struct ProbeStats {
    reports_by_id: BTreeMap<u8, usize>,
    max_simultaneous_contacts: usize,
    distinct_contact_ids: BTreeSet<u8>,
    last_timestamp_ns: Option<u64>,
    last_scan_time: Option<u32>,
    intervals_ns: Vec<u64>,
    scan_time_steps: Vec<u32>,
}

impl ProbeStats {
    const SAMPLE_LIMIT: usize = 10_000;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn reports_by_id(&self) -> &BTreeMap<u8, usize> {
        &self.reports_by_id
    }

    pub fn max_simultaneous_contacts(&self) -> usize {
        self.max_simultaneous_contacts
    }

    pub fn distinct_contact_ids(&self) -> &BTreeSet<u8> {
        &self.distinct_contact_ids
    }

    /// The gate the probe uses to decide the panel really does multitouch: at
    /// least two touching contacts seen at once on the digitizer interface.
    pub fn multitouch_gate_passed(&self) -> bool {
        self.max_simultaneous_contacts >= 2
    }

    pub fn median_interval_ms(&self) -> Option<f64> {
        median(&self.intervals_ns).map(|v| v as f64 / 1_000_000.0)
    }

    pub fn median_scan_time_step(&self) -> Option<u32> {
        median(&self.scan_time_steps)
    }

    pub fn record(&mut self, timestamp_ns: u64, report_id: u8, frame: Option<&TouchFrame>) {
        *self.reports_by_id.entry(report_id).or_insert(0) += 1;
        if let Some(last) = self.last_timestamp_ns
            && timestamp_ns > last && self.intervals_ns.len() < Self::SAMPLE_LIMIT {
                self.intervals_ns.push(timestamp_ns - last);
            }
        self.last_timestamp_ns = Some(timestamp_ns);

        let Some(frame) = frame else { return };
        if frame.source != crate::report::Source::Digitizer {
            return;
        }
        let touching = frame.contacts.iter().filter(|c| c.touching).count();
        self.max_simultaneous_contacts = self.max_simultaneous_contacts.max(touching);
        self.distinct_contact_ids.extend(frame.contacts.iter().map(|c| c.id));
        if let Some(scan_time) = frame.scan_time {
            if let Some(last) = self.last_scan_time
                && scan_time > last && self.scan_time_steps.len() < Self::SAMPLE_LIMIT {
                    self.scan_time_steps.push(scan_time - last);
                }
            self.last_scan_time = Some(scan_time);
        }
    }
}

/// Swift's median: the middle element of the sorted samples (upper median for
/// an even count), nothing for no samples.
fn median<T: Copy + Ord>(values: &[T]) -> Option<T> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort();
    Some(sorted[values.len() / 2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Contact, Source};
    use smallvec::smallvec;

    fn frame(touching: &[u8], lifted: &[u8], scan_time: Option<u32>) -> TouchFrame {
        TouchFrame {
            source: Source::Digitizer,
            contacts: touching
                .iter()
                .map(|&id| Contact { id, touching: true, x: 0.5, y: 0.5 })
                .chain(lifted.iter().map(|&id| Contact { id, touching: false, x: 0.5, y: 0.5 }))
                .collect(),
            scan_time,
        }
    }

    #[test]
    fn counts_reports_per_report_id() {
        let mut stats = ProbeStats::default();
        stats.record(0, 4, Some(&frame(&[0], &[], Some(0))));
        stats.record(1, 4, Some(&frame(&[0], &[], Some(0))));
        stats.record(2, 5, None);
        let mut expected = BTreeMap::new();
        expected.insert(4, 2);
        expected.insert(5, 1);
        assert_eq!(stats.reports_by_id(), &expected);
    }

    #[test]
    fn tracks_max_simultaneous_touching_digitizer_contacts() {
        let mut stats = ProbeStats::default();
        stats.record(0, 4, Some(&frame(&[0, 1, 2], &[3], Some(0))));
        stats.record(1, 4, Some(&frame(&[0], &[], Some(0))));
        assert_eq!(stats.max_simultaneous_contacts(), 3);
        let mut expected = BTreeSet::new();
        expected.extend([0, 1, 2, 3]);
        assert_eq!(stats.distinct_contact_ids(), &expected);
    }

    #[test]
    fn multitouch_gate_passes_only_with_two_or_more_simultaneous_contacts() {
        let mut single = ProbeStats::default();
        single.record(0, 4, Some(&frame(&[0], &[], Some(0))));
        assert!(!single.multitouch_gate_passed());

        let mut multi = ProbeStats::default();
        multi.record(0, 4, Some(&frame(&[0, 1], &[], Some(0))));
        assert!(multi.multitouch_gate_passed());
    }

    #[test]
    fn mouse_frames_do_not_count_toward_multitouch() {
        let mut stats = ProbeStats::default();
        let mouse = TouchFrame {
            source: Source::Mouse,
            contacts: smallvec![Contact { id: 0, touching: true, x: 0.0, y: 0.0 }],
            scan_time: None,
        };
        stats.record(0, 5, Some(&mouse));
        assert_eq!(stats.max_simultaneous_contacts(), 0);
    }

    #[test]
    fn measures_median_report_interval_and_scan_time_step() {
        let mut stats = ProbeStats::default();
        // 8 ms apart on the host clock; scan time advances by 80 per report.
        for i in 0..5u64 {
            stats.record(i * 8_000_000, 4, Some(&frame(&[0], &[], Some((i * 80) as u32))));
        }
        assert_eq!(stats.median_interval_ms(), Some(8.0));
        assert_eq!(stats.median_scan_time_step(), Some(80));
    }
}
