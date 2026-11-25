//! "Start at Login": a per-user launch agent in `~/Library/LaunchAgents` that runs the installed app's
//! executable by path. launchd starts TouchNext at login and restarts it after a crash; a normal Quit
//! (exit 0) stays quit until the next login.
//!
//! It is a plain launch agent and not SMAppService on purpose: SMAppService pins the exact build that
//! registered it (bundle identity and code signature), so every rebuild of this app broke it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "com.gramini.touchnext.agent";

/// Where the agent's property list lives.
pub fn plist_path() -> PathBuf {
    crate::home_dir().join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

pub fn is_enabled() -> bool {
    plist_path().is_file()
}

/// True when launchd started this process through the agent (so a crash will be restarted).
pub fn is_launchd_instance() -> bool {
    std::env::var("XPC_SERVICE_NAME").is_ok_and(|name| name == LABEL)
}

/// One line for the menu and the log.
pub fn describe() -> String {
    if !is_enabled() {
        return "off".to_string();
    }
    if is_launchd_instance() { "on (running as login item)" } else { "on" }.to_string()
}

/// Writes the agent for this copy of the app and loads it; launchd then starts it (`RunAtLoad`).
pub fn enable() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| format!("no executable path: {error}"))?;
    let path = plist_path();
    let agent_log = crate::log::FileLog::directory().join("agent.log");
    let plist = agent_plist(&executable, &agent_log);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    // Written beside the real file and renamed over it, so launchd never reads a half-written agent.
    let temporary = path.with_extension("plist.new");
    fs::write(&temporary, plist).map_err(|error| format!("{}: {error}", temporary.display()))?;
    fs::rename(&temporary, &path).map_err(|error| format!("{}: {error}", path.display()))?;
    // As in the Swift original, launchctl's own exit status is not an error here: the agent is
    // written, and whether launchd took it shows in the menu line and the log.
    launchctl(&["bootout", &service()]);
    launchctl(&["bootstrap", &format!("gui/{}", uid()), &path.to_string_lossy()]);
    Ok(())
}

/// Unloads the agent (which stops an instance launchd is running) and removes it.
pub fn disable() {
    launchctl(&["bootout", &service()]);
    let _ = fs::remove_file(plist_path());
}

/// Starts the agent's instance a moment after this process has exited, so the panel changes hands
/// without both copies contending for it.
pub fn hand_over_to_launchd() {
    let script = format!("sleep 1; /bin/launchctl kickstart {}", service());
    // The shell outlives this process, so it is neither waited for nor killed with it.
    let _ = Command::new("/bin/sh").args(["-c", &script]).spawn();
}

fn service() -> String {
    format!("gui/{}/{}", uid(), LABEL)
}

fn uid() -> u32 {
    // SAFETY: `getuid` has no preconditions.
    unsafe { libc::getuid() }
}

/// Runs launchctl with the given arguments, discarding its output. Returns its exit status, or `None`
/// when it could not start.
fn launchctl(arguments: &[&str]) -> Option<i32> {
    Command::new("/bin/launchctl")
        .args(arguments)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .and_then(|status| status.code())
}

/// The launch agent: the executable by path, started at login and kept alive after a crash, with its
/// output in the log directory.
fn agent_plist(executable: &Path, agent_log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{executable}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <key>LimitLoadToSessionType</key>
    <array>
        <string>Aqua</string>
    </array>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        label = LABEL,
        executable = escape(executable.to_string_lossy()),
        log = escape(agent_log.to_string_lossy()),
    )
}

/// Escapes a path for the text of an XML element.
fn escape(text: std::borrow::Cow<'_, str>) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
