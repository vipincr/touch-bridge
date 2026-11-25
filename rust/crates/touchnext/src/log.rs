//! The app's own record of what it did and why: timestamped lines appended to
//! `~/Library/Logs/TouchNext/<name>.log`. Any thread may write, and a line is never interleaved with
//! another.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct FileLog {
    sink: Mutex<Box<dyn Write + Send>>,
}

impl FileLog {
    /// Where every TouchNext log lives.
    pub fn directory() -> PathBuf {
        crate::home_dir().join("Library/Logs/TouchNext")
    }

    /// Opens `<name>.log` in the log directory for appending, creating the directory and the file.
    /// When that fails (the lines would be lost either way) the lines go to the app's standard error,
    /// which a launch agent captures in `agent.log`.
    pub fn new(name: &str) -> FileLog {
        let path = Self::directory().join(format!("{name}.log"));
        let file = fs::create_dir_all(Self::directory())
            .and_then(|()| OpenOptions::new().create(true).append(true).open(&path))
            .ok();
        let sink: Box<dyn Write + Send> = match file {
            Some(file) => Box::new(file),
            None => Box::new(std::io::stderr()),
        };
        FileLog { sink: Mutex::new(sink) }
    }

    pub fn write(&self, message: &str) {
        let line = format!("{} {message}\n", local_stamp());
        if let Ok(mut sink) = self.sink.lock() {
            let _ = sink.write_all(line.as_bytes());
        }
    }
}

/// The local time of day as `HH:MM:SS.mmm`, the format the log has always had.
fn local_stamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes a whole `tm` into the zeroed space and reads only the time zone
    // database.
    unsafe {
        libc::localtime_r(&(now.as_secs() as libc::time_t), &mut parts);
    }
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        parts.tm_hour, parts.tm_min, parts.tm_sec, now.subsec_millis()
    )
}
