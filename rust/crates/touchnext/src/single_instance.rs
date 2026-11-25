//! Only one TouchNext may own the panel: the first to start holds a lock file, and any later copy
//! sees the lock, logs why it is leaving, and exits.

use std::fs;
use std::path::PathBuf;

/// Holds the lock for as long as it lives. Dropping it (or the process ending) releases the lock.
pub struct SingleInstance {
    descriptor: i32,
}

impl SingleInstance {
    /// Takes the lock, or `None` when another TouchNext already has it.
    pub fn acquire() -> Option<SingleInstance> {
        let directory: PathBuf = crate::home_dir().join("Library/Application Support/TouchNext");
        fs::create_dir_all(&directory).ok()?;
        let path = directory.join("instance.lock");
        let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
        // SAFETY: `c_path` is a valid NUL-terminated path and outlives the call; the other arguments
        // are plain flags.
        let descriptor = unsafe {
            libc::open(c_path.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600)
        };
        if descriptor < 0 {
            return None;
        }
        // SAFETY: `descriptor` is an open file descriptor.
        let taken = unsafe { libc::flock(descriptor, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if taken {
            Some(SingleInstance { descriptor })
        } else {
            // SAFETY: `descriptor` is an open file descriptor that is closed exactly once, here.
            unsafe { libc::close(descriptor) };
            None
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: `descriptor` is open for the whole life of the value, and `drop` runs once.
        unsafe {
            libc::flock(self.descriptor, libc::LOCK_UN);
            libc::close(self.descriptor);
        }
    }
}
