//! "Password for user on Server" prompt, with an option to remember it in the Keychain.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSButton, NSControlStateValueOn, NSSecureTextField, NSView, NSWindow,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// Returns (password, remember) or None if cancelled.
pub fn ask(
    mtm: MainThreadMarker,
    _parent: Option<&NSWindow>,
    connection_name: &str,
    username: &str,
) -> Option<(String, bool)> {
    let alert = NSAlert::new(mtm);
    let who = if username.is_empty() { "this account".to_owned() } else { username.to_owned() };
    alert.setMessageText(&NSString::from_str(&format!("Password for {who} on {connection_name}")));
    alert.setInformativeText(&NSString::from_str("Enter the password to sign in to the remote computer."));
    alert.addButtonWithTitle(&NSString::from_str("Connect"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));

    let accessory = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 56.0)));
    let field = NSSecureTextField::initWithFrame(
        NSSecureTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 30.0), NSSize::new(260.0, 24.0)),
    );
    let remember: Retained<NSButton> = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str("Remember in Keychain"), None, None, mtm)
    };
    remember.setState(NSControlStateValueOn);
    remember.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 22.0)));
    accessory.addSubview(&field);
    accessory.addSubview(&remember);
    alert.setAccessoryView(Some(&accessory));
    alert.window().setInitialFirstResponder(Some(&field));

    if alert.runModal() != NSAlertFirstButtonReturn {
        return None;
    }
    Some((field.stringValue().to_string(), remember.state() == NSControlStateValueOn))
}
