//! The pipeline: HID reports → contacts → gestures → Direct/Warp delivery onto the touch display,
//! with the Dock, Space switching, focus keeping and TUIO around it.
//!
//! One state machine ([`Pipeline`]) owned by a thread of its own; [`InputPipeline`] starts the
//! thread and talks to it over a channel. The machine never reads the clock and never starts a
//! timer: every method takes `now_ns` (nanoseconds of [`touchhid::clock`], the clock the report
//! stamps are on), whatever is pending is a deadline in the struct, and [`Pipeline::tick`]
//! performs what is due. While reports come, `now_ns` is the report's stamp: the machine's time
//! is when the panel was heard. The Swift kept this state on the HID source's dispatch queue and
//! woke it twenty times a second with a repeating timer; this thread sleeps until the earliest
//! deadline or the next command.
//!
//! The thread runs at the user-interactive quality of service the Swift queue ran at, and the
//! display-reconfiguration callback only pushes a command into the channel: every Apple call and
//! every decision happens here, in order with the reports.

use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGDisplayBounds, CGDisplayChangeSummaryFlags, CGDisplayIsBuiltin, CGDisplayModelNumber,
    CGDisplayRegisterReconfigurationCallback, CGDisplayRemoveReconfigurationCallback,
    CGDisplayRotation, CGDisplayVendorNumber, CGMouseButton, CGMainDisplayID,
};
use smallvec::SmallVec;
use touchcore::dock_hold::{DockSwipeHold, Step as HoldStep};
use touchcore::focus::{Action as FocusAction, FocusKeeper};
use touchcore::gestures::{
    DockSwipeKind, GestureAction, GestureConfig, GesturePhase, GestureRecognizer, InputMode,
    OneFingerDragMode, PanelPoint, ThreeFingerAnchor,
};
use touchcore::launchpad::{
    Action as DockReturnAction, DockReturnWatch, LaunchpadDock, LaunchpadDockMode, Plan,
};
use touchcore::mapper::{CoordinateMapper, ScreenPoint, ScreenRect};
use touchcore::mode::{Action as ModeAction, PanelModeWatch};
use touchcore::policy::{
    DeliveryContext, DeliveryMode, DeliveryPolicy, FocusMode, GestureKind, WindowHitTester,
    WindowInfo,
};
use touchcore::prefs::TrackpadPreferences;
use touchcore::report::{ReportParser, Source};
use touchcore::stream::ContactStream;
use touchcore::tracker::{ContactEvent, ContactPhase};
use touchcore::tuio::{TUIO, TUIOSessions};
use touchhid::clock;
use touchhid::{HidTouchSource, PanelEvent};

use crate::apps;
#[cfg(feature = "debug")]
use crate::LogSink;
use crate::deliverer::{Deliverer, SystemDeliveryEffects, Target, WARP_RETURN_DELAY_NS};
use crate::direct::DirectTarget;
use crate::dock::DockLocator;
use crate::dock_swipe::{DockSwipe, Kind as SwipeKind, Phase as SwipePhase};
use crate::geometry::{cg_point, screen_point, screen_rect};
use crate::notification::NotificationCenterOpener;
use crate::system_displays::SystemDisplays;
use crate::tuio_sender::TUIOSender;
use crate::window::{AppDirectory, WindowDragArea, WindowLocator};

/// How long the hidden cursor stays on the touch display after a Space switch ends or
/// Notification Center is opened: past the dock swipe's re-sent end events (0.5 s), so the system
/// has picked its display.
const SYSTEM_GESTURE_CURSOR_DELAY_NS: u64 = 800_000_000;
/// Longest push against a bottom edge before giving up on moving the Dock.
const DOCK_PUSH_TIMEOUT_MS: f64 = 1500.0;
/// How often a TUIO idle frame goes out while nothing touches, so clients keep their state.
const TUIO_IDLE_NS: u64 = 1_000_000_000;
/// Between two nudges of a Dock push (the Swift `asyncAfter(0.015)`).
const DOCK_PUSH_STEP_NS: u64 = 15_000_000;
/// Between two polls of the Dock return watch (the Swift `dockReturnTimer`, 250 ms).
const DOCK_RETURN_POLL_NS: u64 = 250_000_000;
/// Between two polls of the focus keeper (the Swift `asyncAfter(0.05)` while waiting).
const FOCUS_POLL_NS: u64 = 50_000_000;
/// When the diagnostic result line of a system gesture is logged (the Swift `asyncAfter(1.0)`).
#[cfg(feature = "debug")]
const OUTCOME_DELAY_NS: u64 = 1_000_000_000;
/// How often the report census is logged while frames flow.
const CENSUS_INTERVAL_NS: u64 = 10_000_000_000;
/// Mouse-emulation frames newer than the last digitizer frame by less than this are dropped, so
/// one finger is not delivered twice.
const MOUSE_ECHO_WINDOW_NS: u64 = 1_000_000_000;

/// Live settings, changed from the menu. Applied on the pipeline's thread.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineSettings {
    pub policy: DeliveryPolicy,
    pub dry_run: bool,
    /// Bundle identifier → forced delivery mode.
    pub app_overrides: HashMap<String, DeliveryMode>,
    pub focus: FocusMode,
    pub one_finger_drag: OneFingerDragMode,
    pub three_finger_drag: bool,
    pub three_finger_anchor: ThreeFingerAnchor,
    /// When set, the trackpad's settings decide three-finger drag, scroll, pinch, rotate and
    /// momentum.
    pub trackpad: Option<TrackpadPreferences>,
    /// Launchpad opens on the display with the Dock: whether a pinch-in on the touch display
    /// brings it over.
    pub launchpad_dock: LaunchpadDockMode,
    /// After a Space switch on the touch display macOS moves focus there; when on, focus that was
    /// on another display is given back ([`FocusKeeper`]).
    pub keep_focus_on_space_switch: bool,
    /// Two-finger scrolls carry the trackpad's gesture stream, so Safari's back/forward page swipe
    /// (and other apps' trackpad swipes) follow the fingers.
    pub swipe_between_pages: bool,
    /// Gestures (default), drawing (one finger is a pen), or multitouch (every finger as TUIO,
    /// nothing else).
    pub input_mode: InputMode,
    /// The touch panel's active area in millimetres, for gesture thresholds (JAPANNEXT
    /// JN-MD-IPST105FP: 10.5-inch 3:2, 1920×1280 → 221.9 × 147.9 mm). Applied to every recognizer.
    pub panel_width_mm: f64,
    pub panel_height_mm: f64,
    /// The touch display's EDID identity (JN-MD-IPST105FP: manufacturer "RTK" = 0x4A8B, product
    /// 0x2222). A display matching both is used; when none does, the pipeline falls back to the
    /// single external display (see `refresh_display`), so an unlisted panel still works.
    pub display_vendor: u32,
    pub display_model: u32,
}

impl Default for PipelineSettings {
    /// Exactly the Swift struct's default values.
    fn default() -> Self {
        Self {
            policy: DeliveryPolicy::default(),
            dry_run: false,
            app_overrides: HashMap::new(),
            focus: FocusMode::FollowTouch,
            one_finger_drag: OneFingerDragMode::Scroll,
            three_finger_drag: true,
            three_finger_anchor: ThreeFingerAnchor::Centroid,
            trackpad: None,
            launchpad_dock: LaunchpadDockMode::BringDockAndReturn,
            keep_focus_on_space_switch: true,
            swipe_between_pages: true,
            input_mode: InputMode::Gestures,
            panel_width_mm: 221.9,
            panel_height_mm: 147.9,
            display_vendor: 19_083,
            display_model: 8_738,
        }
    }
}

/// What the pipeline reports while it runs. Each hook is called on the pipeline's thread, and
/// dropped there when the pipeline is; a hook that is `None` hears nothing.
#[derive(Default)]
pub struct PipelineHooks {
    /// Every status line, the ones the app's menu and the owner's log read.
    pub status: Option<StatusHook>,
    /// Every finger on the panel (up to 10) after each frame, in panel coordinates. The hook for a
    /// future painting/game mode.
    pub contacts: Option<ContactsHook>,
    /// Called when the number of fingers changes: (now, most seen since start).
    pub touch_count: Option<TouchCountHook>,
    /// Every raw panel report with its stamp, as it arrives and before anything is made of it.
    /// Only a build with the `debug` feature has it: the app's log keeps them there, so a session
    /// can be replayed from its own input.
    #[cfg(feature = "debug")]
    pub report: Option<ReportHook>,
}

/// A status line.
pub type StatusHook = Box<dyn FnMut(&str) + Send>;
/// Every finger on the panel, by contact id.
pub type ContactsHook = Box<dyn FnMut(&BTreeMap<u8, PanelPoint>) + Send>;
/// How many fingers are down now, and the most there have been.
pub type TouchCountHook = Box<dyn FnMut(usize, usize) + Send>;
/// One raw panel report: its bytes, and the stamp the panel gave it (nanoseconds).
#[cfg(feature = "debug")]
pub type ReportHook = Box<dyn FnMut(&[u8], u64) + Send>;

/// One item of work for the pipeline's thread, applied in order with the reports.
enum Command {
    Panel(PanelEvent),
    Update(SettingsChange),
    ReturnDockHome,
    DisplaysChanged,
    Start { panel: Sender<Command> },
    Stop(Option<Sender<()>>),
    Shutdown,
}

/// A settings change, applied on the pipeline's thread.
type SettingsChange = Box<dyn FnOnce(&mut PipelineSettings) + Send>;

/// HID reports → contacts → gestures → Direct/Warp delivery onto the touch display. Owns its
/// thread.
///
/// The app calls this from its main thread only. [`InputPipeline::start`] and
/// [`InputPipeline::stop`] return (or block, for `stop`) exactly as their doc comments say; the
/// work itself happens on the pipeline's thread, in order with the reports.
pub struct InputPipeline {
    /// Commands to the thread.
    tx: Sender<Command>,
    /// A second sender at a stable address, handed to the display-reconfiguration callback as its
    /// `user_info`. Kept boxed in the handle and dropped only after the thread has joined, so the
    /// pointer outlives every callback.
    callback_sender: Box<Sender<Command>>,
    thread: Option<JoinHandle<()>>,
    /// Between `start` and `stop`: start is a no-op when already running, stop when not running.
    started: AtomicBool,
}

impl InputPipeline {
    /// Starts the thread with the default settings; nothing touches the panel until [`start`](Self::start).
    pub fn new(hooks: PipelineHooks) -> Self {
        let (tx, rx) = mpsc::channel();
        let hooks = Arc::new(Mutex::new(hooks));
        let shared = Arc::new(Shared {
            hooks: Arc::clone(&hooks),
            apps: AppDirectory::new(),
            mapper: Mutex::new(None),
            window_drag_memo: Mutex::new(Vec::new()),
            own_pid: std::process::id() as i32,
        });
        let callback_sender = Box::new(tx.clone());
        // The thread takes over the queue role the Swift drove with its repeating timer; the name
        // is the queue label it replaces.
        let thread = std::thread::Builder::new()
            .name("com.gramini.touchnext.hid".to_string())
            .spawn(move || thread_main(rx, hooks, shared))
            .expect("spawn the input pipeline thread");
        Self { tx, callback_sender, thread: Some(thread), started: AtomicBool::new(false) }
    }

    /// Finds the panel, seizes it, and begins delivering. Returns at once. A no-op when already
    /// running.
    pub fn start(&self) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        // SAFETY: `user_info` points at the boxed sender, which lives in this handle until after
        // the thread has joined and outlives every callback; the callback itself only sends.
        unsafe {
            CGDisplayRegisterReconfigurationCallback(Some(display_changed), self.callback_user_info());
        }
        let _ = self.tx.send(Command::Start { panel: self.tx.clone() });
    }

    /// Releases the panel and closes whatever gesture is open. Returns once that is done, because
    /// the app's menu shows the panel released when it does. A no-op when not running.
    pub fn stop(&self) {
        if !self.started.swap(false, Ordering::SeqCst) {
            return;
        }
        self.remove_display_callback();
        let (ack, ack_rx) = mpsc::channel();
        let _ = self.tx.send(Command::Stop(Some(ack)));
        // The thread answers when the panel is released and the gesture is closed. An error means
        // the thread is already gone, which is the state this call was asking for.
        let _ = ack_rx.recv();
    }

    /// Applies `change` to the settings on the pipeline's thread, in order with the reports.
    pub fn update(&self, change: impl FnOnce(&mut PipelineSettings) + Send + 'static) {
        let _ = self.tx.send(Command::Update(Box::new(change)));
    }

    /// "Then back" chosen in the menu: a Dock found on the touch display goes home.
    pub fn return_dock_home(&self) {
        let _ = self.tx.send(Command::ReturnDockHome);
    }

    /// Hands the pipeline a panel event as if the panel had sent it, in order with everything else.
    /// The replay tool (`--replay`) uses it on a pipeline that never started the source, with each
    /// report stamped for the moment the recording says it came: the pipeline runs whatever fell
    /// due before that moment first, so a recording fed a little late is played the same.
    pub fn feed(&self, event: PanelEvent) {
        let _ = self.tx.send(Command::Panel(event));
    }

    /// The `user_info` pointer handed to the reconfiguration callback: the boxed sender's address,
    /// stable for the handle's lifetime.
    fn callback_user_info(&self) -> *mut c_void {
        std::ptr::from_ref(&*self.callback_sender) as *const Sender<Command> as *mut c_void
    }

    /// Removes the display-reconfiguration callback, matching the exact pointer pair `start`
    /// registered.
    fn remove_display_callback(&self) {
        // SAFETY: the same callback and pointer `start` registered; CoreGraphics removes exactly
        // that pair.
        unsafe { CGDisplayRemoveReconfigurationCallback(Some(display_changed), self.callback_user_info()) };
    }
}

impl Drop for InputPipeline {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            // A panic inside the pipeline surfaces here as an `Err`; dropping must not panic.
            let _ = thread.join();
        }
        if self.started.swap(false, Ordering::SeqCst) {
            // `stop` was never called, so the callback is still registered and must be removed
            // before the sender it points at is dropped.
            self.remove_display_callback();
        }
        // `callback_sender` is dropped after this line, once no callback can fire.
    }
}

/// The display-reconfiguration callback: the displays changed. It only pushes a command; the
/// pipeline's thread does the work. `user_info` is the boxed command sender.
///
/// # Safety
///
/// `user_info` must be the pointer [`InputPipeline::callback_user_info`] handed out, and it must
/// live as long as the callback stays registered (the handle drops it only after the thread has
/// joined).
unsafe extern "C-unwind" fn display_changed(
    _display: u32,
    _flags: CGDisplayChangeSummaryFlags,
    user_info: *mut c_void,
) {
    if user_info.is_null() {
        return;
    }
    // SAFETY: the pointer is the boxed sender, valid until the handle is dropped after the join.
    let sender = unsafe { &*(user_info as *const Sender<Command>) };
    let _ = sender.send(Command::DisplaysChanged);
}

/// `int pthread_set_qos_class_self_np(qos_class_t class, int priority)`: puts the calling thread
/// at a quality of service. The function is in neither libc nor the bindings, so it is looked up
/// by name.
type SetQosClassSelf = unsafe extern "C" fn(libc::c_uint, libc::c_int) -> libc::c_int;

static SET_QOS_CLASS_SELF: OnceLock<Option<SetQosClassSelf>> = OnceLock::new();

/// `QOS_CLASS_USER_INTERACTIVE`, the quality of service the Swift queue ran at.
const QOS_CLASS_USER_INTERACTIVE: libc::c_uint = 0x21;

/// Puts the pipeline's thread at the Swift queue's quality of service, the class a thread may ask
/// for itself. A missing symbol only leaves the thread at its default priority.
fn set_thread_qos_user_interactive() {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    let Some(set_qos) = (unsafe { crate::runtime_symbols::lookup(c"pthread_set_qos_class_self_np", &SET_QOS_CLASS_SELF) })
    else {
        return;
    };
    // SAFETY: the call only sets the calling thread's QoS class and priority.
    unsafe { set_qos(QOS_CLASS_USER_INTERACTIVE, 0) };
}

/// The pipeline's thread: owns the state machine, sleeps until a command or the earliest
/// deadline, and never polls.
///
/// The machine runs on the reports' own clock. A report is stamped when it reaches the process,
/// and that stamp is its time while it is handled ([`handle`]): the machine's decisions depend on
/// when the reports came, not on the delays of this thread in between (an Accessibility query
/// takes milliseconds, sometimes tens), and a recording stamped as it was made plays back the
/// same way every time. Other commands, and the wait for a deadline, are on the system clock.
fn thread_main(rx: mpsc::Receiver<Command>, hooks: Arc<Mutex<PipelineHooks>>, shared: Arc<Shared>) {
    set_thread_qos_user_interactive();
    let mut pipeline = Pipeline::new(hooks, shared);
    // The machine's time: the moment of the latest report, command or deadline it has been taken to.
    let mut now_ns = clock::now_ns();
    loop {
        // What is waiting goes first, in order: each command takes the machine to its own moment.
        loop {
            let Ok(command) = rx.try_recv() else { break };
            if handle(&mut pipeline, &mut now_ns, command) {
                return;
            }
        }
        match pipeline.next_deadline_ns() {
            // Due, and overtaken by nothing: it runs, at its own moment.
            Some(deadline) if deadline <= now_ns => {
                pipeline.tick(deadline);
                now_ns = deadline;
            }
            Some(deadline) => match rx.recv_timeout(Duration::from_nanos(deadline - now_ns)) {
                Ok(command) => {
                    if handle(&mut pipeline, &mut now_ns, command) {
                        return;
                    }
                }
                // The deadline came due while waiting; the next pass runs it.
                Err(RecvTimeoutError::Timeout) => now_ns = deadline,
                Err(RecvTimeoutError::Disconnected) => {
                    pipeline.stop(now_ns);
                    return;
                }
            },
            None => match rx.recv() {
                Ok(command) => {
                    if handle(&mut pipeline, &mut now_ns, command) {
                        return;
                    }
                }
                Err(_) => {
                    pipeline.stop(now_ns);
                    return;
                }
            },
        }
    }
}

/// Takes the machine to one command's moment and applies it there. A report's moment is its
/// stamp, and whatever fell due before it runs first, each at its own moment: a lift the panel
/// has gone quiet after is over before a touch that was stamped later begins. Returns true when
/// the thread should leave.
fn handle(pipeline: &mut Pipeline, now_ns: &mut u64, command: Command) -> bool {
    let at_ns = match &command {
        Command::Panel(PanelEvent::Report(report)) => {
            while let Some(deadline) = pipeline.next_deadline_ns()
                && deadline <= report.timestamp_ns
            {
                pipeline.tick(deadline);
                *now_ns = deadline;
            }
            *now_ns = (*now_ns).max(report.timestamp_ns);
            report.timestamp_ns
        }
        _ => {
            *now_ns = (*now_ns).max(clock::now_ns());
            *now_ns
        }
    };
    pipeline.command(command, at_ns)
}

/// What the recognizer's `drag_zone` closure and the delivery decision share, behind an `Arc`
/// because the recognizer calls the closure from inside `handle`, where the pipeline's own borrow
/// of `self` is out.
struct Shared {
    /// The hooks, reachable from the status lines of the shared helpers.
    hooks: Arc<Mutex<PipelineHooks>>,
    apps: AppDirectory,
    /// The touch display's mapper, set by `refresh_display`; `None` while the display is unknown.
    mapper: Mutex<Option<CoordinateMapper>>,
    /// The window-drag answers of the gesture being classified, so Accessibility is asked once
    /// per point: for where the finger landed, and for a window drag also where it is pressed.
    window_drag_memo: Mutex<Vec<(CGPoint, bool)>>,
    /// This process's pid, whose own windows are never touch targets.
    own_pid: i32,
}

impl Shared {
    fn mapper(&self) -> Option<CoordinateMapper> {
        *self.mapper.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_mapper(&self, mapper: Option<CoordinateMapper>) {
        *self.mapper.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = mapper;
    }

    fn window_drag_answer(&self, point: CGPoint) -> Option<bool> {
        self.window_drag_memo
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(asked, _)| same_point(*asked, point))
            .map(|(_, result)| *result)
    }

    fn remember_window_drag(&self, point: CGPoint, result: bool) {
        self.window_drag_memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push((point, result));
    }

    fn forget_window_drag_answers(&self) {
        self.window_drag_memo.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }
}

/// Whether two screen points are the same one to the window server (half a point apart at most).
fn same_point(a: CGPoint, b: CGPoint) -> bool {
    (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5
}

/// Sends one line to the status hook, when there is one. Runs on the pipeline's thread.
fn emit_status(hooks: &Arc<Mutex<PipelineHooks>>, message: &str) {
    let mut hooks = hooks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(hook) = &mut hooks.status {
        hook(message);
    }
}

/// Whether a pan or drag starting at `point` moves the window (toolbar, title bar, title text).
/// Answers from what this gesture asked already, else asks the window list and Accessibility once
/// and remembers the answer.
fn starts_window_drag(shared: &Shared, point: CGPoint, windows: &[WindowInfo]) -> bool {
    if let Some(result) = shared.window_drag_answer(point) {
        return result;
    }
    let Some(window) = WindowHitTester::top_window(screen_point(point), windows, shared.own_pid)
    else {
        return false;
    };
    if window.layer != 0 {
        return false;
    }
    // Accessibility queries make Chromium/Electron apps build their accessibility tree (slows
    // them down); they get the original 28 pt rule instead.
    let use_accessibility = !shared.apps.is_chromium(window.pid);
    let result = WindowDragArea::is_drag_area(point, window, use_accessibility);
    shared.remember_window_drag(point, result);
    if result {
        emit_status(
            &shared.hooks,
            &format!("window drag area at {},{} on {}", point.x as i64, point.y as i64, window.owner_name),
        );
    }
    result
}

/// The panel-point form the recognizer's `drag_zone` calls: mapped onto the display first.
fn starts_window_drag_panel(shared: &Shared, p: PanelPoint) -> bool {
    let Some(mapper) = shared.mapper() else { return false };
    let s = mapper.clamped_point(p.x, p.y);
    let windows = WindowLocator::on_screen_windows();
    starts_window_drag(shared, CGPoint::new(s.x, s.y), &windows)
}

/// The Swift `\(kind)` of a dock swipe: the case's name.
fn kind_name(kind: DockSwipeKind) -> &'static str {
    match kind {
        DockSwipeKind::Horizontal => "horizontal",
        DockSwipeKind::Vertical => "vertical",
        DockSwipeKind::Pinch => "pinch",
    }
}

/// The Swift `\(kind)` of a gesture kind, as an enum with one associated value prints there.
fn gesture_kind_name(kind: GestureKind) -> String {
    match kind {
        GestureKind::Tap => "tap".to_string(),
        GestureKind::RightClick => "rightClick".to_string(),
        GestureKind::Drag { hold } => format!("drag(hold: {hold})"),
        GestureKind::Scroll => "scroll".to_string(),
        GestureKind::Magnify => "magnify".to_string(),
        GestureKind::Rotate => "rotate".to_string(),
    }
}

/// The touch display: the online display whose EDID identity is `vendor`/`model` or, when none
/// matches, the only external display. The flag says whether the fallback was taken. `None` when
/// there is no such display.
///
/// The one home of this lookup: the pipeline drives what it answers, and the tools (the Event
/// Sink window, the system-UI self-test) open on the same display by calling it with
/// `PipelineSettings::default().display_vendor` and `.display_model`.
pub fn locate_touch_display(vendor: u32, model: u32) -> Option<(u32, bool)> {
    let ids = SystemDisplays::online();
    let matched = ids
        .iter()
        .copied()
        .find(|&id| CGDisplayVendorNumber(id) == vendor && CGDisplayModelNumber(id) == model);
    if let Some(id) = matched {
        return Some((id, false));
    }
    // An unlisted panel (the JN-MD-IPST105FP before its EDID numbers are pinned here): with
    // exactly one external display there is nothing to confuse the touch display with.
    let external: Vec<u32> = ids.into_iter().filter(|&id| !CGDisplayIsBuiltin(id)).collect();
    if let [only] = external.as_slice() {
        return Some((*only, true));
    }
    None
}

/// The pipeline's core: the dock-swipe synthesis, the Dock push, the focus keeper and TUIO around
/// the recognizer, all on one thread. Every method takes `now_ns`; the machine never reads the
/// clock itself.
struct Pipeline {
    hooks: Arc<Mutex<PipelineHooks>>,
    shared: Arc<Shared>,
    source: HidTouchSource,
    deliverer: Deliverer<SystemDeliveryEffects>,
    dock_swipe: DockSwipe,
    settings: PipelineSettings,
    /// The panel's reports as contact events, repaired.
    stream: ContactStream,
    recognizer: GestureRecognizer,
    /// Every finger on the panel right now, in panel coordinates.
    contacts: BTreeMap<u8, PanelPoint>,
    touch_count: usize,
    max_touch_count: usize,
    panel_mode: PanelModeWatch,
    /// When the last digitizer frame arrived; mouse-emulation frames newer than this are dropped.
    last_digitizer_frame_ns: u64,
    /// Report census: settles from the log alone whether the panel is streaming digitizer
    /// multitouch or mouse emulation. The counts never reset; the window moves.
    digitizer_reports: usize,
    mouse_reports: usize,
    /// When the current census window began.
    census_at_ns: u64,
    census_baseline: (usize, usize),
    /// The touch display's UUID, as the window server names displays in its Space state.
    touch_display_uuid: Option<String>,
    /// The dock swipe between its `began` and its `ended`/`cancelled`.
    open_dock_swipe: Option<DockSwipeKind>,
    swipe_hold: DockSwipeHold,
    /// Every display's Space when a Space switch began, for the result line a second later.
    #[cfg(feature = "debug")]
    spaces_at_swipe_start: Vec<(String, u64)>,
    /// This pinch parked the cursor to bring the Dock over (so its end gives the cursor back).
    dock_brought_for_pinch: bool,
    /// A push of the hidden cursor against a bottom edge, moving the Dock; `None` when none is
    /// running.
    dock_push: Option<DockPush>,
    /// "Bring the Dock over, then back": where the Dock came from, and the watch that says when
    /// to return it.
    dock_return: Option<(DockReturnWatch, ScreenRect)>,
    /// The next poll of the Dock return watch (the Swift `dockReturnTimer`).
    dock_return_deadline: Option<u64>,
    /// Between `start` and `stop`: only a running pipeline moves the Dock.
    running: bool,
    /// The touch mode in effect (a change ends whatever is under way first).
    active_input_mode: InputMode,
    /// The UDP sender, created lazily like the Swift `lazy var`.
    tuio: Option<TUIOSender>,
    tuio_sessions: TUIOSessions,
    tuio_sequence: i32,
    /// When the last TUIO frame went out; the idle frame is due one second later.
    tuio_last_frame_ns: u64,
    /// The app that had focus on another display when a Space switch began on the touch display.
    focus_before_switch: Option<i32>,
    /// Watching for macOS to move focus after that switch, and when its next poll falls due (the
    /// Swift `asyncAfter(0.05)` while waiting). A new touch cancels it.
    focus_keeper: Option<(FocusKeeper, u64)>,
    /// Diagnostic lines scheduled one second out (the Swift `asyncAfter(1.0)` continuations).
    #[cfg(feature = "debug")]
    pending_statuses: Vec<PendingStatus>,
}

/// A push of the hidden cursor against a display's bottom edge, moving the Dock.
///
/// The Swift guarded its scheduled steps with a generation counter; none is needed here, because
/// each push carries its own deadline and continuation, so replacing or clearing `dock_push`
/// takes both away with it and a stale step cannot run.
struct DockPush {
    /// A back-push is interrupted by a touch; a bring-over push is not.
    back: bool,
    display: ScreenRect,
    edge: CGPoint,
    started_ns: u64,
    step: usize,
    /// When the next nudge and check fall due (the Swift `asyncAfter(0.015)`).
    next_step_ns: u64,
    /// Which continuation runs when the push ends (the Swift's two escaping closures).
    done: DockPushDone,
}

/// Which continuation runs when a Dock push ends.
enum DockPushDone {
    /// "Bring the Dock over": report, park the cursor off the bottom edge, release the held
    /// swipe, and arm the return watch if the pinch asked for it.
    Bring { at: CGPoint, plan: Plan, home: Option<ScreenRect> },
    /// "Push it back": report and release the cursor.
    Back,
}

/// A diagnostic line scheduled one second out.
#[cfg(feature = "debug")]
struct PendingStatus {
    due_ns: u64,
    outcome: PendingOutcome,
}

/// What the scheduled line reports.
#[cfg(feature = "debug")]
enum PendingOutcome {
    /// Where Notification Center actually opened.
    NotificationCenter,
    /// Where a dock swipe left the system UI.
    DockSwipe { kind: DockSwipeKind, spaces_before: Vec<(String, u64)> },
}

/// The log of the deliverer and of the dock swipe: the status hook.
#[cfg(feature = "debug")]
fn forward_to_status(hooks: &Arc<Mutex<PipelineHooks>>) -> LogSink {
    let hooks = Arc::clone(hooks);
    Box::new(move |message: &str| emit_status(&hooks, message))
}

impl Pipeline {
    fn new(hooks: Arc<Mutex<PipelineHooks>>, shared: Arc<Shared>) -> Self {
        let deliverer = Deliverer::new(SystemDeliveryEffects::new());
        let dock_swipe = DockSwipe::new();
        // Deliverer and DockSwipe log through the status hook, as the Swift forwarded to onStatus.
        // Their lines are for the log, so only a build with one asks for them.
        #[cfg(feature = "debug")]
        let (deliverer, dock_swipe) = (
            deliverer.logging_to(forward_to_status(&hooks)),
            dock_swipe.logging_to(forward_to_status(&hooks)),
        );
        let settings = PipelineSettings::default();
        Self {
            hooks,
            shared,
            source: HidTouchSource::new(),
            deliverer,
            dock_swipe,
            stream: ContactStream::for_panel(settings.panel_width_mm, settings.panel_height_mm),
            settings,
            recognizer: GestureRecognizer::new(GestureConfig::default()),
            contacts: BTreeMap::new(),
            touch_count: 0,
            max_touch_count: 0,
            panel_mode: PanelModeWatch::new(),
            last_digitizer_frame_ns: 0,
            digitizer_reports: 0,
            mouse_reports: 0,
            census_at_ns: 0,
            census_baseline: (0, 0),
            touch_display_uuid: None,
            open_dock_swipe: None,
            swipe_hold: DockSwipeHold::new(),
            #[cfg(feature = "debug")]
            spaces_at_swipe_start: Vec::new(),
            dock_brought_for_pinch: false,
            dock_push: None,
            dock_return: None,
            dock_return_deadline: None,
            running: false,
            active_input_mode: InputMode::Gestures,
            tuio: None,
            tuio_sessions: TUIOSessions::new(),
            tuio_sequence: 0,
            tuio_last_frame_ns: 0,
            focus_before_switch: None,
            focus_keeper: None,
            #[cfg(feature = "debug")]
            pending_statuses: Vec::new(),
        }
    }

    /// Applies one command. Returns true when the thread should leave (`Shutdown`).
    fn command(&mut self, command: Command, now_ns: u64) -> bool {
        match command {
            Command::Panel(PanelEvent::Status(message)) => self.status(&message),
            Command::Panel(PanelEvent::Report(report)) => {
                self.report(&report.bytes, report.timestamp_ns, now_ns);
            }
            Command::Update(change) => self.apply_settings(change, now_ns),
            Command::ReturnDockHome => {
                if self.running && self.settings.launchpad_dock == LaunchpadDockMode::BringDockAndReturn {
                    self.send_dock_home_if_on_touch_display(now_ns);
                }
            }
            Command::DisplaysChanged => self.refresh_display(now_ns),
            Command::Start { panel } => self.start(panel, now_ns),
            Command::Stop(ack) => {
                self.stop(now_ns);
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            Command::Shutdown => {
                self.stop(now_ns);
                return true;
            }
        }
        false
    }

    /// Seizes the panel and begins delivering: fresh gesture state, dry-run flags copied, the
    /// display looked up, and the source streaming into the command channel.
    fn start(&mut self, panel: Sender<Command>, now_ns: u64) {
        self.reset_gesture_state(now_ns);
        self.dock_swipe.dry_run = self.settings.dry_run;
        self.deliverer.dry_run = self.settings.dry_run;
        self.deliverer.release_stuck_press(now_ns);
        // So the display line is reported even if settings changed before start.
        self.shared.set_mapper(None);
        self.refresh_display(now_ns);
        self.census_at_ns = now_ns;
        self.running = true;
        self.source.start(move |event| {
            let _ = panel.send(Command::Panel(event));
        });
    }

    /// Releases the panel and closes whatever gesture is open.
    fn stop(&mut self, now_ns: u64) {
        // First, so no frame can open a new gesture while we close the current one.
        self.source.stop();
        self.running = false;
        self.deliverer.abort_gesture();
        self.reset_gesture_state(now_ns);
    }

    // MARK: input

    /// One HID report: counted for the census, dropped while the panel's mouse emulation trails
    /// the digitizer, checked for the panel's mode, then filtered and turned into contact events.
    fn report(&mut self, bytes: &[u8], timestamp_ns: u64, now_ns: u64) {
        #[cfg(feature = "debug")]
        self.hooks_report(bytes, timestamp_ns);
        let Some(frame) = ReportParser::parse(bytes) else { return };
        let source = frame.source;
        let now = timestamp_ns;
        if source == Source::Digitizer {
            self.last_digitizer_frame_ns = now;
            self.digitizer_reports += 1;
            if self.digitizer_reports == 1 {
                self.status("panel report: first digitizer frame");
            }
        } else {
            self.mouse_reports += 1;
            if self.mouse_reports == 1 {
                self.status("panel report: first mouse frame");
            }
        }
        self.run_census(now_ns);
        // Once digitizer frames flow, the panel's mouse emulation is trailing off: ignore it, so
        // one finger is not delivered twice (as a digitizer contact and as contact id 0).
        if source == Source::Mouse
            && now.wrapping_sub(self.last_digitizer_frame_ns) < MOUSE_ECHO_WINDOW_NS
        {
            return;
        }
        match self.panel_mode.observe(source, now) {
            Some(ModeAction::RequestMultitouch) => {
                self.status("panel is sending single-touch (mouse) reports; asking it for multitouch");
                self.source.request_multitouch_mode();
            }
            Some(ModeAction::MultitouchRestored) => {
                self.status("panel sends multitouch reports again");
            }
            None => {}
        }
        let events = self.stream.report(frame, now);
        #[cfg(feature = "debug")]
        self.trace_contacts(&events, "with a report");
        self.process(events, now);
    }

    /// The report census (the Swift timer's 10 s line): when a window of 10 s or more has closed,
    /// log the counts if any report was counted and start a new window.
    fn run_census(&mut self, now_ns: u64) {
        if self.census_at_ns == 0 {
            // The first report begins the first window.
            self.census_at_ns = now_ns;
            return;
        }
        if now_ns.wrapping_sub(self.census_at_ns) < CENSUS_INTERVAL_NS {
            return;
        }
        let digitizer = self.digitizer_reports - self.census_baseline.0;
        let mouse = self.mouse_reports - self.census_baseline.1;
        if digitizer + mouse > 0 {
            self.status(&format!("panel reports (10 s): {digitizer} digitizer, {mouse} mouse"));
        }
        self.census_baseline = (self.digitizer_reports, self.mouse_reports);
        self.census_at_ns = now_ns;
    }

    /// Contact events: refresh the finger map, tell the hooks, cancel a focus watch on a new
    /// touch, interrupt a Dock back-push on a touch, then recognize and route.
    fn process(&mut self, events: SmallVec<[ContactEvent; 16]>, now_ns: u64) {
        if events.is_empty() {
            return;
        }
        for event in &events {
            if event.phase == ContactPhase::Ended {
                self.contacts.remove(&event.id);
            } else {
                self.contacts.insert(event.id, PanelPoint::new(event.x, event.y));
            }
        }
        self.hooks_contacts();
        if self.focus_keeper.is_some() && events.iter().any(|event| event.phase == ContactPhase::Began) {
            // A new touch decides focus itself.
            self.focus_keeper = None;
        }
        if !self.contacts.is_empty()
            && self.dock_push.as_ref().is_some_and(|push| push.back)
            && let Some(home) = self.dock_return.map(|(_, home)| home)
        {
            // A touch needs the cursor: stop pushing the Dock back, and try again once the
            // fingers lift.
            self.dock_push = None;
            self.deliverer.end(now_ns, WARP_RETURN_DELAY_NS);
            self.watch_for_dock_return(home, now_ns);
        }
        if self.contacts.len() != self.touch_count {
            self.touch_count = self.contacts.len();
            self.max_touch_count = self.max_touch_count.max(self.touch_count);
            self.hooks_touch_count();
        }
        if self.active_input_mode == InputMode::Multitouch {
            // Every finger goes to multitouch apps; nothing reaches macOS as clicks or gestures.
            // The map is lent out and put back, so the per-frame path allocates nothing.
            let fingers = std::mem::take(&mut self.contacts);
            self.send_tuio(&fingers, now_ns);
            self.contacts = fingers;
            return;
        }
        let actions = self.recognizer.handle(&events, now_ns);
        for action in actions {
            self.route(action, now_ns);
        }
    }

    // MARK: deadlines

    /// The earliest deadline at which this machine changes state with no further input.
    fn next_deadline_ns(&self) -> Option<u64> {
        let mut earliest: Option<u64> = None;
        let mut offer = |deadline: Option<u64>| {
            if let Some(deadline) = deadline
                && earliest.is_none_or(|known| deadline < known)
            {
                earliest = Some(deadline);
            }
        };
        offer(self.stream.next_deadline_ns());
        offer(self.deliverer.next_deadline_ns());
        offer(self.dock_swipe.next_deadline_ns());
        offer(self.dock_push.as_ref().map(|push| push.next_step_ns));
        offer(self.dock_return_deadline);
        offer(self.focus_keeper.as_ref().map(|(_, due_ns)| *due_ns));
        #[cfg(feature = "debug")]
        offer(self.pending_statuses.iter().map(|pending| pending.due_ns).min());
        if self.active_input_mode == InputMode::Multitouch && self.contacts.is_empty() {
            offer(Some(self.tuio_last_frame_ns.saturating_add(TUIO_IDLE_NS)));
        }
        earliest
    }

    /// Performs whatever is due, earliest deadline first, until nothing is due any more.
    fn tick(&mut self, now_ns: u64) {
        while let Some((_, due)) = self.due_work(now_ns) {
            self.run_due(due, now_ns);
        }
    }

    /// The earliest piece of work due at or before `now_ns`, with its kind.
    fn due_work(&self, now_ns: u64) -> Option<(u64, Due)> {
        let mut best: Option<(u64, Due)> = None;
        let mut offer = |deadline: Option<u64>, due: Due| {
            if let Some(deadline) = deadline
                && deadline <= now_ns
                && best.is_none_or(|(known, _)| deadline < known)
            {
                best = Some((deadline, due));
            }
        };
        offer(self.stream.next_deadline_ns(), Due::Contacts);
        offer(self.deliverer.next_deadline_ns(), Due::Deliverer);
        offer(self.dock_swipe.next_deadline_ns(), Due::DockSwipe);
        offer(self.dock_push.as_ref().map(|push| push.next_step_ns), Due::DockPush);
        offer(self.dock_return_deadline, Due::DockReturn);
        offer(self.focus_keeper.as_ref().map(|(_, due_ns)| *due_ns), Due::Focus);
        #[cfg(feature = "debug")]
        offer(self.pending_statuses.iter().map(|pending| pending.due_ns).min(), Due::Status);
        if self.active_input_mode == InputMode::Multitouch && self.contacts.is_empty() {
            offer(Some(self.tuio_last_frame_ns.saturating_add(TUIO_IDLE_NS)), Due::TuioIdle);
        }
        best
    }

    /// Runs one piece of due work.
    fn run_due(&mut self, due: Due, now_ns: u64) {
        match due {
            Due::Contacts => {
                let events = self.stream.expire(now_ns);
                #[cfg(feature = "debug")]
                self.trace_contacts(&events, "on the timer");
                self.process(events, now_ns);
            }
            Due::Deliverer => self.deliverer.tick(now_ns),
            Due::DockSwipe => self.dock_swipe.tick(now_ns),
            Due::DockPush => self.push_dock_step(now_ns),
            Due::DockReturn => self.poll_dock_return(now_ns),
            Due::Focus => self.poll_focus(now_ns),
            #[cfg(feature = "debug")]
            Due::Status => {
                let earliest = self
                    .pending_statuses
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, pending)| pending.due_ns)
                    .map(|(index, _)| index);
                if let Some(index) = earliest {
                    let pending = self.pending_statuses.swap_remove(index);
                    self.run_pending_status(pending);
                }
            }
            Due::TuioIdle => {
                // TUIO clients expect a frame now and then even while nothing touches; sending
                // moves the idle deadline one second ahead.
                self.send_tuio(&BTreeMap::new(), now_ns);
            }
        }
    }

    // MARK: routing

    fn route(&mut self, action: GestureAction, now_ns: u64) {
        let Some(mapper) = self.shared.mapper() else { return };
        match action {
            GestureAction::Tap { at, count } => {
                let p = self.point(&mapper, at);
                self.begin(GestureKind::Tap, p);
                self.deliverer.click(p, i64::from(count), CGMouseButton::Left);
                self.deliverer.end(now_ns, WARP_RETURN_DELAY_NS);
            }
            GestureAction::RightClick { at } => {
                let p = self.point(&mapper, at);
                self.begin(GestureKind::RightClick, p);
                self.deliverer.click(p, 1, CGMouseButton::Right);
                self.deliverer.end(now_ns, WARP_RETURN_DELAY_NS);
            }
            GestureAction::DragBegan { at, hold } => {
                let p = self.point(&mapper, at);
                self.begin(GestureKind::Drag { hold }, p);
                self.deliverer.drag_down(p);
            }
            GestureAction::DragMoved { to } => {
                let to = self.point(&mapper, to);
                self.deliverer.drag_move(to);
            }
            GestureAction::DragEnded { at } => {
                let p = self.point(&mapper, at);
                self.deliverer.drag_up(p);
                self.deliverer.end(now_ns, WARP_RETURN_DELAY_NS);
            }
            GestureAction::Scroll { dx, dy, phase } => {
                if phase == GesturePhase::Began {
                    let centroid = self.centroid(&mapper);
                    self.begin(GestureKind::Scroll, centroid);
                    // Two fingers, like the trackpad: the page swipe tracks them. One-finger pans
                    // stay plain scrolls.
                    self.deliverer.scroll_gesture_stream =
                        self.settings.swipe_between_pages && self.contacts.len() >= 2;
                }
                let d = self.delta(&mapper, dx, dy);
                self.deliverer.scroll(d.x, d.y, phase);
            }
            GestureAction::ScrollEnded { velocity_x, velocity_y } => {
                let v = self.delta(&mapper, velocity_x, velocity_y);
                self.deliverer.scroll_ended(v.x, v.y, now_ns);
            }
            GestureAction::Magnify { delta, phase } => {
                if phase == GesturePhase::Began {
                    let centroid = self.centroid(&mapper);
                    self.begin(GestureKind::Magnify, centroid);
                }
                self.deliverer.magnify(delta, phase, now_ns);
            }
            GestureAction::Rotate { degrees, phase } => {
                if phase == GesturePhase::Began {
                    let centroid = self.centroid(&mapper);
                    self.begin(GestureKind::Rotate, centroid);
                }
                self.deliverer.rotate(degrees, phase, now_ns);
            }
            GestureAction::DockSwipe { kind, delta, phase } => {
                if phase == GesturePhase::Began {
                    let centroid = self.centroid(&mapper);
                    self.begin_dock_swipe(kind, delta, centroid, now_ns);
                }
                for step in self.swipe_hold.pass(delta, phase) {
                    self.post_dock_swipe(kind, step, now_ns);
                }
                if phase == GesturePhase::Ended {
                    self.end_dock_swipe(kind, now_ns);
                }
            }
            GestureAction::DockSwipeCancelled { .. } => {
                self.close_dock_swipe(now_ns);
                self.status("dock swipe cancelled (palm)");
            }
            GestureAction::NotificationCenter => self.open_notification_center(now_ns),
        }
    }

    /// Notification Center opens on the display that has the cursor, whichever clock opened it,
    /// so the hidden cursor waits on the touch display while the clock is pressed
    /// (`--system-ui-selftest`, nc-2).
    fn open_notification_center(&mut self, now_ns: u64) {
        if self.settings.dry_run {
            self.status("dry-run notification center");
            return;
        }
        let mapper = self.shared.mapper();
        let clock = NotificationCenterOpener::clock();
        let (Some(mapper), Some(clock)) = (mapper, clock) else {
            self.status("notification center → clock not found");
            return;
        };
        let p = if self.contacts.is_empty() {
            CGPoint::new(
                mapper.bounds.x + mapper.bounds.width / 2.0,
                mapper.bounds.y + mapper.bounds.height / 2.0,
            )
        } else {
            self.centroid(&mapper)
        };
        self.deliverer.begin(Target::Warp, p, None, false);
        let pressed = NotificationCenterOpener::press(&clock);
        self.deliverer.end(now_ns, SYSTEM_GESTURE_CURSOR_DELAY_NS);
        self.status(if pressed {
            "notification center → clock pressed, cursor on the touch display"
        } else {
            "notification center → pressing the clock failed"
        });
        #[cfg(feature = "debug")]
        self.pending_statuses.push(PendingStatus {
            due_ns: now_ns.saturating_add(OUTCOME_DELAY_NS),
            outcome: PendingOutcome::NotificationCenter,
        });
    }

    // MARK: system gestures

    /// A Space switch acts on the display with the cursor, whichever display has the active menu
    /// bar (`--system-ui-selftest --spaces`, 2026-10-11): the hidden cursor waits on the touch
    /// display. Launchpad opens on the display with the Dock (see `bring_dock_if_needed`).
    /// Mission Control and App Exposé cover every display.
    fn begin_dock_swipe(&mut self, kind: DockSwipeKind, delta: f64, at: CGPoint, now_ns: u64) {
        // A previous pinch still waiting for the Dock never reached the system: drop it and its
        // push.
        self.drop_held_swipe();
        if self.dock_push.as_ref().is_some_and(|push| !push.back) {
            self.dock_push = None;
        }
        self.open_dock_swipe = Some(kind);
        self.dock_brought_for_pinch = false;
        let active_bar = SystemDisplays::active_menu_bar_display_uuid();
        let name = self.display_name(active_bar.as_deref());
        let swipe = kind_name(kind);
        self.status(&format!("{swipe} dock swipe → system (active menu bar on {name})"));
        if self.settings.dry_run {
            return;
        }
        match kind {
            DockSwipeKind::Horizontal => {
                #[cfg(feature = "debug")]
                {
                    self.spaces_at_swipe_start = SystemDisplays::current_spaces();
                }
                // macOS moves focus to the switched display afterwards; focus on another display
                // is kept.
                self.focus_before_switch = if self.settings.keep_focus_on_space_switch
                    && active_bar.is_some()
                    && active_bar.as_deref() != self.touch_display_uuid.as_deref()
                {
                    apps::frontmost().map(|app| app.processIdentifier())
                } else {
                    None
                };
                self.deliverer.begin(Target::Warp, at, None, false);
            }
            DockSwipeKind::Pinch => self.bring_dock_if_needed(delta > 0.0, at, now_ns),
            DockSwipeKind::Vertical => {}
        }
    }

    // MARK: Launchpad and the Dock

    /// Launchpad opens on the display with the Dock. With "bring the Dock over", a pinch-in on
    /// the touch display first pushes the hidden cursor against the touch display's bottom edge
    /// until the Dock is there (the swipe is held meanwhile), as moving the Dock by hand does.
    fn bring_dock_if_needed(&mut self, pinch_inward: bool, at: CGPoint, now_ns: u64) {
        let Some(mapper) = self.shared.mapper() else { return };
        if !DockLocator::is_at_bottom() || self.settings.launchpad_dock == LaunchpadDockMode::Leave {
            return;
        }
        let Some(dock) = DockLocator::anchor() else { return }; // Dock position unknown: leave it alone
        let plan = LaunchpadDock::plan(self.settings.launchpad_dock, pinch_inward, mapper.bounds.contains(dock));
        let home = self.dock_home(dock, mapper.bounds);
        if !plan.bring_dock {
            // Already here: it still goes back after.
            if plan.return_dock && let Some(home) = home {
                self.watch_for_dock_return(home, now_ns);
            }
            return;
        }
        self.dock_brought_for_pinch = true;
        self.swipe_hold.start();
        self.status("launchpad: bringing the Dock to the touch display first");
        self.push_dock(
            mapper.bounds,
            false,
            DockPushDone::Bring { at, plan, home },
            now_ns,
        );
    }

    /// Where "then back" returns the Dock: found with [`LaunchpadDock::home`] over the online
    /// displays and the main one.
    fn dock_home(&self, dock: ScreenPoint, touch: ScreenRect) -> Option<ScreenRect> {
        let displays: Vec<ScreenRect> =
            SystemDisplays::online().into_iter().map(SystemDisplays::rect).collect();
        LaunchpadDock::home(dock, touch, &displays, Some(SystemDisplays::rect(CGMainDisplayID())))
    }

    /// Choosing "then back" means the Dock lives off the touch display: one found there goes home
    /// once Launchpad is closed and no finger is on the panel (about 2 s later).
    fn send_dock_home_if_on_touch_display(&mut self, now_ns: u64) {
        let Some(mapper) = self.shared.mapper() else { return };
        if self.settings.dry_run || !DockLocator::is_at_bottom() {
            return;
        }
        let Some(dock) = DockLocator::anchor() else { return };
        if !mapper.bounds.contains(dock) {
            return;
        }
        let Some(home) = self.dock_home(dock, mapper.bounds) else { return };
        self.status("launchpad then back: the Dock is on the touch display; sending it home");
        self.watch_for_dock_return(home, now_ns);
    }

    /// Pushes the hidden cursor against `display`'s bottom edge until the Dock is there or the
    /// time is up, checking about every 45 ms, then runs `done` (the push's continuation).
    fn push_dock(&mut self, display: ScreenRect, back: bool, done: DockPushDone, now_ns: u64) {
        let edge = CGPoint::new(display.x + display.width / 2.0, display.y + display.height - 1.0);
        self.dock_push = Some(DockPush {
            back,
            display,
            edge,
            started_ns: now_ns,
            step: 0,
            next_step_ns: now_ns,
            done,
        });
        self.deliverer.begin(Target::Warp, edge, None, false);
        // The first nudge goes out at once (Swift's synchronous first `pushDockStep`).
        self.push_dock_step(now_ns);
    }

    /// One step of a Dock push: check for arrival (every third step), give up at the timeout, or
    /// nudge and schedule the next step 15 ms out.
    fn push_dock_step(&mut self, now_ns: u64) {
        let Some(push) = self.dock_push.as_ref() else { return };
        // The generation guard is implicit: this runs only while this very push is on the struct.
        let display = push.display;
        let edge = push.edge;
        let started_ns = push.started_ns;
        let step = push.step;
        let waited_ms = now_ns.saturating_sub(started_ns) as f64 / 1e6;
        let arrived = step % 3 == 2 && DockLocator::is_dock(display) == Some(true);
        if arrived || waited_ms >= DOCK_PUSH_TIMEOUT_MS {
            self.dock_push = None;
            self.dock_push_finished(arrived, waited_ms, now_ns);
            return;
        }
        self.deliverer.nudge(edge, 0, 4);
        if let Some(push) = self.dock_push.as_mut() {
            push.step = step + 1;
            push.next_step_ns = now_ns.saturating_add(DOCK_PUSH_STEP_NS);
        }
    }

    /// The push ended (the Dock arrived or the time ran out): run the push's continuation.
    fn dock_push_finished(&mut self, arrived: bool, waited_ms: f64, now_ns: u64) {
        let Some(push) = self.dock_push.take() else { return };
        match push.done {
            DockPushDone::Bring { at, plan, home } => {
                let message = if arrived {
                    format!("Dock on the touch display after {} ms", waited_ms as i64)
                } else {
                    format!("Dock did not reach the touch display within {} ms", waited_ms as i64)
                };
                self.status(&message);
                // Off the bottom edge, so the auto-hidden Dock slides away again behind
                // Launchpad.
                self.deliverer.begin(Target::Warp, at, None, false);
                for step in self.swipe_hold.release() {
                    self.post_dock_swipe(DockSwipeKind::Pinch, step, now_ns);
                }
                if self.open_dock_swipe.is_none() {
                    // Fingers already lifted.
                    self.deliverer.end(now_ns, SYSTEM_GESTURE_CURSOR_DELAY_NS);
                }
                if arrived && plan.return_dock && let Some(home) = home {
                    self.watch_for_dock_return(home, now_ns);
                }
            }
            DockPushDone::Back => {
                self.dock_return = None;
                self.deliverer.end(now_ns, WARP_RETURN_DELAY_NS);
                let message = if arrived {
                    format!("Dock back after {} ms", waited_ms as i64)
                } else {
                    format!("Dock did not go back within {} ms", waited_ms as i64)
                };
                self.status(&message);
            }
        }
    }

    /// "Bring the Dock over, then back": checks every 250 ms whether Launchpad has closed.
    fn watch_for_dock_return(&mut self, home: ScreenRect, now_ns: u64) {
        self.dock_return = Some((DockReturnWatch::new(now_ns), home));
        if self.dock_return_deadline.is_none() {
            self.dock_return_deadline = Some(now_ns.saturating_add(DOCK_RETURN_POLL_NS));
        }
    }

    fn poll_dock_return(&mut self, now_ns: u64) {
        if self.dock_push.is_some() {
            // A push owns the cursor; the watch waits (the Swift timer kept firing all the same).
            self.dock_return_deadline = Some(now_ns.saturating_add(DOCK_RETURN_POLL_NS));
            return;
        }
        let Some((mut watch, home)) = self.dock_return else {
            self.dock_return_deadline = None;
            return;
        };
        let launchpad_open = !DockLocator::launchpad_windows().is_empty();
        let fingers_down = !self.contacts.is_empty();
        match watch.poll(launchpad_open, fingers_down, now_ns) {
            DockReturnAction::Wait => {
                self.dock_return_deadline = Some(now_ns.saturating_add(DOCK_RETURN_POLL_NS));
            }
            DockReturnAction::Stop => {
                self.stop_dock_return();
                self.status("launchpad still open after 10 minutes: the Dock stays on the touch display");
            }
            DockReturnAction::PushBack => {
                self.stop_dock_return();
                self.status("launchpad closed: pushing the Dock back");
                // Kept, so a touch during the push can re-arm the watch.
                self.dock_return = Some((watch, home));
                self.push_dock(home, true, DockPushDone::Back, now_ns);
            }
        }
    }

    fn stop_dock_return(&mut self) {
        self.dock_return_deadline = None;
        self.dock_return = None;
    }

    /// One held-swipe step to the system: the swipe type mapped, the pinch inverted (before
    /// macOS 26 pinch dock swipes need the flag; verified on this Mac).
    fn post_dock_swipe(&mut self, kind: DockSwipeKind, step: HoldStep, now_ns: u64) {
        let phase = match step.phase {
            GesturePhase::Began => SwipePhase::Began,
            GesturePhase::Changed => SwipePhase::Changed,
            GesturePhase::Ended => SwipePhase::Ended,
        };
        self.dock_swipe.post(step.delta, synth_kind(kind), phase, kind == DockSwipeKind::Pinch, now_ns);
    }

    fn end_dock_swipe(&mut self, kind: DockSwipeKind, now_ns: u64) {
        self.open_dock_swipe = None;
        if kind == DockSwipeKind::Vertical || self.settings.dry_run {
            return;
        }
        // A pinch still bringing the Dock over gives the cursor back when the Dock has arrived.
        if kind == DockSwipeKind::Horizontal
            || (self.dock_brought_for_pinch && self.dock_push.is_none())
        {
            self.deliverer.end(now_ns, SYSTEM_GESTURE_CURSOR_DELAY_NS);
        }
        if kind == DockSwipeKind::Horizontal && let Some(pid) = self.focus_before_switch {
            self.keep_focus(pid, now_ns);
        }
        self.focus_before_switch = None;
        #[cfg(feature = "debug")]
        {
            let spaces_before = self.spaces_at_swipe_start.clone();
            self.pending_statuses.push(PendingStatus {
                due_ns: now_ns.saturating_add(OUTCOME_DELAY_NS),
                outcome: PendingOutcome::DockSwipe { kind, spaces_before },
            });
        }
    }

    /// Cancels an open dock swipe (palm, stop, display lost) and gives the cursor back.
    fn close_dock_swipe(&mut self, now_ns: u64) {
        let held_back = self.drop_held_swipe();
        self.focus_before_switch = None;
        let Some(kind) = self.open_dock_swipe else { return };
        self.open_dock_swipe = None;
        if !held_back {
            self.dock_swipe.post(
                0.0,
                synth_kind(kind),
                SwipePhase::Cancelled,
                kind == DockSwipeKind::Pinch,
                now_ns,
            );
        }
        if kind == DockSwipeKind::Horizontal || self.dock_brought_for_pinch {
            self.dock_push = None;
            self.deliverer.end(now_ns, WARP_RETURN_DELAY_NS);
        }
    }

    /// Discards a swipe held back for the Dock. True if one was held.
    fn drop_held_swipe(&mut self) -> bool {
        self.swipe_hold.cancel()
    }

    /// Gives focus back to `pid` the first time macOS moves it away within 1.5 s
    /// ([`FocusKeeper`]). The first poll runs now; each one that has to wait sets the next.
    fn keep_focus(&mut self, pid: i32, now_ns: u64) {
        self.focus_keeper = Some((FocusKeeper::new(pid, now_ns), now_ns));
        self.poll_focus(now_ns);
    }

    fn poll_focus(&mut self, now_ns: u64) {
        // Taken out for the poll, and put back only while it waits: a watch that ended leaves
        // nothing behind.
        let Some((keeper, _)) = self.focus_keeper.take() else { return };
        let frontmost = apps::frontmost().map(|app| app.processIdentifier());
        match keeper.poll(frontmost, now_ns) {
            FocusAction::Wait => self.focus_keeper = Some((keeper, now_ns.saturating_add(FOCUS_POLL_NS))),
            FocusAction::Restore(pid) => {
                let app = apps::with_pid(pid);
                if let Some(app) = &app {
                    apps::activate(app);
                }
                let name = app
                    .as_ref()
                    .and_then(|app| app.localizedName())
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| format!("pid {pid}"));
                self.status(&format!("space switch: focus back to {name}"));
            }
            FocusAction::Stop => {}
        }
    }

    /// Runs a line that was scheduled one second out.
    #[cfg(feature = "debug")]
    fn run_pending_status(&mut self, pending: PendingStatus) {
        match pending.outcome {
            PendingOutcome::NotificationCenter => {
                let Some(mapper) = self.shared.mapper() else { return };
                let panels: Vec<&str> = NotificationCenterOpener::open_panels()
                    .iter()
                    .map(|rect| {
                        let centre = ScreenPoint {
                            x: rect.x + rect.width / 2.0,
                            y: rect.y + rect.height / 2.0,
                        };
                        if mapper.bounds.contains(centre) { "touch display" } else { "other display" }
                    })
                    .collect();
                let suffix = if panels.is_empty() {
                    "not open".to_string()
                } else {
                    format!("open on {}", panels.join(", "))
                };
                self.status(&format!("notification center result: {suffix}"));
            }
            PendingOutcome::DockSwipe { kind, spaces_before } => {
                self.log_outcome(kind, &spaces_before);
            }
        }
    }

    /// Where the system put the result (diagnostic log line, read-only).
    #[cfg(feature = "debug")]
    fn log_outcome(&mut self, kind: DockSwipeKind, spaces_before: &[(String, u64)]) {
        match kind {
            DockSwipeKind::Horizontal => {
                let changed: Vec<String> = SystemDisplays::current_spaces()
                    .into_iter()
                    .filter(|(uuid, space)| match spaces_before.iter().find(|(before, _)| before == uuid) {
                        Some((_, before)) => before != space,
                        // A display that gained Spaces since the swipe began counts as changed.
                        None => true,
                    })
                    .map(|(uuid, _)| self.display_name(Some(&uuid)))
                    .collect();
                let suffix = if changed.is_empty() {
                    "no Space changed".to_string()
                } else {
                    format!("Space changed on {}", changed.join(", "))
                };
                self.status(&format!("space switch result: {suffix}"));
            }
            DockSwipeKind::Pinch => {
                let Some(mapper) = self.shared.mapper() else { return };
                let overlays: Vec<String> = WindowLocator::on_screen_windows()
                    .into_iter()
                    .filter(|window| window.owner_name == "Dock" && window.layer > 0)
                    .map(|window| {
                        let centre = ScreenPoint {
                            x: window.bounds.x + window.bounds.width / 2.0,
                            y: window.bounds.y + window.bounds.height / 2.0,
                        };
                        let on = if mapper.bounds.contains(centre) { "touch display" } else { "other display" };
                        format!("{on} (layer {}, {}×{})", window.layer, window.bounds.width as i64, window.bounds.height as i64)
                    })
                    .collect();
                let suffix = if overlays.is_empty() { "none".to_string() } else { overlays.join(", ") };
                self.status(&format!("pinch result: Dock windows on {suffix}"));
            }
            DockSwipeKind::Vertical => {}
        }
    }

    /// How a display UUID reads in a status line: the touch display, another one, or unknown.
    fn display_name(&self, uuid: Option<&str>) -> String {
        let Some(uuid) = uuid else { return "unknown display".to_string() };
        if Some(uuid) == self.touch_display_uuid.as_deref() {
            "touch display".to_string()
        } else {
            "other display".to_string()
        }
    }

    // MARK: delivery

    /// Picks Direct or Warp once, at the start of the gesture.
    fn begin(&mut self, kind: GestureKind, p: CGPoint) {
        let windows = WindowLocator::on_screen_windows();
        let at = screen_point(p);
        let window = WindowHitTester::top_window(at, &windows, self.shared.own_pid).cloned();
        let app_override = window
            .as_ref()
            .and_then(|window| self.shared.apps.bundle_id(window.pid))
            .and_then(|bundle_id| self.settings.app_overrides.get(&bundle_id).copied());
        let context = DeliveryContext::new(
            kind,
            window.clone(),
            at,
            WindowHitTester::popup_menu_visible(&windows),
            window.as_ref().map(|window| self.shared.apps.is_chromium(window.pid)).unwrap_or(false),
            app_override,
            kind == (GestureKind::Drag { hold: false }) && starts_window_drag(&self.shared, p, &windows),
        );
        let mut mode = self.settings.policy.decide(&context);
        if mode == DeliveryMode::Direct && !DirectTarget::is_available() {
            mode = DeliveryMode::Warp; // private call missing on this macOS
        }
        let activate = DeliveryPolicy::needs_activation(kind);
        if mode == DeliveryMode::Direct && let Some(window) = &window {
            let direct = DirectTarget {
                pid: window.pid,
                window: window.number,
                origin: CGPoint::new(window.bounds.x, window.bounds.y),
            };
            self.deliverer.begin(Target::Direct(direct), p, Some(window.pid), activate);
        } else {
            let app_pid = window.as_ref().map(|window| window.pid);
            self.deliverer.begin(Target::Warp, p, app_pid, activate);
        }
        let owner = window.as_ref().map(|window| window.owner_name.as_str()).unwrap_or("no window");
        let kind = gesture_kind_name(kind);
        let mode = mode.as_str();
        self.status(&format!("{kind} → {mode} on {owner}"));
        // The answers served this gesture's classification only (the Swift `defer`).
        self.shared.forget_window_drag_answers();
    }

    // MARK: geometry

    fn point(&self, mapper: &CoordinateMapper, p: PanelPoint) -> CGPoint {
        cg_point(mapper.clamped_point(p.x, p.y))
    }

    fn centroid(&self, mapper: &CoordinateMapper) -> CGPoint {
        if self.contacts.is_empty() {
            return CGPoint::new(0.0, 0.0);
        }
        let n = self.contacts.len() as f64;
        let x = self.contacts.values().map(|p| p.x).sum::<f64>() / n;
        let y = self.contacts.values().map(|p| p.y).sum::<f64>() / n;
        self.point(mapper, PanelPoint::new(x, y))
    }

    /// A panel-space delta in screen points (rotation-aware).
    fn delta(&self, mapper: &CoordinateMapper, dx: f64, dy: f64) -> CGPoint {
        let a = mapper.point(0.5, 0.5);
        let b = mapper.point(0.5 + dx, 0.5 + dy);
        CGPoint::new(b.x - a.x, b.y - a.y)
    }

    // MARK: display and settings

    /// Looks up the touch display by EDID identity (falling back to a single external display) and
    /// rebuilds the mapper when it moved.
    fn refresh_display(&mut self, now_ns: u64) {
        let Some((id, fell_back)) =
            locate_touch_display(self.settings.display_vendor, self.settings.display_model)
        else {
            self.touch_display_uuid = None;
            if self.shared.mapper().is_some() {
                // The monitor went away mid-use: close any open gesture now, it can't finish
                // normally.
                self.deliverer.abort_gesture();
                self.reset_gesture_state(now_ns);
                self.status("touch display not found");
            }
            self.shared.set_mapper(None);
            return;
        };
        if fell_back {
            self.status(&format!(
                "touch display: EDID {}/{} matched nothing; using the only external display",
                self.settings.display_vendor, self.settings.display_model
            ));
        }
        self.touch_display_uuid = SystemDisplays::uuid(id);
        let b = CGDisplayBounds(id);
        let next = CoordinateMapper::new(screen_rect(b), CGDisplayRotation(id));
        if Some(next) != self.shared.mapper() {
            self.status(&format!(
                "touch display at {},{} {}×{} rotation {}",
                b.origin.x as i64,
                b.origin.y as i64,
                b.size.width as i64,
                b.size.height as i64,
                next.rotation
            ));
        }
        self.shared.set_mapper(Some(next));
    }

    /// A settings change arrived: copy what the deliverer holds, re-apply the gesture settings,
    /// re-look the display up, and switch the touch mode when it changed.
    fn apply_settings(&mut self, change: SettingsChange, now_ns: u64) {
        change(&mut self.settings);
        self.deliverer.dry_run = self.settings.dry_run;
        self.dock_swipe.dry_run = self.settings.dry_run;
        self.deliverer.focus_mode = self.settings.focus;
        // The trackpad's momentum when its settings are followed, on otherwise.
        self.deliverer.momentum_enabled = self.settings.trackpad.is_none_or(|trackpad| trackpad.momentum);
        apply_gesture_settings(&self.settings, &mut self.recognizer);
        self.refresh_display(now_ns);
        if self.settings.input_mode != self.active_input_mode {
            self.switch_input_mode(self.settings.input_mode, now_ns);
        }
    }

    /// Ends a gesture or stroke under way, tells TUIO apps every finger is gone when leaving
    /// multitouch, and starts the new mode with a fresh recognizer.
    fn switch_input_mode(&mut self, mode: InputMode, now_ns: u64) {
        self.deliverer.abort_gesture();
        if self.active_input_mode == InputMode::Multitouch {
            self.send_tuio(&BTreeMap::new(), now_ns);
        }
        self.active_input_mode = mode;
        self.reset_gesture_state(now_ns);
        self.tuio_sessions = TUIOSessions::new();
        match mode {
            InputMode::Gestures => self.status("touch mode: gestures"),
            InputMode::Drawing => self.status("touch mode: drawing (the first finger draws, nothing else)"),
            InputMode::Multitouch => self.status(&format!(
                "touch mode: multitouch (every finger as TUIO to 127.0.0.1:{}, nothing else)",
                TUIOSender::PORT
            )),
        }
    }

    /// One TUIO frame with every finger on the panel (none: tells apps they've all lifted).
    fn send_tuio(&mut self, fingers: &BTreeMap<u8, PanelPoint>, now_ns: u64) {
        let cursors = self.tuio_sessions.update(fingers, now_ns);
        self.tuio_sequence = if self.tuio_sequence == i32::MAX { 1 } else { self.tuio_sequence + 1 };
        self.tuio_last_frame_ns = now_ns;
        if self.settings.dry_run {
            return;
        }
        let sender = self.tuio.get_or_insert_with(|| TUIOSender::new(TUIOSender::PORT));
        sender.send(&TUIO::frame(&cursors, self.tuio_sequence));
    }

    /// A fresh contact stream and recognizer (start, stop, display lost), keeping the settings.
    fn reset_gesture_state(&mut self, now_ns: u64) {
        self.stream = ContactStream::for_panel(self.settings.panel_width_mm, self.settings.panel_height_mm);
        let mut fresh = GestureRecognizer::new(GestureConfig::default());
        apply_gesture_settings(&self.settings, &mut fresh);
        // In scroll mode a one-finger pan that starts on a toolbar or title bar still moves the
        // window.
        let shared = Arc::clone(&self.shared);
        fresh.drag_zone = Some(Box::new(move |point: PanelPoint| starts_window_drag_panel(&shared, point)));
        self.recognizer = fresh;
        self.contacts.clear();
        self.shared.forget_window_drag_answers();
        self.close_dock_swipe(now_ns);
        self.dock_push = None;
        self.stop_dock_return();
        self.focus_keeper = None;
        if self.touch_count != 0 {
            self.touch_count = 0;
            self.hooks_touch_count();
        }
    }

    // MARK: hooks

    fn status(&self, message: &str) {
        emit_status(&self.hooks, message);
    }

    fn hooks_contacts(&self) {
        let mut hooks = self.hooks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hook) = &mut hooks.contacts {
            hook(&self.contacts);
        }
    }

    /// Where each finger began or ended, as the tracker reports it, with whether the events came
    /// with a report or from the timer. Only a debug build's log has these lines.
    #[cfg(feature = "debug")]
    fn trace_contacts(&self, events: &[ContactEvent], from: &str) {
        let Some(mapper) = self.shared.mapper() else { return };
        for event in events {
            let phase = match event.phase {
                ContactPhase::Began => "began",
                ContactPhase::Ended => "ended",
                ContactPhase::Moved => continue,
            };
            let at = self.point(&mapper, PanelPoint::new(event.x, event.y));
            self.status(&format!("contact {} {phase} at {},{} {from}", event.id, at.x as i64, at.y as i64));
        }
    }

    #[cfg(feature = "debug")]
    fn hooks_report(&self, bytes: &[u8], timestamp_ns: u64) {
        let mut hooks = self.hooks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hook) = &mut hooks.report {
            hook(bytes, timestamp_ns);
        }
    }

    fn hooks_touch_count(&self) {
        let mut hooks = self.hooks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hook) = &mut hooks.touch_count {
            hook(self.touch_count, self.max_touch_count);
        }
    }
}

/// What fell due: one entry of the deadline set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Due {
    /// The contact stream's own deadline: a lift report the panel has gone quiet after (or
    /// held for the debounce), or a press that has stopped reporting.
    Contacts,
    /// The deliverer's pending cursor returns and re-sends.
    Deliverer,
    /// The dock swipe's re-sent end pairs.
    DockSwipe,
    /// A Dock push's next nudge and check.
    DockPush,
    /// A poll of the Dock return watch.
    DockReturn,
    /// A poll of the focus keeper.
    Focus,
    /// A diagnostic result line, one second after a system gesture.
    #[cfg(feature = "debug")]
    Status,
    /// A TUIO idle frame, one second after the last frame.
    TuioIdle,
}

/// The dock swipe kind the synthesized events carry (the same three cases as the core's).
fn synth_kind(kind: DockSwipeKind) -> SwipeKind {
    match kind {
        DockSwipeKind::Horizontal => SwipeKind::Horizontal,
        DockSwipeKind::Vertical => SwipeKind::Vertical,
        DockSwipeKind::Pinch => SwipeKind::Pinch,
    }
}

/// The recognizer's thresholds and switches from the settings.
fn apply_gesture_settings(settings: &PipelineSettings, recognizer: &mut GestureRecognizer) {
    {
        let config = &mut recognizer.config;
        config.one_finger_drag = settings.one_finger_drag;
        config.three_finger_drag = settings.three_finger_drag;
        config.three_finger_anchor = settings.three_finger_anchor;
        config.drawing_mode = settings.input_mode == InputMode::Drawing;
        config.panel_width_mm = settings.panel_width_mm;
        config.panel_height_mm = settings.panel_height_mm;
    }
    match settings.trackpad {
        Some(trackpad) => trackpad.apply(&mut recognizer.config),
        None => recognizer.enable_all_two_finger_gestures(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Owner's choice 2026-10-11: Launchpad by touch borrows the Dock and gives it back.
    #[test]
    fn launchpad_brings_the_dock_over_then_back_by_default() {
        assert_eq!(PipelineSettings::default().launchpad_dock, LaunchpadDockMode::BringDockAndReturn);
    }

    // Owner's choice 2026-10-11: a Space switch by touch keeps focus on the other display, as a
    // setting.
    #[test]
    fn a_space_switch_keeps_focus_by_default() {
        assert!(PipelineSettings::default().keep_focus_on_space_switch);
    }

    // Owner's choice 2026-10-11: two-finger page swipes, on by default, with a menu switch.
    #[test]
    fn swipe_between_pages_is_on_by_default() {
        assert!(PipelineSettings::default().swipe_between_pages);
    }

    /// A pipeline that has not started, with a hook keeping the x of every set of fingers it
    /// reports, in the panel's units.
    fn machine() -> (Pipeline, Arc<Mutex<Vec<Vec<u16>>>>) {
        let fingers = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&fingers);
        let hooks = Arc::new(Mutex::new(PipelineHooks {
            status: None,
            contacts: Some(Box::new(move |fingers: &BTreeMap<u8, PanelPoint>| {
                kept.lock().unwrap().push(
                    fingers.values().map(|point| (point.x * 16383.0).round() as u16).collect(),
                );
            })),
            touch_count: None,
            #[cfg(feature = "debug")]
            report: None,
        }));
        let shared = Arc::new(Shared {
            apps: AppDirectory::new(),
            mapper: Mutex::new(None),
            window_drag_memo: Mutex::new(Vec::new()),
            own_pid: std::process::id() as i32,
            hooks,
        });
        (Pipeline::new(Arc::clone(&shared.hooks), shared), fingers)
    }

    /// A WCH mouse report (report id 7: buttons, x, y, wheel), the tip down or up at a third or
    /// two thirds of the panel's width, on its middle row.
    fn report(at_ns: u64, x: u16, down: bool) -> Command {
        let [xl, xh] = x.to_le_bytes();
        let [yl, yh] = 4800u16.to_le_bytes();
        Command::Panel(PanelEvent::Report(touchhid::RawReport {
            timestamp_ns: at_ns,
            bytes: vec![7, u8::from(down), xl, xh, yl, yh, 0],
        }))
    }

    // A touch at 1.000 s, its lift report at 1.050 s, and a touch somewhere else stamped from
    // 1.300 s: the pipeline's thread had other work from 1.035 s on, so it gets all of them at
    // once. The first touch ended at 1.100 s, when the panel had been quiet for 50 ms, and the
    // second is a touch of its own. Handled as they queue, with no regard for the stamps, the
    // second touch came while the first was still held, and looked like its finger coming back.
    #[test]
    fn a_report_stamped_after_a_deadline_finds_it_run() {
        let (mut pipeline, fingers) = machine();
        let mut now_ns = 0;
        for at_ns in [1_000_000_000, 1_017_000_000, 1_034_000_000] {
            assert!(!handle(&mut pipeline, &mut now_ns, report(at_ns, 4915, true)));
        }
        assert!(!handle(&mut pipeline, &mut now_ns, report(1_050_000_000, 4915, false)));
        assert_eq!(now_ns, 1_050_000_000);
        for at_ns in [1_300_000_000, 1_317_000_000, 1_334_000_000] {
            assert!(!handle(&mut pipeline, &mut now_ns, report(at_ns, 11468, true)));
        }
        assert_eq!(*fingers.lock().unwrap(), vec![vec![4915], vec![], vec![11468]]);
        assert_eq!(now_ns, 1_334_000_000);
    }
}
