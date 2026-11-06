//! About / user manual window (one at a time).

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSFont, NSTextView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::manual::manual_text;

thread_local! {
    static ABOUT: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
}

const W: f64 = 640.0;
const H: f64 = 560.0;

pub fn show(mtm: MainThreadMarker) {
    if let Some(w) = ABOUT.with(|a| a.borrow().clone()) {
        w.makeKeyAndOrderFront(None);
        return;
    }
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::Miniaturizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("About RDP Launcher"));
    let Some(content) = window.contentView() else { return };

    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setFrame(NSRect::new(NSPoint::new(0.0, 52.0), NSSize::new(W, H - 52.0)));
    scroll.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable);
    if let Some(doc) = scroll.documentView() {
        if let Ok(text) = doc.downcast::<NSTextView>() {
            text.setString(&NSString::from_str(&manual_text(env!("CARGO_PKG_VERSION"))));
            text.setEditable(false);
            text.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(12.0, 0.0)));
            text.setTextContainerInset(NSSize::new(12.0, 10.0));
        }
    }
    content.addSubview(&scroll);

    let target: &AnyObject = window.as_ref();
    let ok = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("OK"), Some(target), Some(sel!(performClose:)), mtm)
    };
    ok.setKeyEquivalent(&NSString::from_str("\r"));
    ok.setFrame(NSRect::new(NSPoint::new(W - 110.0, 12.0), NSSize::new(90.0, 30.0)));
    ok.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinXMargin);
    content.addSubview(&ok);

    window.center();
    window.makeKeyAndOrderFront(None);
    #[cfg(debug_assertions)]
    crate::dev::snapshot_if_requested(&content, "about");
    ABOUT.with(|a| *a.borrow_mut() = Some(window));
}
