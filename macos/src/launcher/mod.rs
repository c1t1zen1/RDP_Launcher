//! The launcher window: header, connection buttons, + strip. Owns no data —
//! the app passes in what to show and receives the user's actions.

pub mod edit_dialog;
pub mod geometry;
pub mod list_view;
pub mod palette;
pub mod password_prompt;

use std::rc::{Rc, Weak};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSApplication, NSBackingStoreType, NSFloatingWindowLevel, NSNormalWindowLevel, NSScreen, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use geometry::{content_height, CONTENT_W, MAX_HEIGHT_FRACTION};
use list_view::{Action, ConnectionListView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    Idle,
    Connecting,
    Connected,
}

pub struct LauncherCallbacks {
    pub connect: Box<dyn Fn(usize)>,
    pub edit: Box<dyn Fn(usize)>,
    pub duplicate: Box<dyn Fn(usize)>,
    pub delete: Box<dyn Fn(usize)>,
    pub add: Box<dyn Fn()>,
    pub reordered: Box<dyn Fn(usize, usize)>,
    /// The pin (always-on-top) was toggled; persist the new value.
    pub pin_changed: Box<dyn Fn(bool)>,
    pub about: Box<dyn Fn()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherLauncherWindowDelegate"]
    pub struct LauncherWindowDelegate;

    unsafe impl NSObjectProtocol for LauncherWindowDelegate {}

    unsafe impl NSWindowDelegate for LauncherWindowDelegate {
        /// The close button only hides the launcher; quit from the menu-bar icon or ⌘Q.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            sender.orderOut(None);
            false
        }
    }
);

impl LauncherWindowDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

pub struct LauncherController {
    window: Retained<NSWindow>,
    list: Retained<ConnectionListView>,
    _delegate: Retained<LauncherWindowDelegate>,
    callbacks: LauncherCallbacks,
    pinned: std::cell::Cell<bool>,
}

impl LauncherController {
    pub fn new(mtm: MainThreadMarker, callbacks: LauncherCallbacks, always_on_top: bool) -> Rc<Self> {
        let height = content_height(0);
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(CONTENT_W, height));
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
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
        window.setTitle(&NSString::from_str("RDP Launcher"));
        // The launcher keeps the Windows app's light look even in Dark Mode.
        window.setAppearance(NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }).as_deref());
        window.setAcceptsMouseMovedEvents(true);
        let list = ConnectionListView::new(mtm, rect);
        window.setContentView(Some(&list));
        let delegate = LauncherWindowDelegate::new(mtm);
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        window.center();

        let this = Rc::new(LauncherController {
            window,
            list,
            _delegate: delegate,
            callbacks,
            pinned: std::cell::Cell::new(always_on_top),
        });
        let weak: Weak<LauncherController> = Rc::downgrade(&this);
        this.list.set_on_action(Rc::new(move |action| {
            if let Some(me) = weak.upgrade() {
                me.handle(action);
            }
        }));
        this.apply_pin();
        this
    }

    fn handle(&self, action: Action) {
        let cb = &self.callbacks;
        match action {
            Action::Connect(i) => (cb.connect)(i),
            Action::Edit(i) => (cb.edit)(i),
            Action::Duplicate(i) => (cb.duplicate)(i),
            Action::Delete(i) => (cb.delete)(i),
            Action::Add => (cb.add)(),
            Action::Reordered { from, to } => (cb.reordered)(from, to),
            Action::Minimize => self.hide(),
            Action::TogglePin => {
                let on = !self.pinned.get();
                self.set_always_on_top(on);
                (cb.pin_changed)(on);
            }
            Action::About => (cb.about)(),
        }
    }

    pub fn set_items(&self, items: Vec<(String, String, ItemStatus)>) {
        let n = items.len();
        self.autosize(n);
        self.list.set_items(items);
    }

    pub fn set_status(&self, index: usize, status: ItemStatus) {
        self.list.set_status(index, status);
    }

    pub fn show(&self) {
        let mtm = MainThreadMarker::from(&*self.window);
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn hide(&self) {
        self.window.orderOut(None);
    }

    pub fn is_visible(&self) -> bool {
        self.window.isVisible() && !self.window.isMiniaturized()
    }

    pub fn toggle(&self) {
        if self.is_visible() && self.window.isKeyWindow() {
            self.hide();
        } else {
            if self.window.isMiniaturized() {
                self.window.deminiaturize(None);
            }
            self.show();
        }
    }

    pub fn set_always_on_top(&self, on: bool) {
        self.pinned.set(on);
        self.apply_pin();
    }

    fn apply_pin(&self) {
        let on = self.pinned.get();
        self.window.setLevel(if on { NSFloatingWindowLevel } else { NSNormalWindowLevel });
        self.list.set_pinned(on);
    }

    pub fn window(&self) -> &NSWindow {
        &self.window
    }

    /// Grow/shrink with the list, keeping the window's top edge in place.
    fn autosize(&self, n: usize) {
        let mtm = MainThreadMarker::from(&*self.window);
        let screen = self.window.screen().or_else(|| NSScreen::mainScreen(mtm));
        let max_h = screen.map(|s| s.visibleFrame().size.height * MAX_HEIGHT_FRACTION).unwrap_or(800.0);
        let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(CONTENT_W, content_height(n).min(max_h)));
        let mut frame = self.window.frameRectForContentRect(content);
        let old = self.window.frame();
        frame.origin.x = old.origin.x;
        frame.origin.y = old.origin.y + old.size.height - frame.size.height;
        self.window.setFrame_display(frame, true);
        self.list.snap_springs();
    }
}
