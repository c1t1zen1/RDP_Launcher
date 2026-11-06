//! The window(s) showing one RDP session.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationPresentationOptions, NSBackingStoreType, NSResponder, NSScreen, NSWindow,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer};

use crate::model::DisplayMode;
use crate::monitors::{monitor_region, DisplayInfo, Rect, RemoteMonitor};
use crate::session::engine::{PointerUpdate, SessionHandle};
use crate::session::framebuffer::Region;
use crate::session::geometry::{remote_size, ViewMapping};
use crate::session::view::{make_cg_image, RdpView};

/// Wait this long after the last window resize before asking the server to resize.
const RESIZE_DEBOUNCE_SECS: f64 = 0.3;

pub const WINDOWED_W: f64 = 1280.0;
pub const WINDOWED_H: f64 = 800.0;

pub struct DelegateIvars {
    handle: SessionHandle,
    views: RefCell<Vec<Retained<RdpView>>>,
    on_closed: RefCell<Option<Box<dyn Fn()>>>,
    /// Single-window sessions follow the window size; multi-monitor ones don't.
    follows_window: Cell<bool>,
    resize_timer: RefCell<Option<Retained<NSTimer>>>,
    last_size: Cell<(u16, u16)>,
    /// Multi-monitor: hide the menu bar and Dock while a session window is in front.
    cover_displays: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherSessionWindowDelegate"]
    #[ivars = DelegateIvars]
    pub struct SessionWindowDelegate;

    unsafe impl NSObjectProtocol for SessionWindowDelegate {}

    unsafe impl NSWindowDelegate for SessionWindowDelegate {
        /// Closing a session window disconnects the session (like mstsc).
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> bool {
            self.ivars().handle.disconnect();
            true
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(cb) = self.ivars().on_closed.borrow_mut().take() {
                cb();
            }
        }

        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            for view in self.ivars().views.borrow().iter() {
                view.release_input();
            }
            if self.ivars().cover_displays.get() {
                set_presentation(MainThreadMarker::from(self), false);
            }
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &NSNotification) {
            if self.ivars().cover_displays.get() {
                set_presentation(MainThreadMarker::from(self), true);
            }
        }

        /// Restart the debounce; the server is asked to resize once resizing stops.
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            if !self.ivars().follows_window.get() {
                return;
            }
            if let Some(t) = self.ivars().resize_timer.borrow_mut().take() {
                t.invalidate();
            }
            let timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    RESIZE_DEBOUNCE_SECS,
                    self,
                    sel!(resizeSettled:),
                    None,
                    false,
                )
            };
            *self.ivars().resize_timer.borrow_mut() = Some(timer);
        }
    }

    impl SessionWindowDelegate {
        #[unsafe(method(resizeSettled:))]
        fn resize_settled(&self, _timer: Option<&AnyObject>) {
            self.ivars().resize_timer.borrow_mut().take();
            let Some(view) = self.ivars().views.borrow().first().cloned() else { return };
            let size = view.bounds().size;
            let wanted = remote_size(size.width, size.height);
            if wanted != self.ivars().last_size.get() {
                self.ivars().last_size.set(wanted);
                tracing::info!(width = wanted.0, height = wanted.1, "requesting remote resize");
                self.ivars().handle.resize(wanted.0, wanted.1);
            }
        }
    }
);

impl SessionWindowDelegate {
    fn new(mtm: MainThreadMarker, handle: SessionHandle) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            handle,
            views: RefCell::new(Vec::new()),
            on_closed: RefCell::new(None),
            follows_window: Cell::new(true),
            resize_timer: RefCell::new(None),
            last_size: Cell::new((0, 0)),
            cover_displays: Cell::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY: NSWindow has no extra subclassing requirements; no Drop impl.
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherKeyableWindow"]
    struct KeyableWindow;

    impl KeyableWindow {
        /// Borderless windows can't take keyboard focus unless they say so.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            true
        }
    }
);

/// A borderless window covering `frame` (one display of a multi-monitor session).
fn display_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    let this = KeyableWindow::alloc(mtm).set_ivars(());
    let window: Retained<KeyableWindow> = unsafe {
        msg_send![super(this), initWithContentRect: frame, styleMask: NSWindowStyleMask::Borderless, backing: NSBackingStoreType::Buffered, defer: false]
    };
    let window = Retained::into_super(window);
    unsafe { window.setReleasedWhenClosed(false) };
    window.setCollectionBehavior(NSWindowCollectionBehavior::Managed | NSWindowCollectionBehavior::FullScreenNone);
    window.setAcceptsMouseMovedEvents(true);
    window
}

/// Auto-hide the menu bar and Dock while a multi-monitor session is in front.
fn set_presentation(mtm: MainThreadMarker, cover: bool) {
    #[cfg(debug_assertions)]
    if crate::dev::fake_displays().is_some() {
        return; // fake test displays don't cover anything
    }
    let options = if cover {
        NSApplicationPresentationOptions::AutoHideMenuBar | NSApplicationPresentationOptions::AutoHideDock
    } else {
        NSApplicationPresentationOptions::Default
    };
    NSApplication::sharedApplication(mtm).setPresentationOptions(options);
}

pub struct SessionWindows {
    handle: SessionHandle,
    windows: Vec<(Retained<NSWindow>, Retained<RdpView>)>,
    delegate: Retained<SessionWindowDelegate>,
}

/// Initial windowed size: 1280×800 points, clamped to 90% of the main display.
pub fn windowed_size(mtm: MainThreadMarker) -> (f64, f64) {
    let visible = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame());
    match visible {
        Some(v) => (WINDOWED_W.min(v.size.width * 0.9), WINDOWED_H.min(v.size.height * 0.9)),
        None => (WINDOWED_W, WINDOWED_H),
    }
}

fn ns_rect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

/// A titled, resizable window with `w`×`h` points of content, ready for native full screen.
fn titled_window(mtm: MainThreadMarker, title: &str, w: f64, h: f64) -> Retained<NSWindow> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    window.setAcceptsMouseMovedEvents(true);
    window.setContentMinSize(NSSize::new(400.0, 300.0));
    window
}

fn full_region(w: f64, h: f64) -> Region {
    Region { left: 0, top: 0, right: (w as u16).saturating_sub(1), bottom: (h as u16).saturating_sub(1) }
}

impl SessionWindows {
    /// Open the session's window(s) for `mode`. `layout` is what the server was asked
    /// for; `displays` are the chosen Mac displays (full screen only).
    pub fn open(
        mtm: MainThreadMarker,
        title: &str,
        handle: SessionHandle,
        mode: DisplayMode,
        displays: &[DisplayInfo],
        layout: &[RemoteMonitor],
    ) -> Rc<SessionWindows> {
        let first = layout.first().copied().unwrap_or(RemoteMonitor {
            left: 0,
            top: 0,
            width: WINDOWED_W as u32,
            height: WINDOWED_H as u32,
            primary: true,
        });
        if mode == DisplayMode::Fullscreen && displays.len() > 1 && layout.len() == displays.len() {
            return Self::open_spanning(mtm, title, handle, displays, layout);
        }
        let (w, h) = (f64::from(first.width), f64::from(first.height));
        let window = titled_window(mtm, title, w, h);
        let mapping = ViewMapping { view_w: w, view_h: h, src: full_region(w, h) };
        let view = RdpView::new(mtm, handle.clone(), mapping);
        window.setContentView(Some(&view));

        let delegate = SessionWindowDelegate::new(mtm, handle.clone());
        delegate.ivars().views.borrow_mut().push(view.clone());
        delegate.ivars().last_size.set((first.width as u16, first.height as u16));
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        let fullscreen = mode == DisplayMode::Fullscreen;
        match displays.first() {
            // Put the window on the chosen display so its full-screen Space opens there.
            Some(d) if fullscreen => window.setFrame_display(ns_rect(d.visible_frame), false),
            _ => window.center(),
        }
        window.makeKeyAndOrderFront(None);
        view.make_first_responder();
        if fullscreen {
            window.toggleFullScreen(None);
        }
        Rc::new(SessionWindows { handle, windows: vec![(window, view)], delegate })
    }

    /// One borderless window per display, each showing its monitor's part of the desktop.
    fn open_spanning(
        mtm: MainThreadMarker,
        title: &str,
        handle: SessionHandle,
        displays: &[DisplayInfo],
        layout: &[RemoteMonitor],
    ) -> Rc<SessionWindows> {
        let delegate = SessionWindowDelegate::new(mtm, handle.clone());
        delegate.ivars().follows_window.set(false);
        delegate.ivars().cover_displays.set(true);
        let mut windows = Vec::new();
        for (i, display) in displays.iter().enumerate() {
            let frame = ns_rect(display.frame);
            let window = display_window(mtm, frame);
            window.setTitle(&NSString::from_str(title));
            let mapping = ViewMapping { view_w: frame.size.width, view_h: frame.size.height, src: monitor_region(layout, i) };
            let view = RdpView::new(mtm, handle.clone(), mapping);
            view.set_can_toggle_full_screen(false);
            window.setContentView(Some(&view));
            window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            delegate.ivars().views.borrow_mut().push(view.clone());
            window.makeKeyAndOrderFront(None);
            windows.push((window, view));
        }
        if let Some((window, view)) = windows.first() {
            window.makeKeyAndOrderFront(None);
            view.make_first_responder();
        }
        Rc::new(SessionWindows { handle, windows, delegate })
    }

    pub fn handle(&self) -> &SessionHandle {
        &self.handle
    }

    /// Called once when the (first) window closes.
    pub fn set_on_closed(&self, cb: Box<dyn Fn()>) {
        *self.delegate.ivars().on_closed.borrow_mut() = Some(cb);
    }

    /// Redraw from the shared framebuffer if it changed since the last redraw.
    pub fn present_frame(&self) {
        let fb = self.handle.framebuffer();
        let image = {
            let mut fb = fb.lock().expect("framebuffer");
            if !fb.take_dirty() {
                return;
            }
            let full = Region {
                left: 0,
                top: 0,
                right: fb.width().saturating_sub(1),
                bottom: fb.height().saturating_sub(1),
            };
            // Single-window sessions show the whole (possibly resized) desktop.
            if self.windows.len() == 1 {
                let view = &self.windows[0].1;
                let bounds = view.bounds();
                view.set_mapping(ViewMapping { view_w: bounds.size.width, view_h: bounds.size.height, src: full });
            }
            (make_cg_image(fb.pixels(), fb.width(), fb.height()), fb.width(), fb.height())
        };
        if let (Some(img), w, h) = image {
            #[cfg(debug_assertions)]
            crate::dev::snapshot_frame(&img);
            for (_, view) in &self.windows {
                view.present(&img, w, h);
            }
            #[cfg(debug_assertions)]
            for (i, (_, view)) in self.windows.iter().enumerate() {
                crate::dev::snapshot_layer(view, &format!("window{i}"));
            }
        }
    }

    pub fn set_pointer(&self, update: &PointerUpdate) {
        for (_, view) in &self.windows {
            view.set_pointer(update);
        }
    }

    pub fn focus(&self) {
        for (window, _) in &self.windows {
            window.makeKeyAndOrderFront(None);
        }
    }

    /// Close every window without asking (the session already ended).
    pub fn close_all(&self) {
        for (window, _) in &self.windows {
            window.close();
        }
    }

    pub fn windows(&self) -> Vec<Retained<NSWindow>> {
        self.windows.iter().map(|(w, _)| w.clone()).collect()
    }
}
