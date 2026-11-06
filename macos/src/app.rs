//! NSApplication setup: delegate, main menu, shared tokio runtime.

use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuItem};
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
            #[cfg(debug_assertions)]
            crate::dev::start_from_env(mtm);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            false
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

fn build_main_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let main = NSMenu::new(mtm);
    let (app_item, _) = submenu(
        mtm,
        "RDP Launcher",
        &[
            item(mtm, "About RDP Launcher", Some(sel!(orderFrontStandardAboutPanel:)), ""),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Hide RDP Launcher", Some(sel!(hide:)), "h"),
            NSMenuItem::separatorItem(mtm),
            item(mtm, "Quit RDP Launcher", Some(sel!(terminate:)), "q"),
        ],
    );
    main.addItem(&app_item);
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
    build_main_menu(mtm, &app);
    let delegate = AppDelegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app.run();
}
