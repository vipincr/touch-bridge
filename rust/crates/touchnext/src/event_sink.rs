//! The Event Sink (`--event-sink`): a window that shows and logs every event an ordinary app
//! receives, so the driver's delivery can be compared with what the window server routes. Opens on
//! the touch display. Three listeners feed it: the local AppKit monitor (events as the app
//! dequeues them), a listen-only event tap for this process (everything the window server sends,
//! including what AppKit's swipe tracking pulls straight from the queue), and a TUIO socket
//! (Multitouch mode's fingers).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use block2::RcBlock;
use dispatch2::{
    DispatchObject, DispatchQueue, DispatchRetained, DispatchSource, _dispatch_source_type_read,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApp, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBezierPath, NSButton, NSColor, NSEvent,
    NSEventMask, NSEventModifierFlags, NSEventPhase, NSEventSwipeTrackingOptions, NSEventType,
    NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSLineCapStyle, NSLineJoinStyle,
    NSRectFill, NSScreen, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CGFloat, CFMachPort, CFRunLoop, CFRetained, CGPoint, CGSize, CGRect, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy, CGEventType,
};
use objc2_foundation::{
    NSDictionary, NSNotification, NSNumber, NSString, NSAttributedStringKey,
};

use touchcore::tuio::{Frame, TUIO};
use touchplatform::event::TAG;
use touchplatform::hid_bridge::HIDEventBridge;
use touchplatform::pipeline::{self, PipelineSettings};
use touchplatform::runtime_symbols::lookup;

use crate::log::FileLog;

/// `CGPoint CGEventGetWindowLocation(CGEventRef event)`: the window-relative location the window
/// server stores in routed events, read out of the private symbol.
type GetWindowLocation = unsafe extern "C" fn(*const CGEvent) -> CGPoint;

static GET_WINDOW_LOCATION: OnceLock<Option<GetWindowLocation>> = OnceLock::new();

/// The private `CGEventGetWindowLocation`, when this macOS exports it.
fn get_window_location() -> Option<GetWindowLocation> {
    // SAFETY: the cell stores exactly the C signature of the symbol it names.
    unsafe { lookup(c"CGEventGetWindowLocation", &GET_WINDOW_LOCATION) }
}

/// The delegate's state: what it owns for the process's life.
struct EventSinkIvars {
    log: Arc<FileLog>,
    window: RefCell<Option<Retained<NSWindow>>>,
    sink: RefCell<Option<Retained<SinkView>>>,
    /// The local event monitor's token: dropping it would uninstall the monitor.
    monitor: RefCell<Option<Retained<AnyObject>>>,
    /// The event tap's port; the run loop source holds it too, this keeps the owner explicit.
    tap_port: RefCell<Option<CFRetained<CFMachPort>>>,
    /// The TUIO read source; dropping it would close the listener.
    tuio_source: RefCell<Option<DispatchRetained<DispatchSource>>>,
}

define_class!(
    // SAFETY: NSObject imposes no subclassing requirements beyond `init`, which is inherited.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TouchNextEventSink"]
    #[ivars = EventSinkIvars]
    struct EventSink;

    impl EventSink {}

    unsafe impl NSObjectProtocol for EventSink {}

    unsafe impl NSApplicationDelegate for EventSink {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn finished_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let ivars = self.ivars();
            NSApp(mtm).setActivationPolicy(NSApplicationActivationPolicy::Regular);
            // Created *on* the touch screen: without `screen:` AppKit constrains the frame onto the
            // main display. The touch display comes from the one place that knows it; the main
            // screen stands in when the panel is not attached.
            let settings = PipelineSettings::default();
            let screen = pipeline::locate_touch_display(settings.display_vendor, settings.display_model)
                .and_then(|(id, _)| screen_with_number(mtm, id))
                .or_else(|| NSScreen::mainScreen(mtm))
                .expect("no screen to open the event sink on");
            let bounds = screen.frame();
            let frame = CGRect::new(
                CGPoint::new(bounds.size.width / 2.0 - 450.0, bounds.size.height / 2.0 - 300.0),
                CGSize::new(900.0, 600.0),
            );
            // SAFETY: the initializer is NSWindow's designated one, and the screen is live.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer_screen(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                    Some(&screen),
                )
            };
            window.setTitle(&NSString::from_str("TouchNext Event Sink"));
            // SAFETY: a plain property setter.
            unsafe { window.setReleasedWhenClosed(false) };

            let sink = SinkView::new(
                mtm,
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(900.0, 540.0)),
                Arc::clone(&ivars.log),
                bounds,
            );
            sink.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            // SAFETY: the target is live for the window's life, and the action is on its class.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Button"),
                    Some(&*sink),
                    Some(sel!(buttonPressed)),
                    mtm,
                )
            };
            button.setFrame(CGRect::new(CGPoint::new(16.0, 552.0), CGSize::new(120.0, 32.0)));
            button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
            let field = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                CGRect::new(CGPoint::new(150.0, 556.0), CGSize::new(300.0, 24.0)),
            );
            field.setPlaceholderString(Some(&NSString::from_str("Text field (tap, then type)")));
            field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
            // SAFETY: as the Button above.
            let clear = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Clear"),
                    Some(&*sink),
                    Some(sel!(clear)),
                    mtm,
                )
            };
            clear.setFrame(CGRect::new(CGPoint::new(466.0, 552.0), CGSize::new(100.0, 32.0)));
            clear.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
            let content = NSView::initWithFrame(
                NSView::alloc(mtm),
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(900.0, 600.0)),
            );
            content.addSubview(&sink);
            content.addSubview(&button);
            content.addSubview(&field);
            content.addSubview(&clear);
            window.setContentView(Some(&content));
            window.makeKeyAndOrderFront(None);
            ivars.log.write(&format!(
                "— event sink opened on {}, window {}, pid {} —",
                screen.localizedName(),
                window.windowNumber(),
                // SAFETY: the call has no preconditions and never fails.
                unsafe { libc::getpid() },
            ));
            log_every_event_entering_the_app(ivars);
            tap_every_event_sent_to_this_process(ivars);
            listen_for_tuio(ivars, &sink);
            *ivars.sink.borrow_mut() = Some(sink);
            *ivars.window.borrow_mut() = Some(window);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            true
        }
    }
);

/// The Event Sink's application delegate (`--event-sink`): a window that shows and logs every
/// event an ordinary app receives.
pub fn delegate(mtm: MainThreadMarker, log: Arc<FileLog>) -> Retained<ProtocolObject<dyn NSApplicationDelegate>> {
    ProtocolObject::from_retained(EventSink::new(mtm, log))
}

impl EventSink {
    fn new(mtm: MainThreadMarker, log: Arc<FileLog>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EventSinkIvars {
            log,
            window: RefCell::new(None),
            sink: RefCell::new(None),
            monitor: RefCell::new(None),
            tap_port: RefCell::new(None),
            tuio_source: RefCell::new(None),
        });
        // SAFETY: the instance variables are initialized, and `init` is NSObject's.
        unsafe { msg_send![super(this), init] }
    }
}

/// The screen whose display number is `id`, as the window is opened on it.
fn screen_with_number(mtm: MainThreadMarker, id: u32) -> Option<Retained<NSScreen>> {
    let number_key = NSString::from_str("NSScreenNumber");
    NSScreen::screens(mtm)
        .iter()
        .find(|screen| {
            let Some(number) = screen.deviceDescription().objectForKey(&number_key) else {
                return false;
            };
            let Some(number) = number.downcast_ref::<NSNumber>() else {
                return false;
            };
            number.unsignedIntValue() == id
        })
}

/// Logs events as the app dequeues them, before any window or view sees them, so the log can tell
/// "never reached the app" from "reached the app but no window took it".
fn log_every_event_entering_the_app(ivars: &EventSinkIvars) {
    let log = Arc::clone(&ivars.log);
    let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        let event = unsafe { event.as_ref() };
        let kind = event.r#type();
        const NOISE: [NSEventType; 8] = [
            NSEventType::MouseMoved,
            NSEventType::AppKitDefined,
            NSEventType::SystemDefined,
            NSEventType::ApplicationDefined,
            NSEventType::Periodic,
            NSEventType::CursorUpdate,
            NSEventType::FlagsChanged,
            NSEventType::Pressure,
        ];
        if NOISE.contains(&kind) {
            return NonNull::from(event).as_ptr();
        }
        let tag = if event
            .CGEvent()
            .map(|cg| CGEvent::integer_value_field(Some(&cg), CGEventField::EventSourceUserData))
            == Some(TAG)
        {
            " TOUCHBRIDGE"
        } else {
            ""
        };
        // Raw fields of mouse-downs, the first and last event of each scroll gesture, and gesture
        // events, to diff window-server-routed events against TouchNext's.
        let scroll_edge = kind == NSEventType::ScrollWheel
            && (event.phase() == NSEventPhase::Began
                || event.phase() == NSEventPhase::Ended
                || event.momentumPhase() == NSEventPhase::Began);
        let gesture = [
            NSEventType::Gesture,
            NSEventType::BeginGesture,
            NSEventType::EndGesture,
            NSEventType::Swipe,
        ]
        .contains(&kind);
        let mut fields = String::new();
        if (kind == NSEventType::LeftMouseDown || scroll_edge || gesture)
            && let Some(cg) = event.CGEvent() {
                fields = format!(
                    " fields:{}",
                    event_fields(&cg, 0..256)
                        .iter()
                        .map(|field| format!(" {field}"))
                        .collect::<String>(),
                );
            }
        if kind == NSEventType::ScrollWheel || gesture {
            fields = format!(
                " phase={} momentum={} dx={:.1} dy={:.1} precise={} inverted={}{fields}",
                event.phase().bits(),
                event.momentumPhase().bits(),
                event.scrollingDeltaX(),
                event.scrollingDeltaY(),
                if event.hasPreciseScrollingDeltas() { "y" } else { "n" },
                if event.isDirectionInvertedFromDevice() { "y" } else { "n" },
            );
        }
        if kind == NSEventType::ScrollWheel
            && (event.phase() == NSEventPhase::Began
                || event.phase() == NSEventPhase::Ended
                || (event.phase() == NSEventPhase::Changed && event.scrollingDeltaX() != 0.0))
            && let Some(cg) = event.CGEvent()
        {
            let hid = HIDEventBridge::describe_attached(&cg)
                .map(|text| text.replace('\n', " | "))
                .unwrap_or_else(|| "none".to_string());
            fields.push_str(&format!(" hid={hid}"));
        }
        if kind == NSEventType::KeyDown || kind == NSEventType::KeyUp {
            fields.push_str(&format!(
                " key={} chars={} mods={}",
                event.keyCode(),
                event.characters().map(|chars| chars.to_string()).unwrap_or_default(),
                (event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask).bits(),
            ));
        }
        let mut window_location = String::new();
        if let Some(cg) = event.CGEvent()
            && let Some(getter) = get_window_location()
        {
            // SAFETY: the event is live and `getter` is `CGEventGetWindowLocation` itself.
            let p = unsafe { getter(&*cg) };
            window_location = format!(" wloc={:.0},{:.0}", p.x, p.y);
        }
        log.write(&format!(
            "APP type={} win={} loc={:.0},{:.0}{}{}{}",
            kind.0,
            event.windowNumber(),
            event.locationInWindow().x,
            event.locationInWindow().y,
            window_location,
            tag,
            fields,
        ));
        NonNull::from(event).as_ptr()
    });
    // SAFETY: the block returns the event pointer to pass every event on, which is the monitor's
    // contract.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask(!0), &block)
    };
    *ivars.monitor.borrow_mut() = monitor;
}

/// A listen-only tap on every event the window server sends this process, including those that
/// AppKit's swipe tracking pulls straight from the queue (the app-level monitor never sees them).
fn tap_every_event_sent_to_this_process(ivars: &EventSinkIvars) {
    // The tap's userInfo outlives every call: a leaked Arc the callback reads, valid as long as
    // the process runs.
    let log: &'static Arc<FileLog> = Box::leak(Box::new(Arc::clone(&ivars.log)));
    let user_info = Arc::as_ptr(log) as *mut c_void;
    // SAFETY: the callback passes every event through untouched, and `user_info` is the leaked
    // Arc above.
    let tap = unsafe {
        CGEvent::tap_create_for_pid(
            libc::getpid(),
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            u64::MAX,
            Some(tap_callback),
            user_info,
        )
    };
    let Some(tap) = tap else {
        ivars.log.write("event tap for this process could not be created");
        return;
    };
    if let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        && let Some(run_loop) = CFRunLoop::main()
    {
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    }
    CGEvent::tap_enable(&tap, true);
    *ivars.tap_port.borrow_mut() = Some(tap);
    ivars.log.write("tapping every event sent to this process");
}

/// The tap's callback: logs the event's private fields, and returns the event to let it through.
unsafe extern "C-unwind" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    if user_info.is_null() {
        return event.as_ptr();
    }
    // SAFETY: `user_info` is the leaked `Arc<FileLog>` the tap was created with, which lives as
    // long as the process.
    let log = unsafe { &*(user_info as *const Arc<FileLog>) };
    if event_type != CGEventType::MouseMoved {
        let event = unsafe { event.as_ref() };
        let fields = event_fields(event, 100..180).join(" ");
        let tag = if CGEvent::integer_value_field(Some(event), CGEventField::EventSourceUserData) == TAG {
            " TOUCHBRIDGE"
        } else {
            ""
        };
        log.write(&format!("TAP type={}{} {}", event_type.0, tag, fields));
    }
    event.as_ptr()
}

/// Multitouch mode sends every finger as TUIO; listening like any TUIO app lets this window paint
/// them all.
fn listen_for_tuio(ivars: &EventSinkIvars, view: &SinkView) {
    let log = &ivars.log;
    // SAFETY: plain socket creation; a negative fd only means it failed.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_UDP) };
    if fd < 0 {
        log.write("TUIO: no socket");
        return;
    }
    // Both options, so the driver's sender and this window may share the port.
    let yes: libc::c_int = 1;
    // SAFETY: `yes` outlives each call, and the size matches the option's type.
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            std::ptr::addr_of!(yes).cast(),
            std::mem::size_of::<libc::c_int>() as u32,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            std::ptr::addr_of!(yes).cast(),
            std::mem::size_of::<libc::c_int>() as u32,
        );
    }
    let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    address.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    address.sin_family = libc::AF_INET as u8;
    address.sin_port = 3333u16.to_be();
    // 127.0.0.1, in the network byte order the field is stored in.
    address.sin_addr.s_addr = u32::from_ne_bytes([127, 0, 0, 1]);
    // SAFETY: `address` is a valid `sockaddr_in`, and the length is its documented size.
    let bound = unsafe {
        libc::bind(
            fd,
            std::ptr::addr_of!(address).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    if bound != 0 {
        log.write("TUIO: port 3333 is in use by another app; Multitouch fingers won't show here");
        // SAFETY: closing the socket this function created.
        unsafe { libc::close(fd) };
        return;
    }
    // The read source delivers on the main queue, so the view is painted on the main thread.
    // SAFETY: the read source type is the static libdispatch provides for file descriptors.
    let source = unsafe {
        DispatchSource::new(
            std::ptr::addr_of!(_dispatch_source_type_read).cast_mut(),
            fd as usize,
            0,
            Some(DispatchQueue::main()),
        )
    };
    // SAFETY: the view is alive (the caller holds a reference to it), so the retain cannot fail.
    let view = unsafe { Retained::retain(NonNull::from(view).as_ptr()) }.expect("the sink view is alive");
    let block = RcBlock::new(move || {
        let mut buffer = [0u8; 65_536];
        // SAFETY: `buffer` is a valid receive buffer of the length passed.
        let count = unsafe { libc::recv(fd, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
        if count <= 0 {
            return;
        }
        let Some(frame) = TUIO::decode(&buffer[..count as usize]) else {
            return;
        };
        view.show(&frame);
    });
    // SAFETY: the block is kept alive by the source that owns it.
    unsafe { source.set_event_handler_with_block(RcBlock::into_raw(block)) };
    let cancel = RcBlock::new(move || {
        // SAFETY: closing the socket this listener created; the cancel handler runs once.
        unsafe { libc::close(fd) };
    });
    // SAFETY: as above.
    unsafe { source.set_cancel_handler_with_block(RcBlock::into_raw(cancel)) };
    source.resume();
    *ivars.tuio_source.borrow_mut() = Some(source);
    log.write("TUIO: listening on 127.0.0.1:3333 (Multitouch mode paints every finger here)");
}

/// The event's non-zero fields in `range`, as "field=value" strings: the double when it carries
/// something the integer does not, the integer otherwise, nothing at zero.
fn event_fields(event: &CGEvent, range: std::ops::Range<u32>) -> Vec<String> {
    range
        .filter_map(|i| {
            let field = CGEventField(i);
            let int = CGEvent::integer_value_field(Some(event), field);
            let double = CGEvent::double_value_field(Some(event), field);
            if double != 0.0 && double != int as f64 {
                Some(format!("{i}={}", format_g(double, 4)))
            } else if int != 0 {
                Some(format!("{i}={int}"))
            } else {
                None
            }
        })
        .collect()
}

/// C's `%g` with `precision` significant digits, as the Swift tools print field values.
fn format_g(value: f64, precision: usize) -> String {
    let magnitude = value.abs();
    let mut decimal_exp = 0i32;
    if magnitude != 0.0 && magnitude.is_finite() {
        decimal_exp = magnitude.log10().floor() as i32;
        // `log10` of a value just under a power of ten can round the exponent the wrong way.
        let scaled = magnitude / 10f64.powi(decimal_exp);
        if !(1.0..10.0).contains(&scaled) {
            decimal_exp += if scaled < 1.0 { -1 } else { 1 };
        }
    }
    let text = if decimal_exp < -4 || decimal_exp >= precision as i32 {
        let decimals = precision - 1;
        let mut mantissa = format!("{:.*}", decimals, magnitude / 10f64.powi(decimal_exp));
        if mantissa.starts_with("10.") || mantissa == "10" {
            // The mantissa rounded up to the next power of ten; %g renormalizes instead.
            decimal_exp += 1;
            mantissa = "1".to_string();
        } else {
            strip_zeros(&mut mantissa);
        }
        let sign = if decimal_exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", decimal_exp.abs())
    } else {
        let decimals = usize::try_from(precision as i32 - 1 - decimal_exp).unwrap_or(0);
        let mut text = format!("{:.*}", decimals, magnitude);
        strip_zeros(&mut text);
        text
    };
    if value.is_sign_negative() && magnitude != 0.0 {
        format!("-{text}")
    } else {
        text
    }
}

/// Trailing zeros after the decimal point, and a bare trailing point, as %g drops them.
fn strip_zeros(text: &mut String) {
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
}

/// The view's state.
struct SinkViewIvars {
    log: Arc<FileLog>,
    /// TUIO positions are fractions of the touch display, wherever this window is.
    touch_screen: CGRect,
    /// The last log lines, drawn at the top of the view.
    lines: RefCell<Vec<String>>,
    /// Every stroke painted since the last Clear: one per finger (TUIO) and per mouse drag.
    trails: RefCell<Vec<Trail>>,
    /// Session -> index into `trails`, for the fingers still down.
    live_trail: RefCell<HashMap<i32, usize>>,
    /// Session -> the finger's position now.
    live_fingers: RefCell<HashMap<i32, CGPoint>>,
    /// The mouse drag's trail index, while one is down.
    mouse_trail: RefCell<Option<usize>>,
    most_fingers: Cell<usize>,
}

/// One stroke: its colour and its points so far.
struct Trail {
    color: Retained<NSColor>,
    points: Vec<CGPoint>,
}

define_class!(
    // SAFETY: NSView's designated initializer is `initWithFrame:`, which the class inherits.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "TouchNextSinkView"]
    #[ivars = SinkViewIvars]
    struct SinkView;

    impl SinkView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(buttonPressed))]
        fn button_pressed(&self) {
            self.record("BUTTON action fired".to_string());
        }

        #[unsafe(method(clear))]
        fn clear(&self) {
            let ivars = self.ivars();
            ivars.trails.borrow_mut().clear();
            ivars.live_trail.borrow_mut().clear();
            *ivars.mouse_trail.borrow_mut() = None;
            ivars.most_fingers.set(ivars.live_fingers.borrow().len());
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: CGRect) {
            let ivars = self.ivars();
            NSColor::textBackgroundColor().setFill();
            NSRectFill(self.bounds());
            for trail in ivars.trails.borrow().iter() {
                let Some(first) = trail.points.first() else { continue };
                let path = NSBezierPath::bezierPath();
                path.setLineWidth(5.0);
                path.setLineCapStyle(NSLineCapStyle::Round);
                path.setLineJoinStyle(NSLineJoinStyle::Round);
                path.moveToPoint(*first);
                if trail.points.len() == 1 {
                    path.lineToPoint(*first); // a tap: a dot
                }
                for p in trail.points.iter().skip(1) {
                    path.lineToPoint(*p);
                }
                trail.color.setStroke();
                path.stroke();
            }
            let label_attributes = NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(
                &[unsafe { NSFontAttributeName }, unsafe { NSForegroundColorAttributeName }],
                &[
                    &*NSFont::boldSystemFontOfSize(13.0),
                    &*NSColor::labelColor(),
                ],
            );
            for (session, p) in ivars.live_fingers.borrow().iter() {
                color(*session)
                    .colorWithAlphaComponent(0.3)
                    .setFill();
                NSBezierPath::bezierPathWithOvalInRect(CGRect::new(
                    CGPoint::new(p.x - 20.0, p.y - 20.0),
                    CGSize::new(40.0, 40.0),
                ))
                .fill();
                let digit = NSString::from_str(&session.to_string());
                // `drawAtPoint:withAttributes:` (NSStringDrawing) is not in the bindings.
                let _: () = unsafe {
                    msg_send![&digit,
                        drawAtPoint: CGPoint::new(p.x - 8.0, p.y - 9.0),
                        withAttributes: &*label_attributes,
                    ]
                };
            }
            let status = NSString::from_str(&format!(
                "Fingers now: {} · most at once: {}    (Touch Mode → Multitouch paints every finger in colour; Drawing paints the mouse in grey)",
                ivars.live_fingers.borrow().len(),
                ivars.most_fingers.get(),
            ));
            let _: () = unsafe {
                msg_send![&status,
                    drawAtPoint: CGPoint::new(12.0, 8.0),
                    withAttributes: &*label_attributes,
                ]
            };
            let line_attributes = NSDictionary::<NSAttributedStringKey, AnyObject>::from_slices(
                &[unsafe { NSFontAttributeName }, unsafe { NSForegroundColorAttributeName }],
                &[
                    &*NSFont::monospacedSystemFontOfSize_weight(12.0, 0.0),
                    &*NSColor::secondaryLabelColor(),
                ],
            );
            for (i, line) in ivars.lines.borrow().iter().enumerate() {
                let text = NSString::from_str(line);
                let _: () = unsafe {
                    msg_send![&text,
                        drawAtPoint: CGPoint::new(12.0, 30.0 + i as f64 * 17.0),
                        withAttributes: &*line_attributes,
                    ]
                };
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            self.paint_mouse(e, true, false);
            self.record(format!("{} clicks={}", self.describe("mouseDown", e), e.clickCount()));
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            self.paint_mouse(e, false, true);
            self.record(format!("{} clicks={}", self.describe("mouseUp", e), e.clickCount()));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            self.paint_mouse(e, false, false);
            self.record(self.describe("mouseDragged", e));
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, e: &NSEvent) {
            self.record(self.describe("rightMouseDown", e));
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, e: &NSEvent) {
            self.record(self.describe("rightMouseUp", e));
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            // What Safari/WebKit does for back/forward: start swipe tracking on a horizontal scroll
            // that begins.
            if e.phase() == NSEventPhase::Began
                && e.scrollingDeltaX().abs() > e.scrollingDeltaY().abs()
            {
                self.record(format!(
                    "SWIPE start? tracking-enabled={} dx={:.1}",
                    NSEvent::isSwipeTrackingFromScrollEventsEnabled(),
                    e.scrollingDeltaX(),
                ));
                if NSEvent::isSwipeTrackingFromScrollEventsEnabled() {
                    let reported = Rc::new(Cell::new(0usize));
                    // SAFETY: the view is alive (this call holds a reference to it), so the
                    // retain cannot fail.
                    let this = unsafe { Retained::retain(NonNull::from(self).as_ptr()) }
                        .expect("the sink view is alive");
                    let block = RcBlock::new(
                        move |amount: CGFloat,
                              phase: NSEventPhase,
                              complete: objc2::runtime::Bool,
                              _unused: *mut objc2::runtime::Bool| {
                            let complete = complete.as_bool();
                            let seen = reported.get() + 1;
                            reported.set(seen);
                            if seen <= 3
                                || complete
                                || phase == NSEventPhase::Ended
                                || phase == NSEventPhase::Cancelled
                            {
                                this.record(format!(
                                    "SWIPE tracking amount={:.2} phase={} complete={}",
                                    amount,
                                    phase.bits(),
                                    if complete { "y" } else { "n" },
                                ));
                            }
                        },
                    );
                    // `trackSwipeEventWithOptions:dampenAmountThresholdMin:max:usingHandler:` is not
                    // in the bindings.
                    let _: () = unsafe {
                        msg_send![e,
                            trackSwipeEventWithOptions: NSEventSwipeTrackingOptions::LockDirection
                                | NSEventSwipeTrackingOptions::ClampGestureAmount,
                            dampenAmountThresholdMin: -1.0_f64,
                            max: 1.0_f64,
                            usingHandler: &*block,
                        ]
                    };
                }
            }
            self.record(format!(
                "{} dx={:.1} dy={:.1} precise={} phase={} momentum={}",
                self.describe("scroll", e),
                e.scrollingDeltaX(),
                e.scrollingDeltaY(),
                if e.hasPreciseScrollingDeltas() { "y" } else { "n" },
                e.phase().bits(),
                e.momentumPhase().bits(),
            ));
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, e: &NSEvent) {
            self.record(format!(
                "{} m={:.3} phase={}",
                self.describe("magnify", e),
                e.magnification(),
                e.phase().bits(),
            ));
        }

        #[unsafe(method(rotateWithEvent:))]
        fn rotate_with_event(&self, e: &NSEvent) {
            self.record(format!(
                "{} r={:.2} phase={}",
                self.describe("rotate", e),
                e.rotation(),
                e.phase().bits(),
            ));
        }

        #[unsafe(method(swipeWithEvent:))]
        fn swipe_with_event(&self, e: &NSEvent) {
            self.record(format!(
                "{} dx={:.0} dy={:.0}",
                self.describe("swipe", e),
                e.deltaX(),
                e.deltaY(),
            ));
        }
    }

    unsafe impl NSObjectProtocol for SinkView {}
);

impl SinkView {
    /// Creates the view with its log and the touch display's frame.
    fn new(mtm: MainThreadMarker, frame: CGRect, log: Arc<FileLog>, touch_screen: CGRect) -> Retained<Self> {
        // SAFETY: the instance variables are initialized; the class inherits `initWithFrame:`.
        unsafe {
            msg_send![
                super(SinkView::alloc(mtm).set_ivars(SinkViewIvars {
                    log,
                    touch_screen,
                    lines: RefCell::new(Vec::new()),
                    trails: RefCell::new(Vec::new()),
                    live_trail: RefCell::new(HashMap::new()),
                    live_fingers: RefCell::new(HashMap::new()),
                    mouse_trail: RefCell::new(None),
                    most_fingers: Cell::new(0),
                })),
                initWithFrame: frame
            ]
        }
    }

    /// One TUIO frame: extends each finger's trail, ends the trails of fingers that lifted.
    fn show(&self, frame: &Frame) {
        let Some(window) = self.window() else { return };
        let ivars = self.ivars();
        {
            let mut live_fingers = ivars.live_fingers.borrow_mut();
            let mut live_trail = ivars.live_trail.borrow_mut();
            live_fingers.retain(|session, _| frame.alive.contains(session));
            live_trail.retain(|session, _| frame.alive.contains(session));
            let s = ivars.touch_screen;
            for cursor in &frame.cursors {
                let on_screen = CGPoint::new(
                    s.origin.x + cursor.x as f64 * s.size.width,
                    s.max().y - cursor.y as f64 * s.size.height,
                );
                let p = self.convertPoint_fromView(window.convertPointFromScreen(on_screen), None);
                live_fingers.insert(cursor.session, p);
                match live_trail.get(&cursor.session) {
                    Some(&index) => ivars.trails.borrow_mut()[index].points.push(p),
                    None => {
                        let mut trails = ivars.trails.borrow_mut();
                        trails.push(Trail { color: color(cursor.session), points: vec![p] });
                        live_trail.insert(cursor.session, trails.len() - 1);
                    }
                }
            }
        }
        let live = ivars.live_fingers.borrow().len();
        if live > ivars.most_fingers.get() {
            ivars.most_fingers.set(live);
            ivars.log.write(&format!("TUIO: {live} fingers at once"));
        }
        self.setNeedsDisplay(true);
    }

    /// Extends or starts the mouse's grey trail for a drag event.
    fn paint_mouse(&self, e: &NSEvent, starting: bool, ending: bool) {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let ivars = self.ivars();
        let mut trails = ivars.trails.borrow_mut();
        let mut mouse_trail = ivars.mouse_trail.borrow_mut();
        if starting || mouse_trail.is_none() {
            *mouse_trail = Some(trails.len());
            trails.push(Trail { color: NSColor::systemGrayColor(), points: vec![p] });
        } else if let Some(index) = *mouse_trail {
            trails[index].points.push(p);
        }
        if ending {
            *mouse_trail = None;
        }
    }

    /// Where and how the event landed: the view's coordinates, the app's and window's state, and
    /// the modifier keys.
    fn describe(&self, name: &str, e: &NSEvent) -> String {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let state = format!(
            "{}/{}",
            if NSApp(self.mtm()).isActive() { "active" } else { "inactive" },
            if self.window().is_some_and(|window| window.isKeyWindow()) { "key" } else { "not-key" },
        );
        let modifiers = e.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
        let mods = if modifiers.is_empty() {
            String::new()
        } else {
            format!(" mods={}", modifiers.bits())
        };
        format!("{name} at {:.0},{:.0} [{state}]{mods}", p.x, p.y)
    }

    /// One diagnostic line: to the log, on screen, and the oldest one out.
    fn record(&self, line: String) {
        let ivars = self.ivars();
        ivars.log.write(&line);
        {
            let mut lines = ivars.lines.borrow_mut();
            lines.push(line);
            if lines.len() > 30 {
                let excess = lines.len() - 30;
                lines.drain(0..excess);
            }
        }
        self.setNeedsDisplay(true);
    }
}

/// The trail colour for one finger session, cycling through the palette.
fn color(session: i32) -> Retained<NSColor> {
    palette(session as u32 as usize)
}

/// The palette the trails cycle through, one colour per finger.
fn palette(index: usize) -> Retained<NSColor> {
    match index % 10 {
        0 => NSColor::systemRedColor(),
        1 => NSColor::systemOrangeColor(),
        2 => NSColor::systemYellowColor(),
        3 => NSColor::systemGreenColor(),
        4 => NSColor::systemMintColor(),
        5 => NSColor::systemTealColor(),
        6 => NSColor::systemBlueColor(),
        7 => NSColor::systemIndigoColor(),
        8 => NSColor::systemPurpleColor(),
        _ => NSColor::systemPinkColor(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_g_matches_c_for_the_values_the_fields_carry() {
        assert_eq!(format_g(1.5, 4), "1.5");
        assert_eq!(format_g(33231.0, 4), "3.323e+04");
        assert_eq!(format_g(-36.0, 4), "-36");
        assert_eq!(format_g(-0.5, 4), "-0.5");
        assert_eq!(format_g(0.0222, 4), "0.0222");
        assert_eq!(format_g(100.0, 4), "100");
        assert_eq!(format_g(4.0, 4), "4");
    }
}
