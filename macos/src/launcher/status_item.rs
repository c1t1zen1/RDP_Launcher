//! Menu-bar icon: click toggles the launcher; right-click (or Ctrl-click) shows
//! Open / About / Quit.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEventMask, NSEventModifierFlags, NSEventType, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

pub struct StatusIvars {
    on_toggle: Box<dyn Fn()>,
    on_about: Box<dyn Fn()>,
    on_quit: Box<dyn Fn()>,
    item: std::cell::RefCell<Option<Retained<NSStatusItem>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherStatusTarget"]
    #[ivars = StatusIvars]
    pub struct StatusTarget;

    unsafe impl NSObjectProtocol for StatusTarget {}

    impl StatusTarget {
        #[unsafe(method(clicked:))]
        fn clicked(&self, _sender: Option<&AnyObject>) {
            let mtm = MainThreadMarker::from(self);
            let event = NSApplication::sharedApplication(mtm).currentEvent();
            let secondary = event.is_some_and(|e| {
                e.r#type() == NSEventType::RightMouseUp || e.modifierFlags().contains(NSEventModifierFlags::Control)
            });
            if secondary {
                self.show_menu(mtm);
            } else {
                (self.ivars().on_toggle)();
            }
        }

        #[unsafe(method(openLauncher:))]
        fn open_launcher(&self, _sender: Option<&AnyObject>) {
            (self.ivars().on_toggle)();
        }

        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) {
            (self.ivars().on_about)();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            (self.ivars().on_quit)();
        }
    }
);

impl StatusTarget {
    fn show_menu(&self, mtm: MainThreadMarker) {
        let menu = NSMenu::new(mtm);
        for (title, action) in [("Open RDP Launcher", sel!(openLauncher:)), ("About RDP Launcher", sel!(showAbout:))] {
            menu.addItem(&self.menu_item(mtm, title, action));
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.menu_item(mtm, "Quit RDP Launcher", sel!(quit:)));
        if let Some(item) = self.ivars().item.borrow().as_ref() {
            // Attach the menu just for this click, then detach so left-click keeps toggling.
            item.setMenu(Some(&menu));
            if let Some(button) = item.button(mtm) {
                unsafe { button.performClick(None) };
            }
            item.setMenu(None);
        }
    }

    fn menu_item(&self, mtm: MainThreadMarker, title: &str, action: objc2::runtime::Sel) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(""),
            )
        };
        unsafe { item.setTarget(Some(self)) };
        item
    }
}

/// Install the menu-bar icon. Keep the returned target alive for the app's lifetime.
pub fn install(
    mtm: MainThreadMarker,
    on_toggle: Box<dyn Fn()>,
    on_about: Box<dyn Fn()>,
    on_quit: Box<dyn Fn()>,
) -> Retained<StatusTarget> {
    let target = StatusTarget::alloc(mtm).set_ivars(StatusIvars {
        on_toggle,
        on_about,
        on_quit,
        item: std::cell::RefCell::new(None),
    });
    let target: Retained<StatusTarget> = unsafe { msg_send![super(target), init] };
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    if let Some(button) = item.button(mtm) {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str("display"),
            Some(&NSString::from_str("RDP Launcher")),
        );
        if let Some(image) = &image {
            image.setTemplate(true);
        }
        button.setImage(image.as_deref());
        button.setToolTip(Some(&NSString::from_str("RDP Launcher\nClick to show or hide, right-click for more")));
        unsafe {
            button.setTarget(Some(&target));
            button.setAction(Some(sel!(clicked:)));
        }
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        tracing::info!(has_image = image.is_some(), "menu-bar icon installed");
    } else {
        tracing::warn!("menu-bar icon has no button");
    }
    *target.ivars().item.borrow_mut() = Some(item);
    target
}
