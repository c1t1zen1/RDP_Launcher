//! The window(s) showing one RDP session.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSScreen, NSWindow, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::session::engine::{PointerUpdate, SessionHandle};
use crate::session::framebuffer::Region;
use crate::session::geometry::ViewMapping;
use crate::session::view::{make_cg_image, RdpView};

pub const WINDOWED_W: f64 = 1280.0;
pub const WINDOWED_H: f64 = 800.0;

pub struct DelegateIvars {
    handle: SessionHandle,
    views: RefCell<Vec<Retained<RdpView>>>,
    on_closed: RefCell<Option<Box<dyn Fn()>>>,
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
        }
    }
);

impl SessionWindowDelegate {
    fn new(mtm: MainThreadMarker, handle: SessionHandle) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            handle,
            views: RefCell::new(Vec::new()),
            on_closed: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }
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

fn full_region(w: f64, h: f64) -> Region {
    Region { left: 0, top: 0, right: (w as u16).saturating_sub(1), bottom: (h as u16).saturating_sub(1) }
}

impl SessionWindows {
    /// A normal, resizable window of `size` points, centred on the main display.
    pub fn open_windowed(
        mtm: MainThreadMarker,
        title: &str,
        handle: SessionHandle,
        size: (f64, f64),
    ) -> Rc<SessionWindows> {
        let (w, h) = size;
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

        let mapping = ViewMapping { view_w: w, view_h: h, src: full_region(w, h) };
        let view = RdpView::new(mtm, handle.clone(), mapping);
        window.setContentView(Some(&view));

        let delegate = SessionWindowDelegate::new(mtm, handle.clone());
        delegate.ivars().views.borrow_mut().push(view.clone());
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        window.center();
        window.makeKeyAndOrderFront(None);
        view.make_first_responder();

        Rc::new(SessionWindows { handle, windows: vec![(window, view)], delegate })
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
