//! NSApplication setup: delegate, main menu, shared tokio runtime.

use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply, NSEventModifierFlags,
    NSMenu, NSMenuItem,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// The tokio runtime every RDP session shares.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("rdp-io")
            .build()
            .expect("tokio runtime")
    })
}

/// Run `f` on the main thread (from any thread).
pub fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("main queue runs on the main thread");
        f(mtm);
    });
}

thread_local! {
    static DEFERRED: std::cell::RefCell<Vec<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f` on the main thread after the current AppKit call returns — e.g. to tear down
/// a window and its delegate from inside that delegate's own callback.
pub fn defer_main(f: impl FnOnce() + 'static) {
    DEFERRED.with(|q| q.borrow_mut().push(Box::new(f)));
    on_main(|_| run_deferred());
}

/// Run everything queued by [`defer_main`] (normally called from the main queue).
pub fn run_deferred() {
    loop {
        let batch = DEFERRED.with(|q| std::mem::take(&mut *q.borrow_mut()));
        if batch.is_empty() {
            return;
        }
        for f in batch {
            f();
        }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherAppDelegate"]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            crate::controller::start(mtm);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            false
        }

        /// Clicking the Dock icon brings the launcher back.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _has_visible: bool) -> bool {
            crate::controller::show_launcher();
            true
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            if crate::controller::confirm_quit(MainThreadMarker::from(self)) {
                NSApplicationTerminateReply::TerminateNow
            } else {
                NSApplicationTerminateReply::TerminateCancel
            }
        }
    }

    impl AppDelegate {
        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&objc2::runtime::AnyObject>) {
            crate::launcher::about::show(MainThreadMarker::from(self));
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

fn submenu(mtm: MainThreadMarker, title: &str, items: &[Retained<NSMenuItem>]) -> (Retained<NSMenuItem>, Retained<NSMenu>) {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for i in items {
        menu.addItem(i);
    }
    let top = item(mtm, title, None, "");
    top.setSubmenu(Some(&menu));
    (top, menu)
}

fn build_main_menu(mtm: MainThreadMarker, app: &NSApplication, delegate: &AppDelegate) {
    let main = NSMenu::new(mtm);
    let about = item(mtm, "About RDP Launcher", Some(sel!(showAbout:)), "");
    unsafe { about.setTarget(Some(delegate)) };
    let (app_item, _) = submenu(
        mtm,
        "RDP Launcher",
        &[
            about,
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Hide RDP Launcher", Some(sel!(hide:)), "h"),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Quit RDP Launcher", Some(sel!(terminate:)), "q"),
        ],
    );
    main.addItem(&app_item);
    let redo = item(mtm, "Redo", Some(sel!(redo:)), "z");
    redo.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    let (edit_item, _) = submenu(
        mtm,
        "Edit",
        &[
            item(mtm, "Undo", Some(sel!(undo:)), "z"),
            redo,
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Cut", Some(sel!(cut:)), "x"),
            item(mtm, "Copy", Some(sel!(copy:)), "c"),
            item(mtm, "Paste", Some(sel!(paste:)), "v"),
            item(mtm, "Select All", Some(sel!(selectAll:)), "a"),
        ],
    );
    main.addItem(&edit_item);
    let full_screen = item(mtm, "Toggle Full Screen", Some(sel!(toggleSessionFullScreen:)), "f");
    full_screen.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Control);
    let disconnect = item(mtm, "Disconnect", Some(sel!(disconnectSession:)), "w");
    disconnect.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);
    let (session_item, _) = submenu(
        mtm,
        "Session",
        &[
            full_screen,
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Send Ctrl+Alt+Del", Some(sel!(sendCtrlAltDel:)), ""),
            item(mtm, "Send Windows Key", Some(sel!(sendWindowsKey:)), ""),
            NSMenuItem::separatorItem(mtm),
            disconnect,
        ],
    );
    main.addItem(&session_item);
    let (window_item, window_menu) = submenu(
        mtm,
        "Window",
        &[
            item(mtm, "Minimize", Some(sel!(performMiniaturize:)), "m"),
            item(mtm, "Zoom", Some(sel!(performZoom:)), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Bring All to Front", Some(sel!(arrangeInFront:)), ""),
        ],
    );
    main.addItem(&window_item);
    app.setMainMenu(Some(&main));
    app.setWindowsMenu(Some(&window_menu));
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("run() must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate = AppDelegate::new(mtm);
    build_main_menu(mtm, &app, &delegate);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn defer_main_runs_later_not_now() {
        let token = Rc::new(());
        let held = Rc::clone(&token);
        defer_main(move || drop(held));
        assert_eq!(Rc::strong_count(&token), 2, "work must not run synchronously");
        run_deferred();
        assert_eq!(Rc::strong_count(&token), 1, "work runs once the queue is drained");
    }
}
