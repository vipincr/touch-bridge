//! The crash journal: an open press, remembered on disk so a crash can't leave a mouse button held.
//!
//! The file is byte-compatible with the Swift app's journal, so a press left open by one
//! implementation is released by the other: the same field names, and the fields past `pid` and
//! `right` absent when unknown, as Swift's `JSONEncoder` omits nils. An empty file is no press.
//!
//! The file stays open for as long as the app runs. A press overwrites it and a release empties it:
//! writes into the file cache, with nothing created or removed. Creating and removing the file for
//! every click cost ten times as much, and stalled for milliseconds when the volume was busy
//! (2026-11-25, 20 000 clicks: 47 µs each against 5 µs), on the path that leads to the mouse-down.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// An open press, journaled so a crash can't leave a mouse button held: enough to release it where
/// it was made. Before, only the pid was kept, and the release went to the cursor's position (on
/// the other display) without the window fields Direct delivery needs, or to whichever app had
/// inherited the pid.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PressRecord {
    /// 0 when pressed through the window server (Warp).
    pub pid: i32,
    pub right: bool,
    /// Where it was pressed, in global display points (missing in journals from before 0.6.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    /// Direct presses: the target window and its origin, and the app (a restarted system reuses
    /// pids).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "originX")]
    pub origin_x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "originY")]
    pub origin_y: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "bundleID")]
    pub bundle_id: Option<String>,
}

/// Remembers an open press on disk so a crash can't leave a mouse button held down.
pub struct PressJournal {
    /// `None` when the file could not be opened (a full or read-only disk): no journal then, and
    /// the driver runs without one.
    file: Option<File>,
}

impl PressJournal {
    /// The app's journal for this user: `~/Library/Application Support/TouchNext/press.json`.
    pub fn new() -> Self {
        // $HOME is how the rest of the crate finds the user's files; a macOS process always has it.
        let home = std::env::var_os("HOME").unwrap_or_default();
        Self::with_path(&PathBuf::from(home).join("Library/Application Support/TouchNext/press.json"))
    }

    /// A journal at an arbitrary path, for tests.
    pub fn with_path(path: &Path) -> Self {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        Self { file: OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).ok() }
    }

    /// Writes the open press over whatever was journaled before.
    pub fn record(&self, press: &PressRecord) {
        let Some(file) = &self.file else { return };
        let Ok(bytes) = serde_json::to_vec(press) else { return };
        // Emptied first: a crash while this runs leaves either no press (it is not posted yet) or
        // the whole record, never the new one followed by the tail of the old.
        let _ = file.set_len(0).and_then(|()| file.write_all_at(&bytes, 0));
    }

    /// The press was released; nothing is open any more.
    pub fn clear(&self) {
        if let Some(file) = &self.file {
            let _ = file.set_len(0);
        }
    }

    /// The press a previous run left open, if any. Journals written before 0.6.3 hold only `pid`
    /// and `right`; they decode with the rest missing.
    pub fn pending(&self) -> Option<PressRecord> {
        let file = self.file.as_ref()?;
        let length = file.metadata().ok()?.len();
        let mut bytes = vec![0; length as usize];
        file.read_exact_at(&mut bytes, 0).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

impl Default for PressJournal {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal of this test run, in the system's temporary folder.
    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("touchnext-journal-{}-{name}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        path
    }

    fn journaled_direct() -> PressRecord {
        PressRecord {
            pid: 42,
            right: false,
            x: Some(2500.0),
            y: Some(500.0),
            window: Some(7),
            origin_x: Some(2000.0),
            origin_y: Some(100.0),
            bundle_id: Some("com.example.app".to_string()),
        }
    }

    #[test]
    fn a_press_round_trips_and_the_old_format_is_still_read() {
        let path = scratch("round-trip");
        let journal = PressJournal::with_path(&path);
        journal.record(&journaled_direct());
        assert_eq!(journal.pending(), Some(journaled_direct()));
        journal.clear();
        assert_eq!(journal.pending(), None);
        // Written by versions before 0.6.3, which removed the file instead of emptying it.
        fs::write(&path, br#"{"pid":321,"right":true}"#).unwrap();
        assert_eq!(journal.pending(), Some(PressRecord { pid: 321, right: true, ..Default::default() }));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_shorter_press_leaves_no_tail_of_the_one_before() {
        let path = scratch("no-tail");
        let journal = PressJournal::with_path(&path);
        journal.record(&journaled_direct());
        let short = PressRecord { pid: 0, right: true, ..Default::default() };
        journal.record(&short);
        assert_eq!(journal.pending(), Some(short));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn without_a_file_there_is_no_journal() {
        // Its parent is a file, so the journal's file cannot be created.
        let parent = scratch("parent");
        fs::write(&parent, b"not a folder").unwrap();
        let journal = PressJournal::with_path(&parent.join("press.json"));
        journal.record(&journaled_direct());
        journal.clear();
        assert_eq!(journal.pending(), None);
        let _ = fs::remove_file(&parent);
    }
}
