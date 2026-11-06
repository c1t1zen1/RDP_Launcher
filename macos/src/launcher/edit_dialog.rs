//! Add / edit connection dialog.

use std::path::PathBuf;

use crate::model::{parse_host, Connection, DisplayMode, HostError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordChange {
    Keep,
    Set(String),
    Clear,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EditResult {
    pub connection: Connection,
    pub password: PasswordChange,
}

/// Raw values read from the dialog's controls.
#[derive(Clone, Debug)]
pub struct FormValues {
    pub name: String,
    pub host: String,
    pub username: String,
    pub password: String,
    pub clear_password: bool,
    pub display: DisplayMode,
    /// Keys of the ticked displays, in the order they are listed.
    pub monitors_checked: Vec<String>,
    pub main_key: String,
    pub shared_folder: Option<PathBuf>,
    pub sound: bool,
}

/// Validate the form and build the updated connection (id kept; trust reset if the host changed).
pub fn compose_result(initial: &Connection, f: FormValues) -> Result<EditResult, HostError> {
    parse_host(&f.host)?;
    let host = f.host.trim().to_owned();
    let name = match f.name.trim() {
        "" => host.clone(),
        n => n.to_owned(),
    };
    let only_main = f.monitors_checked.is_empty() || f.monitors_checked == [f.main_key.clone()];
    let cert_fingerprint = if host == initial.host.trim() { initial.cert_fingerprint.clone() } else { None };
    let connection = Connection {
        id: initial.id.clone(),
        name,
        host,
        username: f.username.trim().to_owned(),
        display: f.display,
        monitors: if only_main { Vec::new() } else { f.monitors_checked },
        shared_folder: f.shared_folder,
        sound: f.sound,
        cert_fingerprint,
    };
    let password = if !f.password.is_empty() {
        PasswordChange::Set(f.password)
    } else if f.clear_password {
        PasswordChange::Clear
    } else {
        PasswordChange::Keep
    };
    Ok(EditResult { connection, password })
}


// ---------- AppKit dialog ----------

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSModalResponseOK, NSOpenPanel, NSPanel, NSPopUpButton, NSSecureTextField, NSTextField, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::monitors::DisplayInfo;

const DIALOG_W: f64 = 440.0;
const PAD: f64 = 20.0;
const FIELD_W: f64 = DIALOG_W - 2.0 * PAD;
const LABEL_H: f64 = 17.0;
const FIELD_H: f64 = 24.0;
const ROW_GAP: f64 = 10.0;
const RESPONSE_SAVE: isize = 1;
const RESPONSE_CANCEL: isize = 0;

struct Controls {
    name: Retained<NSTextField>,
    host: Retained<NSTextField>,
    username: Retained<NSTextField>,
    password: Retained<NSSecureTextField>,
    clear_password: Option<Retained<NSButton>>,
    display: Retained<NSPopUpButton>,
    monitors: Vec<(String, Retained<NSButton>)>,
    folder: Retained<NSPopUpButton>,
    sound: Retained<NSButton>,
    error: Retained<NSTextField>,
}

pub struct TargetIvars {
    initial: Connection,
    main_key: String,
    controls: RefCell<Option<Controls>>,
    folder_path: RefCell<Option<PathBuf>>,
    result: RefCell<Option<EditResult>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherEditDialogTarget"]
    #[ivars = TargetIvars]
    struct EditDialogTarget;

    unsafe impl NSObjectProtocol for EditDialogTarget {}

    impl EditDialogTarget {
        #[unsafe(method(save:))]
        fn save(&self, _sender: Option<&AnyObject>) {
            let values = match self.ivars().controls.borrow().as_ref() {
                Some(c) => self.read(c),
                None => return,
            };
            match compose_result(&self.ivars().initial, values) {
                Ok(result) => {
                    *self.ivars().result.borrow_mut() = Some(result);
                    NSApplication::sharedApplication(MainThreadMarker::from(self)).stopModalWithCode(RESPONSE_SAVE);
                }
                Err(e) => {
                    if let Some(c) = self.ivars().controls.borrow().as_ref() {
                        c.error.setStringValue(&NSString::from_str(&e.to_string()));
                    }
                }
            }
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(MainThreadMarker::from(self)).stopModalWithCode(RESPONSE_CANCEL);
        }

        #[unsafe(method(displayChanged:))]
        fn display_changed(&self, _sender: Option<&AnyObject>) {
            self.update_monitor_enablement();
        }

        #[unsafe(method(folderChanged:))]
        fn folder_changed(&self, _sender: Option<&AnyObject>) {
            let Some(index) = self.ivars().controls.borrow().as_ref().map(|c| c.folder.indexOfSelectedItem()) else {
                return;
            };
            let has_path = self.ivars().folder_path.borrow().is_some();
            let choose_index = if has_path { 2 } else { 1 };
            if index == 0 {
                *self.ivars().folder_path.borrow_mut() = None;
            } else if index == choose_index {
                if let Some(path) = choose_folder(MainThreadMarker::from(self)) {
                    *self.ivars().folder_path.borrow_mut() = Some(path);
                }
            }
            self.rebuild_folder_menu();
        }
    }
);

impl EditDialogTarget {
    fn read(&self, c: &Controls) -> FormValues {
        let text = |f: &NSTextField| f.stringValue().to_string();
        let on = |b: &NSButton| b.state() == NSControlStateValueOn;
        FormValues {
            name: text(&c.name),
            host: text(&c.host),
            username: text(&c.username),
            password: c.password.stringValue().to_string(),
            clear_password: c.clear_password.as_deref().is_some_and(on),
            display: if c.display.indexOfSelectedItem() == 1 { DisplayMode::Windowed } else { DisplayMode::Fullscreen },
            monitors_checked: c.monitors.iter().filter(|(_, b)| on(b)).map(|(k, _)| k.clone()).collect(),
            main_key: self.ivars().main_key.clone(),
            shared_folder: self.ivars().folder_path.borrow().clone(),
            sound: on(&c.sound),
        }
    }

    fn update_monitor_enablement(&self) {
        if let Some(c) = self.ivars().controls.borrow().as_ref() {
            let fullscreen = c.display.indexOfSelectedItem() == 0;
            for (_, b) in &c.monitors {
                b.setEnabled(fullscreen);
            }
        }
    }

    /// Items: None, [current path], Choose…
    fn rebuild_folder_menu(&self) {
        let path = self.ivars().folder_path.borrow().clone();
        if let Some(c) = self.ivars().controls.borrow().as_ref() {
            c.folder.removeAllItems();
            c.folder.addItemWithTitle(&NSString::from_str("None"));
            if let Some(p) = &path {
                c.folder.addItemWithTitle(&NSString::from_str(&p.display().to_string()));
            }
            c.folder.addItemWithTitle(&NSString::from_str("Choose…"));
            c.folder.selectItemAtIndex(if path.is_some() { 1 } else { 0 });
        }
    }
}

fn choose_folder(mtm: MainThreadMarker) -> Option<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(false);
    panel.setAllowsMultipleSelection(false);
    panel.setPrompt(Some(&NSString::from_str("Share Folder")));
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    panel.URLs().firstObject().and_then(|url| url.path()).map(|p| PathBuf::from(p.to_string()))
}

/// Lays controls out top-to-bottom, then converts to AppKit's bottom-left origin once
/// the total height is known.
struct Column {
    placed: Vec<(Retained<NSView>, f64, f64)>, // (view, top, height)
    y: f64,
}

impl Column {
    fn new() -> Self {
        Column { placed: Vec::new(), y: PAD }
    }

    fn label(&mut self, mtm: MainThreadMarker, text: &str) {
        let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
        self.y += 4.0; // a little extra air above each label
        self.placed.push((Retained::into_super(Retained::into_super(l)), self.y, LABEL_H));
        self.y += LABEL_H + 3.0;
    }

    fn add(&mut self, control: &NSView, h: f64) {
        self.placed.push((control.retain(), self.y, h));
        self.y += h + ROW_GAP;
    }

    /// Position everything in `content`; returns the height used (excluding the bottom pad).
    fn finish(self, content: &NSView, height: f64) {
        for (view, top, h) in self.placed {
            view.setFrame(NSRect::new(NSPoint::new(PAD, height - top - h), NSSize::new(FIELD_W, h)));
            content.addSubview(&view);
        }
    }
}

fn text_field(mtm: MainThreadMarker, value: &str) -> Retained<NSTextField> {
    NSTextField::textFieldWithString(&NSString::from_str(value), mtm)
}

fn checkbox(mtm: MainThreadMarker, title: &str, on: bool) -> Retained<NSButton> {
    let b = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), None, None, mtm) };
    b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    b
}

fn button(mtm: MainThreadMarker, title: &str, target: &AnyObject, action: Sel, key: &str) -> Retained<NSButton> {
    let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm) };
    b.setKeyEquivalent(&NSString::from_str(key));
    b
}

fn popup(mtm: MainThreadMarker, titles: &[&str], selected: isize, target: &AnyObject, action: Sel) -> Retained<NSPopUpButton> {
    let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for t in titles {
        p.addItemWithTitle(&NSString::from_str(t));
    }
    p.selectItemAtIndex(selected);
    unsafe {
        p.setTarget(Some(target));
        p.setAction(Some(action));
    }
    p
}

/// Show the dialog modally. `displays` are the connected Mac displays.
pub fn show(
    mtm: MainThreadMarker,
    title: &str,
    initial: &Connection,
    has_password: bool,
    displays: &[DisplayInfo],
) -> Option<EditResult> {
    let main_key = displays.iter().find(|d| d.is_main).map(|d| d.key.clone()).unwrap_or_default();
    let target = EditDialogTarget::alloc(mtm).set_ivars(TargetIvars {
        initial: initial.clone(),
        main_key: main_key.clone(),
        controls: RefCell::new(None),
        folder_path: RefCell::new(initial.shared_folder.clone()),
        result: RefCell::new(None),
    });
    let target: Retained<EditDialogTarget> = unsafe { msg_send![super(target), init] };
    let t: &AnyObject = target.as_ref();

    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(DIALOG_W, 400.0)),
        NSWindowStyleMask::Titled,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(&NSString::from_str(title));
    let content = panel.contentView()?;
    let mut col = Column::new();

    col.label(mtm, "Name");
    let name = text_field(mtm, &initial.name);
    name.setPlaceholderString(Some(&NSString::from_str("e.g. Office PC")));
    col.add(&name, FIELD_H);

    col.label(mtm, "Host / IP  (optionally host:port)");
    let host = text_field(mtm, &initial.host);
    host.setPlaceholderString(Some(&NSString::from_str("e.g. 192.168.1.50 or server:3390")));
    col.add(&host, FIELD_H);

    col.label(mtm, "Username  (DOMAIN\\user or user)");
    let username = text_field(mtm, &initial.username);
    username.setPlaceholderString(Some(&NSString::from_str("optional")));
    col.add(&username, FIELD_H);

    col.label(mtm, "Password");
    let password = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), NSRect::ZERO);
    let hint = if has_password { "Saved — leave blank to keep" } else { "optional; leave blank to be asked when connecting" };
    password.setPlaceholderString(Some(&NSString::from_str(hint)));
    col.add(&password, FIELD_H);
    let clear_password = has_password.then(|| {
        let b = checkbox(mtm, "Clear saved password", false);
        col.add(&b, FIELD_H - 4.0);
        b
    });

    col.label(mtm, "Display");
    let display_index = if initial.display == DisplayMode::Windowed { 1 } else { 0 };
    let display = popup(mtm, &["Full screen", "Windowed"], display_index, t, sel!(displayChanged:));
    col.add(&display, FIELD_H + 2.0);

    col.label(mtm, "Monitors (full screen) — tick two or more to span them");
    let saved: Vec<&str> = initial.monitors.iter().map(String::as_str).collect();
    let mut monitors = Vec::new();
    for d in displays {
        let ticked = if saved.is_empty() { d.is_main } else { saved.contains(&d.key.as_str()) };
        let label = format!("{} — {}×{}", d.name, d.frame.w as i64, d.frame.h as i64);
        let b = checkbox(mtm, &label, ticked);
        col.add(&b, FIELD_H - 4.0);
        monitors.push((d.key.clone(), b));
    }

    col.label(mtm, "Shared folder (appears as a drive on the remote PC)");
    let folder = popup(mtm, &["None"], 0, t, sel!(folderChanged:));
    col.add(&folder, FIELD_H + 2.0);

    let sound = checkbox(mtm, "Play remote sound on this Mac", initial.sound);
    col.add(&sound, FIELD_H - 4.0);

    let error = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    error.setTextColor(Some(&NSColor::systemRedColor()));
    col.add(&error, 2.0 * LABEL_H);

    // Buttons go below everything; size the window to fit.
    let height = col.y + 32.0 + PAD;
    panel.setContentSize(NSSize::new(DIALOG_W, height));
    col.finish(&content, height);
    let save_b = button(mtm, "Save", t, sel!(save:), "\r");
    let cancel_b = button(mtm, "Cancel", t, sel!(cancel:), "\u{1b}");
    save_b.setFrame(NSRect::new(NSPoint::new(DIALOG_W - PAD - 90.0, PAD), NSSize::new(90.0, 32.0)));
    cancel_b.setFrame(NSRect::new(NSPoint::new(DIALOG_W - PAD - 190.0, PAD), NSSize::new(90.0, 32.0)));
    content.addSubview(&save_b);
    content.addSubview(&cancel_b);

    *target.ivars().controls.borrow_mut() = Some(Controls {
        name: name.clone(),
        host,
        username,
        password,
        clear_password,
        display,
        monitors,
        folder,
        sound,
        error,
    });
    target.rebuild_folder_menu();
    target.update_monitor_enablement();

    panel.center();
    panel.setInitialFirstResponder(Some(&name));
    #[cfg(debug_assertions)]
    if let Some(view) = panel.contentView() {
        panel.display();
        crate::dev::snapshot_if_requested(&view, "edit-dialog");
    }
    let app = NSApplication::sharedApplication(mtm);
    let response = app.runModalForWindow(&panel);
    panel.orderOut(None);
    let result = target.ivars().result.borrow_mut().take();
    (response == RESPONSE_SAVE).then_some(result).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Connection, DisplayMode, HostError};

    fn form() -> FormValues {
        FormValues {
            name: "Media Server".into(),
            host: "192.168.0.201".into(),
            username: "kevin".into(),
            password: String::new(),
            clear_password: false,
            display: DisplayMode::Fullscreen,
            monitors_checked: vec!["main".into()],
            main_key: "main".into(),
            shared_folder: None,
            sound: true,
        }
    }

    #[test]
    fn keeps_id_and_applies_fields() {
        let initial = Connection::new("old", "10.0.0.1");
        let r = compose_result(&initial, form()).unwrap();
        assert_eq!(r.connection.id, initial.id);
        assert_eq!((r.connection.name.as_str(), r.connection.host.as_str()), ("Media Server", "192.168.0.201"));
        assert_eq!(r.connection.username, "kevin");
        assert_eq!(r.password, PasswordChange::Keep);
    }

    #[test]
    fn empty_name_uses_host() {
        let r = compose_result(&Connection::new("", ""), FormValues { name: "  ".into(), ..form() }).unwrap();
        assert_eq!(r.connection.name, "192.168.0.201");
    }

    #[test]
    fn invalid_host_is_rejected() {
        assert_eq!(compose_result(&Connection::new("", ""), FormValues { host: "".into(), ..form() }).unwrap_err(), HostError::Empty);
        assert_eq!(
            compose_result(&Connection::new("", ""), FormValues { host: "h:99999".into(), ..form() }).unwrap_err(),
            HostError::InvalidPort
        );
    }

    #[test]
    fn host_is_trimmed() {
        let r = compose_result(&Connection::new("", ""), FormValues { host: " server:3390 ".into(), ..form() }).unwrap();
        assert_eq!(r.connection.host, "server:3390");
    }

    #[test]
    fn only_main_display_ticked_stores_no_monitors() {
        let r = compose_result(&Connection::new("", ""), form()).unwrap();
        assert!(r.connection.monitors.is_empty());
        let none = compose_result(&Connection::new("", ""), FormValues { monitors_checked: vec![], ..form() }).unwrap();
        assert!(none.connection.monitors.is_empty());
    }

    #[test]
    fn several_displays_are_stored_in_order() {
        let f = FormValues { monitors_checked: vec!["dell-1".into(), "dell-2".into()], ..form() };
        let r = compose_result(&Connection::new("", ""), f).unwrap();
        assert_eq!(r.connection.monitors, vec!["dell-1".to_string(), "dell-2".to_string()]);
    }

    #[test]
    fn password_changes() {
        let c = Connection::new("", "");
        let set = compose_result(&c, FormValues { password: "s3cret".into(), ..form() }).unwrap();
        assert_eq!(set.password, PasswordChange::Set("s3cret".into()));
        let clear = compose_result(&c, FormValues { clear_password: true, ..form() }).unwrap();
        assert_eq!(clear.password, PasswordChange::Clear);
    }

    #[test]
    fn changing_host_forgets_trusted_certificate() {
        let mut c = Connection::new("A", "10.0.0.1");
        c.cert_fingerprint = Some("sha256:aa".into());
        let same = compose_result(&c, FormValues { host: "10.0.0.1".into(), ..form() }).unwrap();
        assert_eq!(same.connection.cert_fingerprint.as_deref(), Some("sha256:aa"));
        let moved = compose_result(&c, form()).unwrap();
        assert_eq!(moved.connection.cert_fingerprint, None);
    }

    #[test]
    fn display_folder_and_sound_are_applied() {
        let f = FormValues {
            display: DisplayMode::Windowed,
            shared_folder: Some("/Users/me/Downloads".into()),
            sound: false,
            ..form()
        };
        let r = compose_result(&Connection::new("", ""), f).unwrap();
        assert_eq!(r.connection.display, DisplayMode::Windowed);
        assert_eq!(r.connection.shared_folder.as_deref(), Some(std::path::Path::new("/Users/me/Downloads")));
        assert!(!r.connection.sound);
    }
}
