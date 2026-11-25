//! The normal mode: a menu-bar agent that owns the touch panel while enabled.

use std::cell::{Cell, OnceCell, RefCell};
use std::process::Command;
use std::sync::Arc;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSApp, NSApplication, NSApplicationDelegate, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem,
    NSRunningApplication, NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWorkspace, NSWorkspaceDidWakeNotification,
    NSWorkspaceOpenConfiguration,
};
use objc2_application_services::{AXIsProcessTrusted, AXIsProcessTrustedWithOptions};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSBundle, NSError, NSNotification, NSString, NSTimer};
use objc2_io_kit::{IOHIDAccessType, IOHIDCheckAccess, IOHIDRequestAccess, IOHIDRequestType};
use touchcore::gestures::{InputMode, OneFingerDragMode, ThreeFingerAnchor};
use touchcore::launchpad::LaunchpadDockMode;
use touchcore::policy::{DeliveryMode, FocusMode};
use touchcore::prefs::TrackpadPreferences;
use touchplatform::pipeline::{InputPipeline, PipelineHooks};
use touchplatform::prefs_reader::TrackpadPreferencesReader;

#[cfg(feature = "debug")]
use objc2_foundation::{NSArray, NSURL};
#[cfg(feature = "debug")]
use touchplatform::pipeline::ReportHook;

use crate::log::{log, FileLog};
use crate::login_item;
use crate::settings::StoredSettings;
use crate::single_instance::SingleInstance;

/// The status item and every item an action reads or changes, built at launch.
struct Menu {
    status_item: Retained<NSStatusItem>,
    enabled: Retained<NSMenuItem>,
    dry_run: Retained<NSMenuItem>,
    gestures_direct: Retained<NSMenuItem>,
    return_focus: Retained<NSMenuItem>,
    keep_focus: Retained<NSMenuItem>,
    swipe_pages: Retained<NSMenuItem>,
    one_finger_scroll: Retained<NSMenuItem>,
    three_finger: Retained<NSMenuItem>,
    follow_trackpad: Retained<NSMenuItem>,
    login_item: Retained<NSMenuItem>,
    anchor_items: Vec<Retained<NSMenuItem>>,
    launchpad_items: Vec<Retained<NSMenuItem>>,
    touch_mode_items: Vec<Retained<NSMenuItem>>,
    mode_items: Vec<Retained<NSMenuItem>>,
    /// The followed trackpad settings, one indented line each under "Follow Trackpad Settings".
    trackpad_lines: Vec<Retained<NSMenuItem>>,
}

struct Ivars {
    pipeline: InputPipeline,
    log: Arc<FileLog>,
    /// Empty until `launch` builds the menu.
    menu: OnceCell<Menu>,
    panel_line: Retained<NSMenuItem>,
    display_line: Retained<NSMenuItem>,
    gesture_line: Retained<NSMenuItem>,
    touches_line: Retained<NSMenuItem>,
    stored: RefCell<StoredSettings>,
    trackpad: RefCell<TrackpadPreferences>,
    trackpad_timer: RefCell<Option<Retained<NSTimer>>>,
    permission_timer: RefCell<Option<Retained<NSTimer>>>,
    running: Cell<bool>,
    /// The lock that makes this the one TouchNext that owns the panel.
    instance: RefCell<Option<SingleInstance>>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements beyond `init`, which is inherited.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TouchNextMenuBarApp"]
    #[ivars = Ivars]
    struct MenuBarApp;

    impl MenuBarApp {
        #[unsafe(method(toggleEnabled))]
        fn toggle_enabled(&self) {
            self.set_enabled(!self.ivars().running.get());
        }

        #[unsafe(method(chooseMode:))]
        fn choose_mode(&self, sender: &NSMenuItem) {
            let forced = match sender.tag() {
                1 => Some(DeliveryMode::Direct),
                2 => Some(DeliveryMode::Warp),
                _ => None,
            };
            check_only(&self.menu().mode_items, sender);
            self.change_stored(move |stored| stored.delivery = forced);
            self.ivars().pipeline.update(move |settings| settings.policy.forced = forced);
            let named = forced.as_ref().map(DeliveryMode::as_str).unwrap_or("automatic");
            log!(self.ivars().log, "delivery mode: {named}");
        }

        #[unsafe(method(toggleGesturesDirect))]
        fn toggle_gestures_direct(&self) {
            let on = self.flip(&self.menu().gestures_direct);
            self.change_stored(move |stored| stored.gestures_direct = on);
            self.ivars().pipeline.update(move |settings| settings.policy.gestures_direct = on);
        }

        #[unsafe(method(toggleOneFingerScroll))]
        fn toggle_one_finger_scroll(&self) {
            let on = self.flip(&self.menu().one_finger_scroll);
            self.change_stored(move |stored| stored.one_finger_scroll = on);
            self.ivars().pipeline.update(move |settings| {
                settings.one_finger_drag = if on { OneFingerDragMode::Scroll } else { OneFingerDragMode::ClickAndDrag };
            });
            log!(self.ivars().log, "{}", if on { "one-finger drag: scroll" } else { "one-finger drag: click-and-drag" });
        }

        #[unsafe(method(toggleReturnFocus))]
        fn toggle_return_focus(&self) {
            let on = self.flip(&self.menu().return_focus);
            self.change_stored(move |stored| stored.return_focus = on);
            self.ivars().pipeline.update(move |settings| {
                settings.focus = if on { FocusMode::ReturnToPrevious } else { FocusMode::FollowTouch };
            });
            log!(self.ivars().log, "{}", if on { "focus: return to previous app" } else { "focus: follow touch" });
        }

        #[unsafe(method(toggleSwipePages))]
        fn toggle_swipe_pages(&self) {
            let on = self.flip(&self.menu().swipe_pages);
            self.change_stored(move |stored| stored.swipe_between_pages = on);
            self.ivars().pipeline.update(move |settings| settings.swipe_between_pages = on);
            log!(self.ivars().log, "{}", if on { "swipe between pages: on" } else { "swipe between pages: off" });
        }

        #[unsafe(method(toggleKeepFocus))]
        fn toggle_keep_focus(&self) {
            let on = self.flip(&self.menu().keep_focus);
            self.change_stored(move |stored| stored.keep_focus_on_space_switch = on);
            self.ivars().pipeline.update(move |settings| settings.keep_focus_on_space_switch = on);
            log!(self.ivars().log, "{}", if on {
                "space switch: keeps focus on the other display"
            } else {
                "space switch: focus follows the switch"
            });
        }

        #[unsafe(method(toggleThreeFinger))]
        fn toggle_three_finger(&self) {
            let on = self.flip(&self.menu().three_finger);
            self.change_stored(move |stored| stored.three_finger_drag = on);
            self.ivars().pipeline.update(move |settings| settings.three_finger_drag = on);
            log!(self.ivars().log, "{}", if on { "three-finger drag: on" } else { "three-finger drag: off" });
        }

        #[unsafe(method(toggleFollowTrackpad))]
        fn toggle_follow_trackpad(&self) {
            let stored = {
                let mut stored = self.ivars().stored.borrow_mut();
                stored.follow_trackpad = !stored.follow_trackpad;
                stored.save();
                stored.clone()
            };
            let following = stored.follow_trackpad;
            let latest = *self.ivars().trackpad.borrow();
            self.ivars().pipeline.update(move |settings| stored.apply(settings, &latest));
            self.refresh_trackpad_items();
            log!(self.ivars().log, "{}", if following { "follow trackpad settings: on" } else { "follow trackpad settings: off" });
        }

        #[unsafe(method(chooseAnchor:))]
        fn choose_anchor(&self, sender: &NSMenuItem) {
            let Some(anchor) = represented_text(sender).and_then(|raw| raw.parse::<ThreeFingerAnchor>().ok()) else { return };
            check_only(&self.menu().anchor_items, sender);
            self.change_stored(move |stored| stored.three_finger_anchor = anchor);
            self.ivars().pipeline.update(move |settings| settings.three_finger_anchor = anchor);
            log!(self.ivars().log, "three-finger drag point: {}", anchor.as_str());
        }

        #[unsafe(method(chooseTouchMode:))]
        fn choose_touch_mode(&self, sender: &NSMenuItem) {
            let Some(mode) = represented_text(sender).and_then(|raw| raw.parse::<InputMode>().ok()) else { return };
            check_only(&self.menu().touch_mode_items, sender);
            self.ivars().pipeline.update(move |settings| settings.input_mode = mode);
            // The menu-bar icon shows a special mode, so it isn't left on by accident.
            let symbol = match mode {
                InputMode::Gestures => "hand.point.up.left",
                InputMode::Drawing => "pencil.tip",
                InputMode::Multitouch => "hand.raised.fingers.spread",
            };
            self.set_icon(symbol);
            log!(self.ivars().log, "touch mode chosen: {}", mode.as_str());
        }

        #[unsafe(method(chooseLaunchpadDock:))]
        fn choose_launchpad_dock(&self, sender: &NSMenuItem) {
            let Some(mode) = represented_text(sender).and_then(|raw| raw.parse::<LaunchpadDockMode>().ok()) else { return };
            check_only(&self.menu().launchpad_items, sender);
            self.change_stored(move |stored| stored.launchpad_dock = mode);
            self.ivars().pipeline.update(move |settings| settings.launchpad_dock = mode);
            log!(self.ivars().log, "launchpad on the touch display: {}", mode.as_str());
            if mode == LaunchpadDockMode::BringDockAndReturn {
                self.ivars().pipeline.return_dock_home();
            }
        }

        #[unsafe(method(toggleLoginItem))]
        fn toggle_login_item(&self) {
            if login_item::is_enabled() {
                self.disable_login_item();
            } else {
                let _ = self.enable_login_item();
            }
        }

        #[unsafe(method(toggleDryRun))]
        fn toggle_dry_run(&self) {
            let on = self.flip(&self.menu().dry_run);
            self.ivars().pipeline.update(move |settings| settings.dry_run = on);
            log!(self.ivars().log, "{}", if on { "dry run on" } else { "dry run off" });
        }

        /// A diagnostic: a build with the `debug` feature only.
        #[cfg(feature = "debug")]
        #[unsafe(method(openEventSink))]
        fn open_event_sink(&self) {
            let config = NSWorkspaceOpenConfiguration::new();
            config.setCreatesNewApplicationInstance(true);
            config.setArguments(&NSArray::<NSString>::arrayWithObject(&NSString::from_str("--event-sink")));
            NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(&NSBundle::mainBundle().bundleURL(), &config, None);
        }

        /// A build with the `debug` feature has a log to open.
        #[cfg(feature = "debug")]
        #[unsafe(method(openLogs))]
        fn open_logs(&self) {
            let folder = NSURL::fileURLWithPath(&NSString::from_str(&crate::logs_directory().to_string_lossy()));
            NSWorkspace::sharedWorkspace().openURL(&folder);
        }

        #[unsafe(method(quit))]
        fn quit(&self) {
            NSApp(self.mtm()).terminate(None);
        }

        /// The trackpad timer: re-reads the settings every few seconds.
        #[unsafe(method(reloadTrackpadSettings:))]
        fn reload_trackpad_settings_tick(&self, _timer: &NSTimer) {
            self.reload_trackpad_settings();
        }

        /// The permission timer: relaunches once both permissions are granted.
        #[unsafe(method(checkPermissions:))]
        fn check_permissions_tick(&self, _timer: &NSTimer) {
            self.check_permissions();
        }

        /// The workspace woke from sleep: the settings may have changed meanwhile.
        #[unsafe(method(workspaceDidWake:))]
        fn workspace_did_wake(&self, _notification: &NSNotification) {
            self.reload_trackpad_settings();
        }
    }

    unsafe impl NSObjectProtocol for MenuBarApp {}

    unsafe impl NSApplicationDelegate for MenuBarApp {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn finished_launching(&self, _notification: &NSNotification) {
            self.launch();
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.stop_pipeline();
            log!(self.ivars().log, "quit; panel released");
        }
    }
);

/// The menu-bar app's application delegate, with its pipeline already reporting into its log and its
/// menu. Nothing touches the panel until the app has launched.
pub fn delegate(mtm: MainThreadMarker) -> Retained<ProtocolObject<dyn NSApplicationDelegate>> {
    ProtocolObject::from_retained(MenuBarApp::new(mtm))
}

impl MenuBarApp {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let log = Arc::new(FileLog::new("touchnext"));
        // Hooks first: anything the pipeline reports from here on reaches the log.
        let ivars = Ivars::new(mtm, InputPipeline::new(hooks(Arc::clone(&log))), log);
        // SAFETY: the instance variables are initialized, and `init` is NSObject's.
        unsafe { msg_send![super(Self::alloc(mtm).set_ivars(ivars)), init] }
    }

    /// Everything `applicationDidFinishLaunching` does.
    fn launch(&self) {
        let ivars = self.ivars();
        let Some(lock) = SingleInstance::acquire() else {
            log!(ivars.log, "another TouchNext is already running; exiting");
            NSApp(self.mtm()).terminate(None);
            return;
        };
        *ivars.instance.borrow_mut() = Some(lock);
        self.build_menu();
        log!(
            ivars.log,
            "started {}{}",
            NSBundle::mainBundle().bundlePath(),
            if login_item::is_launchd_instance() { " as login item" } else { "" }
        );
        let initial = ivars.stored.borrow().clone();
        let initial_trackpad = *ivars.trackpad.borrow();
        for line in initial.log_lines(&initial_trackpad) {
            log!(ivars.log, "{line}");
        }
        ivars.pipeline.update(move |settings| initial.apply(settings, &initial_trackpad));
        let arguments: Vec<String> = std::env::args().collect();
        if arguments.iter().any(|argument| argument == "--dry-run") {
            self.menu().dry_run.setState(NSControlStateValueOn);
            ivars.pipeline.update(|settings| settings.dry_run = true);
            log!(ivars.log, "started in dry run (log only)");
        }
        if login_item::remove_legacy_registration() {
            log!(ivars.log, "removed the old SMAppService login item");
        }
        self.refresh_login_item();
        if arguments.iter().any(|argument| argument == "--enable-login-item") && !login_item::is_launchd_instance()
            && self.enable_login_item() {
                return;
            }
        // Two permissions, each asked for once per launch with the system prompt. Accessibility lets
        // the app post events; Input Monitoring lets it seize the panel's mouse interface. A panel
        // with only a digitizer interface works without the second, so the app starts either way and
        // relaunches when a missing one arrives.
        let monitoring = IOHIDCheckAccess(IOHIDRequestType::ListenEvent) == IOHIDAccessType::Granted;
        if !monitoring {
            IOHIDRequestAccess(IOHIDRequestType::ListenEvent);
        }
        // "AXTrustedCheckOptionPrompt" is the value of kAXTrustedCheckOptionPrompt.
        let options = CFDictionary::<CFString, CFBoolean>::from_slices(
            &[&CFString::from_str("AXTrustedCheckOptionPrompt")],
            &[CFBoolean::new(true)],
        );
        // SAFETY: the cast only forgets the dictionary's key and value types.
        let options: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(options) };
        // SAFETY: the call reads a dictionary of prompt options and has no other preconditions.
        let accessibility = unsafe { AXIsProcessTrustedWithOptions(Some(&options)) };
        if accessibility {
            self.set_enabled(true);
        } else {
            ivars.panel_line.setTitle(&NSString::from_str("Panel: waiting for Accessibility permission"));
        }
        if !accessibility {
            log!(ivars.log, "Accessibility not granted; prompted, waiting");
        }
        if !monitoring {
            log!(ivars.log, "Input Monitoring not granted; prompted, waiting");
        }
        if !accessibility || !monitoring {
            // SAFETY: the target is this delegate, which lives as long as the app and answers the selector.
            let timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(2.0, self, sel!(checkPermissions:), None, true)
            };
            *ivars.permission_timer.borrow_mut() = Some(timer);
        }
    }

    /// The menu, which exists from `launch` on: every action and timer fires after it.
    fn menu(&self) -> &Menu {
        self.ivars().menu.get().expect("the menu is built before any action fires")
    }

    /// Changes one stored setting and saves the set.
    fn change_stored(&self, change: impl FnOnce(&mut StoredSettings)) {
        let mut stored = self.ivars().stored.borrow_mut();
        change(&mut stored);
        stored.save();
    }

    /// Flips a check-mark item and returns its new state.
    fn flip(&self, item: &NSMenuItem) -> bool {
        let on = item.state() != NSControlStateValueOn;
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        on
    }

    /// Builds the status item and its menu.
    fn build_menu(&self) {
        let mtm = self.mtm();
        let ivars = self.ivars();
        let stored = ivars.stored.borrow().clone();
        let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        set_image(mtm, &status_item, "hand.point.up.left");
        let menu = NSMenu::new(mtm);
        // Items greyed out while the trackpad settings decide.
        menu.setAutoenablesItems(false);
        for line in [&ivars.panel_line, &ivars.display_line, &ivars.gesture_line, &ivars.touches_line] {
            menu.addItem(line);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let enabled = self.add(&menu, "Enabled", sel!(toggleEnabled));
        // Per session: every start is back in Gestures, so the panel can't stay stuck in a special mode.
        let touch_mode_menu = NSMenu::new(mtm);
        let mut touch_mode_items = Vec::new();
        for (title, mode) in [
            ("Gestures", InputMode::Gestures),
            ("Drawing: One Finger Draws, Nothing Else", InputMode::Drawing),
            ("Multitouch Apps: All Fingers as TUIO (port 3333)", InputMode::Multitouch),
        ] {
            let checked = mode == InputMode::Gestures;
            touch_mode_items.push(self.choice(&touch_mode_menu, title, sel!(chooseTouchMode:), mode.as_str(), checked));
        }
        self.parent(&menu, "Touch Mode", &touch_mode_menu);
        let mode_menu = NSMenu::new(mtm);
        let mut mode_items = Vec::new();
        for (title, tag, mode) in [
            ("Automatic", 0, None),
            ("Direct only (testing)", 1, Some(DeliveryMode::Direct)),
            ("Warp only (testing)", 2, Some(DeliveryMode::Warp)),
        ] {
            let entry = self.choice(&mode_menu, title, sel!(chooseMode:), "", mode == stored.delivery);
            entry.setTag(tag);
            mode_items.push(entry);
        }
        self.parent(&menu, "Delivery", &mode_menu);
        let gestures_direct = self.add(&menu, "Pinch & Rotate Direct", sel!(toggleGesturesDirect));
        gestures_direct.setState(checked(stored.gestures_direct));
        let one_finger_scroll = self.add(&menu, "One-Finger Drag Scrolls (hold, then drag to click-and-drag)", sel!(toggleOneFingerScroll));
        one_finger_scroll.setState(checked(stored.one_finger_scroll));
        let swipe_pages = self.add(&menu, "Swipe Between Pages (Two Fingers)", sel!(toggleSwipePages));
        swipe_pages.setState(checked(stored.swipe_between_pages));
        let follow_trackpad = self.add(&menu, "Follow Trackpad Settings", sel!(toggleFollowTrackpad));
        let mut trackpad_lines = Vec::new();
        for _ in ivars.trackpad.borrow().settings() {
            let line = disabled_item(mtm, "");
            line.setIndentationLevel(1);
            menu.addItem(&line);
            trackpad_lines.push(line);
        }
        let three_finger = self.add(&menu, "Three-Finger Drag", sel!(toggleThreeFinger));
        let anchor_menu = NSMenu::new(mtm);
        let mut anchor_items = Vec::new();
        for (title, anchor) in [("Centre of the Fingers", ThreeFingerAnchor::Centroid), ("First Finger to Touch", ThreeFingerAnchor::FirstFinger)] {
            anchor_items.push(self.choice(&anchor_menu, title, sel!(chooseAnchor:), anchor.as_str(), anchor == stored.three_finger_anchor));
        }
        self.parent(&menu, "Three-Finger Drag Point", &anchor_menu);
        // Launchpad opens on the display that has the Dock.
        let launchpad_menu = NSMenu::new(mtm);
        let mut launchpad_items = Vec::new();
        for (title, mode) in [
            ("Bring the Dock Over, Then Back", LaunchpadDockMode::BringDockAndReturn),
            ("Bring the Dock Over (it stays)", LaunchpadDockMode::BringDock),
            ("Leave the Dock Where It Is", LaunchpadDockMode::Leave),
        ] {
            launchpad_items.push(self.choice(&launchpad_menu, title, sel!(chooseLaunchpadDock:), mode.as_str(), mode == stored.launchpad_dock));
        }
        self.parent(&menu, "Launchpad on the Touch Display", &launchpad_menu);
        let return_focus = self.add(&menu, "Return Focus to Previous App", sel!(toggleReturnFocus));
        return_focus.setState(checked(stored.return_focus));
        let keep_focus = self.add(&menu, "Keep Focus When Switching Spaces", sel!(toggleKeepFocus));
        keep_focus.setState(checked(stored.keep_focus_on_space_switch));
        // A build with the log file says where the record goes; elsewhere the menu line is all.
        let dry_run_title = if cfg!(feature = "debug") { "Dry Run (log only)" } else { "Dry Run (menu only)" };
        let dry_run = self.add(&menu, dry_run_title, sel!(toggleDryRun));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let login_item = self.add(&menu, "Start at Login", sel!(toggleLoginItem));
        #[cfg(feature = "debug")]
        let _ = self.add(&menu, "Open Event Sink", sel!(openEventSink));
        #[cfg(feature = "debug")]
        let _ = self.add(&menu, "Open Logs Folder", sel!(openLogs));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let quit_item = self.add(&menu, "Quit TouchNext", sel!(quit));
        quit_item.setKeyEquivalent(&NSString::from_str("q"));
        status_item.setMenu(Some(&menu));
        let built = ivars.menu.set(Menu {
            status_item,
            enabled,
            dry_run,
            gestures_direct,
            return_focus,
            keep_focus,
            swipe_pages,
            one_finger_scroll,
            three_finger,
            follow_trackpad,
            login_item,
            anchor_items,
            launchpad_items,
            touch_mode_items,
            mode_items,
            trackpad_lines,
        });
        debug_assert!(built.is_ok(), "the menu is built once, at launch");
        self.watch_trackpad_settings();
        self.refresh_trackpad_items();
    }

    /// Adds a menu item that sends `action` to this delegate.
    fn add(&self, menu: &NSMenu, title: &str, action: Sel) -> Retained<NSMenuItem> {
        let entry = item(self.mtm(), title, Some(action));
        // SAFETY: the target is this delegate, which lives as long as the menu and answers `action`.
        unsafe { entry.setTarget(Some(self)) };
        menu.addItem(&entry);
        entry
    }

    /// Adds one entry of a submenu that carries `raw` as its represented object.
    fn choice(&self, menu: &NSMenu, title: &str, action: Sel, raw: &str, checked: bool) -> Retained<NSMenuItem> {
        let entry = self.add(menu, title, action);
        // SAFETY: the represented object is a string this delegate reads back.
        unsafe { entry.setRepresentedObject(Some(&*NSString::from_str(raw))) };
        entry.setState(if checked { NSControlStateValueOn } else { NSControlStateValueOff });
        entry
    }

    /// Adds a parent item with its submenu.
    fn parent(&self, menu: &NSMenu, title: &str, submenu: &NSMenu) {
        let entry = item(self.mtm(), title, None);
        entry.setSubmenu(Some(submenu));
        menu.addItem(&entry);
    }

    /// Shows `symbol` on the status item, the default icon when the symbol is missing.
    fn set_icon(&self, symbol: &str) {
        set_image(self.mtm(), &self.menu().status_item, symbol);
    }

    /// The trackpad timer (every few seconds) and the wake notification: cfprefsd writes the plist
    /// lazily, so watching the file would miss changes.
    fn watch_trackpad_settings(&self) {
        // SAFETY: the target is this delegate, which lives as long as the app and answers the selector.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(3.0, self, sel!(reloadTrackpadSettings:), None, true)
        };
        *self.ivars().trackpad_timer.borrow_mut() = Some(timer);
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        // SAFETY: the observer is this delegate, which answers `workspaceDidWake:` with one object
        // argument and lives as long as the app.
        unsafe { center.addObserver_selector_name_object(self, sel!(workspaceDidWake:), Some(NSWorkspaceDidWakeNotification), None) };
    }

    /// Puts a status line where it belongs in the menu.
    fn show(&self, line: MenuLine) {
        let ivars = self.ivars();
        match line {
            MenuLine::Panel(text) => ivars.panel_line.setTitle(&NSString::from_str(&text)),
            MenuLine::Display(text) => ivars.display_line.setTitle(&NSString::from_str(&text)),
            MenuLine::Gesture(text) => ivars.gesture_line.setTitle(&NSString::from_str(&text)),
        }
    }

    fn set_enabled(&self, on: bool) {
        if !on && self.ivars().running.get() {
            self.stop_pipeline();
            self.ivars().panel_line.setTitle(&NSString::from_str("Panel: released (macOS default behaviour)"));
        } else if on && !self.ivars().running.get() {
            self.ivars().pipeline.start();
            self.ivars().running.set(true);
        }
        self.menu().enabled.setState(checked(self.ivars().running.get()));
    }

    /// Releases the panel and closes whatever gesture is open.
    fn stop_pipeline(&self) {
        if self.ivars().running.get() {
            self.ivars().pipeline.stop();
            self.ivars().running.set(false);
        }
    }

    /// The followed settings, and whether the menu's own three-finger switch is in charge.
    fn refresh_trackpad_items(&self) {
        let ivars = self.ivars();
        let following = ivars.stored.borrow().follow_trackpad;
        let menu = self.menu();
        menu.follow_trackpad.setState(checked(following));
        let trackpad = ivars.trackpad.borrow();
        for (line, (name, on)) in menu.trackpad_lines.iter().zip(trackpad.settings()) {
            line.setTitle(&NSString::from_str(&format!("{}: {}", capitalised(name), if on { "on" } else { "off" })));
            line.setHidden(!following);
        }
        let dragging = if following { trackpad.three_finger_drag } else { ivars.stored.borrow().three_finger_drag };
        menu.three_finger.setEnabled(!following);
        menu.three_finger.setState(checked(dragging));
        menu.three_finger.setTitle(&NSString::from_str(if following {
            "Three-Finger Drag (from Trackpad settings)"
        } else {
            "Three-Finger Drag"
        }));
    }

    /// Re-reads the trackpad settings, which change while the app runs.
    fn reload_trackpad_settings(&self) {
        let latest = TrackpadPreferencesReader::read();
        if latest == *self.ivars().trackpad.borrow() {
            return;
        }
        *self.ivars().trackpad.borrow_mut() = latest;
        log!(self.ivars().log, "trackpad settings changed: {}", latest.summary());
        self.refresh_trackpad_items();
        if self.ivars().stored.borrow().follow_trackpad {
            self.ivars().pipeline.update(move |settings| settings.trackpad = Some(latest));
        }
    }

    fn refresh_login_item(&self) {
        let item = &self.menu().login_item;
        item.setState(checked(login_item::is_enabled()));
        item.setTitle(&NSString::from_str("Start at Login (restarts after a crash)"));
    }

    /// Registers the launch agent and, unless launchd already runs this process, hands the panel
    /// over to it so a crash will be restarted. True when this process is about to quit for the
    /// hand-over.
    fn enable_login_item(&self) -> bool {
        if let Err(error) = login_item::enable() {
            log!(self.ivars().log, "login item: {error}");
            self.refresh_login_item();
            return false;
        }
        self.refresh_login_item();
        log!(self.ivars().log, "login item: {} ({})", login_item::describe(), login_item::plist_path().display());
        if login_item::is_launchd_instance() {
            return false;
        }
        log!(self.ivars().log, "handing over to the login item");
        self.stop_pipeline();
        self.release_instance();
        login_item::hand_over_to_launchd();
        NSApp(self.mtm()).terminate(None);
        true
    }

    /// Unregistering stops the launchd job, so a login-item instance first starts a normal copy of
    /// itself.
    fn disable_login_item(&self) {
        if !login_item::is_launchd_instance() {
            login_item::disable();
            self.refresh_login_item();
            log!(self.ivars().log, "login item: {}", login_item::describe());
            return;
        }
        log!(self.ivars().log, "login item off; continuing as a normal app");
        self.stop_pipeline();
        self.release_instance();
        let config = NSWorkspaceOpenConfiguration::new();
        config.setCreatesNewApplicationInstance(true);
        // The completion runs on one of the workspace's queues. Unloading the agent ends this
        // process, so the quit is told to the main thread first-in-line there.
        let handler = RcBlock::new(move |_application: *mut NSRunningApplication, _error: *mut NSError| {
            on_main(|mtm| {
                // Unloading the agent ends this process.
                login_item::disable();
                NSApplication::sharedApplication(mtm).terminate(None);
            });
        });
        NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(&NSBundle::mainBundle().bundleURL(), &config, Some(&handler));
    }

    /// Privacy grants only reach a new process, so start a fresh copy and exit.
    fn relaunch(&self) {
        if let Some(timer) = self.ivars().permission_timer.borrow_mut().take() {
            timer.invalidate();
        }
        log!(self.ivars().log, "Accessibility and Input Monitoring granted; relaunching");
        if login_item::is_launchd_instance() {
            // A failure exit makes launchd start a fresh process (which sees the new permission).
            self.release_instance();
            std::process::exit(1);
        }
        let script = format!("sleep 1; /usr/bin/open -n \"{}\"", NSBundle::mainBundle().bundlePath());
        // The shell outlives this process, so it is neither waited for nor killed with it.
        let _ = Command::new("/bin/sh").args(["-c", &script]).spawn();
        self.release_instance();
        NSApp(self.mtm()).terminate(None);
    }

    fn check_permissions(&self) {
        // SAFETY: the call only asks the system whether this process is trusted.
        let trusted = unsafe { AXIsProcessTrusted() };
        if trusted && IOHIDCheckAccess(IOHIDRequestType::ListenEvent) == IOHIDAccessType::Granted {
            self.relaunch();
        }
    }

    /// Gives the lock up, in time for the copy that takes over.
    fn release_instance(&self) {
        drop(self.ivars().instance.borrow_mut().take());
    }
}

impl Ivars {
    fn new(mtm: MainThreadMarker, pipeline: InputPipeline, log: Arc<FileLog>) -> Ivars {
        Ivars {
            pipeline,
            log,
            menu: OnceCell::new(),
            panel_line: disabled_item(mtm, "Panel: starting…"),
            display_line: disabled_item(mtm, "Display: looking for the touch display…"),
            gesture_line: disabled_item(mtm, "Last gesture: none"),
            touches_line: disabled_item(mtm, &touch_count_title(0, 0)),
            stored: RefCell::new(StoredSettings::load()),
            trackpad: RefCell::new(TrackpadPreferencesReader::read()),
            trackpad_timer: RefCell::new(None),
            permission_timer: RefCell::new(None),
            running: Cell::new(false),
            instance: RefCell::new(None),
        }
    }
}

/// What the pipeline reports while it runs. The pipeline calls its hooks on its own thread, so each
/// hands its line to the main thread, which owns the menu.
fn hooks(log: Arc<FileLog>) -> PipelineHooks {
    let status_log = Arc::clone(&log);
    PipelineHooks {
        status: Some(Box::new(move |message| {
            log!(status_log, "{message}");
            // A line the menu does not show never leaves this thread.
            let Some(line) = menu_line(message) else { return };
            on_main(move |mtm| {
                if let Some(app) = app_delegate(mtm) {
                    app.show(line);
                }
            });
        })),
        contacts: None,
        touch_count: Some(Box::new(move |now, most| {
            let title = touch_count_title(now, most);
            on_main(move |mtm| {
                if let Some(app) = app_delegate(mtm) {
                    app.ivars().touches_line.setTitle(&NSString::from_str(&title));
                }
            });
        })),
        #[cfg(feature = "debug")]
        report: Some(trace_raw_reports(&log)),
    }
}

/// How many runs keep their raw reports. A run that crashes is restarted every few seconds, and
/// the reports from before the first crash are the ones wanted.
#[cfg(feature = "debug")]
const KEPT_TRACES: usize = 10;

/// Keeps this run's raw panel reports beside the log, one line a report in the probe's format, so
/// that what the app was given can be replayed through it again (`--replay`). The file is named
/// after the moment the run started, and the line this writes to the log names it and says what
/// the machine's clock read then: the reports are stamped on that clock.
#[cfg(feature = "debug")]
fn trace_raw_reports(log: &FileLog) -> ReportHook {
    let name = format!("reports-{}", crate::log::file_stamp());
    let mut earlier: Vec<std::path::PathBuf> = std::fs::read_dir(crate::logs_directory())
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("reports-") && name.ends_with(".log"))
                })
                .collect()
        })
        .unwrap_or_default();
    // Named by their start time, so the first names are the oldest runs.
    earlier.sort();
    for path in earlier.iter().take(earlier.len().saturating_sub(KEPT_TRACES - 1)) {
        let _ = std::fs::remove_file(path);
    }

    let reports = Arc::new(FileLog::new(&name));
    log!(log, "this run's raw reports go to {name}.log; the machine clock read {} ns", touchhid::clock::now_ns());
    Box::new(move |bytes: &[u8], at_ns: u64| {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        reports.write_line(&format!("{{\"t_ns\":{at_ns},\"hex\":\"{hex}\"}}\n"));
    })
}

/// The menu line a status line belongs to, with the title it gets there. `None` for a line the
/// menu does not show.
#[derive(Debug, PartialEq)]
enum MenuLine {
    Panel(String),
    Display(String),
    Gesture(String),
}

fn menu_line(message: &str) -> Option<MenuLine> {
    let starts = |prefix: &str| message.starts_with(prefix);
    if starts("panel") || starts("seize") || starts("matching") || starts("stopped") {
        Some(MenuLine::Panel(format!("Panel: {message}")))
    } else if starts("touch display") {
        Some(MenuLine::Display(format!("Display: {message}")))
    } else if message.contains(" → ") {
        Some(MenuLine::Gesture(format!("Last gesture: {message}")))
    } else {
        None
    }
}

/// The "Touches" line: how many fingers are down now and the most there have been.
pub(crate) fn touch_count_title(now: usize, most: usize) -> String {
    format!("Touches: {now} now · {most} max (tracks up to 10)")
}

/// This delegate, found the way AppKit code finds it: as the running application's delegate. `None`
/// before the app has one or after it lost it.
fn app_delegate(mtm: MainThreadMarker) -> Option<Retained<MenuBarApp>> {
    let delegate = NSApp(mtm).delegate()?;
    let object: &AnyObject = <ProtocolObject<dyn NSApplicationDelegate> as AsRef<AnyObject>>::as_ref(&*delegate);
    Some(object.downcast_ref::<MenuBarApp>()?.retain())
}

/// Runs `work` on the main thread, after what the main thread is doing.
fn on_main(work: impl FnOnce(MainThreadMarker) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("the main queue runs on the main thread");
        work(mtm);
    });
}

/// A menu item. `action` is `None` for a parent item and for a status line.
fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>) -> Retained<NSMenuItem> {
    // SAFETY: `alloc` returned an uninitialized item, which its designated initializer initializes.
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), action, &NSString::from_str("")) }
}

/// A menu item the user cannot choose: a status line.
fn disabled_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    let line = item(mtm, title, None);
    line.setEnabled(false);
    line
}

/// Puts the symbol image on the status item's button, the default icon when the symbol is missing.
fn set_image(mtm: MainThreadMarker, status_item: &NSStatusItem, symbol: &str) {
    let symbol_image = |name: &str| NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), Some(&NSString::from_str("TouchNext")));
    let image = symbol_image(symbol).or_else(|| symbol_image("hand.point.up.left"));
    if let Some(button) = status_item.button(mtm) {
        button.setImage(image.as_deref());
    }
}

/// Checks `sender` among `items` and unchecks the others.
fn check_only(items: &[Retained<NSMenuItem>], sender: &NSMenuItem) {
    for item in items {
        item.setState(checked(std::ptr::eq(&**item, sender)));
    }
}

/// The state of a check-mark item.
fn checked(on: bool) -> isize {
    if on { NSControlStateValueOn } else { NSControlStateValueOff }
}

/// The string a menu item carries as its represented object.
fn represented_text(item: &NSMenuItem) -> Option<String> {
    item.representedObject()?.downcast_ref::<NSString>().map(|text| text.to_string())
}

/// `name` with its first character in upper case, as the menu line shows it.
fn capitalised(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// SIGINT and SIGTERM end the app the way Quit does, so the panel is released first. Both are blocked
/// for every thread (threads started later inherit the mask) and waited for on a thread of their own,
/// which hands the quit to the main thread. Call before any other thread exists.
pub fn watch_termination_signals() {
    // SAFETY: a zeroed `sigset_t` is an empty set, and these calls only fill it in and change this
    // thread's signal mask.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    };
    std::thread::Builder::new()
        .name("signals".to_owned())
        .spawn(move || {
            loop {
                let mut signal: libc::c_int = 0;
                // SAFETY: `set` is a filled-in set and `signal` is a valid out slot.
                if unsafe { libc::sigwait(&set, &mut signal) } != 0 {
                    return;
                }
                on_main(|mtm| NSApplication::sharedApplication(mtm).terminate(None));
            }
        })
        .expect("the signal thread starts");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_line_goes_to_the_menu_line_it_names() {
        assert_eq!(menu_line("panel seized"), Some(MenuLine::Panel("Panel: panel seized".to_owned())));
        assert_eq!(
            menu_line("touch display at 0,0 1920×1280 rotation 0"),
            Some(MenuLine::Display("Display: touch display at 0,0 1920×1280 rotation 0".to_owned()))
        );
        assert_eq!(
            menu_line("tap → direct on Safari"),
            Some(MenuLine::Gesture("Last gesture: tap → direct on Safari".to_owned()))
        );
        // The rest of the stream is for the log.
        assert_eq!(menu_line("touch mode: gestures"), None);
    }

    #[test]
    fn the_touches_line_counts_now_and_the_most() {
        assert_eq!(touch_count_title(2, 3), "Touches: 2 now · 3 max (tracks up to 10)");
    }
}
