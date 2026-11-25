//! The app's own record of what it did and why: timestamped lines appended to
//! `~/Library/Logs/TouchNext/<name>.log`. Any thread may write, and a line is never interleaved with
//! another.
//!
//! Only a build with the `debug` feature has it (`./scripts/bundle.sh --debug`). Without the feature
//! [`FileLog`] is nothing and [`log!`] drops its line unbuilt: the prod build opens no file and
//! formats no line.

/// Appends a line to the log. Without the `debug` feature the line is never built.
macro_rules! log {
    ($log:expr, $($argument:tt)*) => {
        if cfg!(feature = "debug") {
            ($log).write(&format!($($argument)*))
        }
    };
}
pub(crate) use log;

#[cfg(feature = "debug")]
use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Appends stamped lines to one log file; any thread may write.
#[cfg(feature = "debug")]
pub struct FileLog {
    sink: Mutex<Box<dyn Write + Send>>,
}

/// The log of a build without the `debug` feature: nothing is opened, and nothing is kept.
#[cfg(not(feature = "debug"))]
pub struct FileLog;

#[cfg(feature = "debug")]
impl FileLog {
    /// Opens `<name>.log` in the log directory for appending, creating the directory and the file.
    /// When that fails (the lines would be lost either way) the lines go to the app's standard error,
    /// which a launch agent captures in `agent.log`.
    pub fn new(name: &str) -> FileLog {
        let directory = crate::logs_directory();
        let path = directory.join(format!("{name}.log"));
        let file = fs::create_dir_all(&directory)
            .and_then(|()| OpenOptions::new().create(true).append(true).open(&path))
            .ok();
        let sink: Box<dyn Write + Send> = match file {
            Some(file) => Box::new(file),
            None => Box::new(std::io::stderr()),
        };
        FileLog { sink: Mutex::new(sink) }
    }

    /// Appends `message` with the local time in front.
    pub(crate) fn write(&self, message: &str) {
        let line = format!("{} {message}\n", local_stamp());
        if let Ok(mut sink) = self.sink.lock() {
            let _ = sink.write_all(line.as_bytes());
        }
    }
}

#[cfg(not(feature = "debug"))]
impl FileLog {
    /// Opens nothing.
    #[inline(always)]
    pub fn new(_name: &str) -> FileLog {
        FileLog
    }

    /// Keeps nothing.
    #[inline(always)]
    pub(crate) fn write(&self, _message: &str) {}
}

/// The local time of day as `HH:MM:SS.mmm`, the format the log has always had.
#[cfg(feature = "debug")]
fn local_stamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` writes a whole `tm` into the zeroed space and reads only the time zone
    // database.
    unsafe {
        libc::localtime_r(&(now.as_secs() as libc::time_t), &mut parts);
    }
    format!("{:02}:{:02}:{:02}.{:03}", parts.tm_hour, parts.tm_min, parts.tm_sec, now.subsec_millis())
}

/// The gate itself, the other way round in each build: a prod build keeps no log, a debug build
/// writes the lines it is handed.
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "debug"))]
    #[test]
    fn a_prod_build_keeps_no_log() {
        let name = format!("gate-check-{}", std::process::id());
        let log = FileLog::new(&name);
        log!(log, "a line that is never written: {name}");
        assert!(!crate::logs_directory().join(format!("{name}.log")).exists());
    }

    #[cfg(feature = "debug")]
    #[test]
    fn a_debug_build_writes_the_lines_it_is_handed() {
        let name = format!("gate-check-{}", std::process::id());
        let path = crate::logs_directory().join(format!("{name}.log"));
        let _ = std::fs::remove_file(&path);
        let log = FileLog::new(&name);
        log!(log, "a line: {name}");
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(text.ends_with(&format!(" a line: {name}\n")), "{text}");
        let stamp = text.split(' ').next().unwrap();
        assert_eq!(stamp.len(), 12, "HH:MM:SS.mmm: {stamp}");
    }
}
