//! App state (connections, settings) and what each launcher action does.
//! Everything here runs on the main thread.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle};
use objc2_foundation::NSString;

use crate::displays::current_displays;
use crate::keychain::Keychain;
use crate::launcher::edit_dialog::{self, PasswordChange};
use crate::launcher::{ItemStatus, LauncherCallbacks, LauncherController};
use crate::model::{Connection, Settings, Store};

pub struct AppState {
    pub store: Store,
    pub conns: Vec<Connection>,
    pub settings: Settings,
    pub launcher: Option<Rc<LauncherController>>,
}

thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// Run `f` with the app state. Never call AppKit modal UI inside `f`.
pub fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> Option<R> {
    STATE.with(|s| s.borrow_mut().as_mut().map(f))
}

fn launcher() -> Option<Rc<LauncherController>> {
    with_state(|s| s.launcher.clone()).flatten()
}

fn save(state: &AppState) {
    if let Err(e) = state.store.save_connections(&state.conns) {
        tracing::error!(error = %e, "could not save connections");
    }
}

/// Push the current list to the launcher (statuses all idle until sessions exist).
pub fn refresh_list() {
    let items = with_state(|s| {
        s.conns
            .iter()
            .map(|c| (c.name.clone(), c.host.clone(), ItemStatus::Idle))
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    if let Some(l) = launcher() {
        l.set_items(items);
    }
}

pub fn start(mtm: MainThreadMarker) {
    let store = Store::new(Store::default_dir());
    let conns = store.load_connections();
    let settings = store.load_settings();
    let pinned = settings.always_on_top;
    STATE.with(|s| *s.borrow_mut() = Some(AppState { store, conns, settings, launcher: None }));

    let callbacks = LauncherCallbacks {
        connect: Box::new(|i| tracing::info!(index = i, "connect requested")),
        edit: Box::new(move |i| edit(mtm, i)),
        duplicate: Box::new(duplicate),
        delete: Box::new(move |i| delete(mtm, i)),
        add: Box::new(move || add(mtm)),
        reordered: Box::new(reordered),
        pin_changed: Box::new(pin_changed),
        about: Box::new(|| tracing::info!("about requested")),
    };
    let launcher = LauncherController::new(mtm, callbacks, pinned);
    with_state(|s| s.launcher = Some(launcher.clone()));
    refresh_list();
    launcher.show();
    #[cfg(debug_assertions)]
    {
        if let Some(view) = launcher.window().contentView() {
            crate::dev::snapshot_if_requested(&view, "launcher");
        }
        crate::dev::open_if_requested();
    }
}

pub(crate) fn add(mtm: MainThreadMarker) {
    let displays = current_displays(mtm);
    let initial = Connection::new("", "");
    let Some(result) = edit_dialog::show(mtm, "New Connection", &initial, false, &displays) else { return };
    apply_password(mtm, &result.connection.id, &result.password);
    with_state(|s| {
        s.conns.push(result.connection);
        save(s);
    });
    refresh_list();
}

pub(crate) fn edit(mtm: MainThreadMarker, i: usize) {
    let Some(Some(conn)) = with_state(|s| s.conns.get(i).cloned()) else { return };
    let has_password = Keychain::app().get(&conn.id).is_some();
    let displays = current_displays(mtm);
    let Some(result) = edit_dialog::show(mtm, "Edit Connection", &conn, has_password, &displays) else { return };
    apply_password(mtm, &result.connection.id, &result.password);
    with_state(|s| {
        // Look up by id: the list may have changed while the dialog was open.
        if let Some(slot) = s.conns.iter_mut().find(|c| c.id == result.connection.id) {
            *slot = result.connection;
            save(s);
        }
    });
    refresh_list();
}

fn apply_password(mtm: MainThreadMarker, id: &str, change: &PasswordChange) {
    let kc = Keychain::app();
    match change {
        PasswordChange::Keep => {}
        PasswordChange::Clear => kc.delete(id),
        PasswordChange::Set(p) => {
            if let Err(e) = kc.set(id, p) {
                tracing::error!(error = %e, "could not save password");
                show_error(mtm, "The password could not be saved in the Keychain.", &e.to_string());
            }
        }
    }
}

/// Modal error message.
pub fn show_error(mtm: MainThreadMarker, message: &str, detail: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    alert.setAlertStyle(NSAlertStyle::Critical);
    alert.runModal();
}

fn duplicate(i: usize) {
    with_state(|s| {
        let Some(original) = s.conns.get(i) else { return };
        let copy = original.duplicate();
        if let Err(e) = Keychain::app().copy(&original.id, &copy.id) {
            tracing::warn!(error = %e, "could not copy password for duplicate");
        }
        s.conns.insert(i + 1, copy);
        save(s);
    });
    refresh_list();
}

fn delete(mtm: MainThreadMarker, i: usize) {
    let Some(Some(name)) = with_state(|s| s.conns.get(i).map(|c| c.name.clone())) else { return };
    if !confirm(mtm, &format!("Delete \"{name}\"?"), "Delete") {
        return;
    }
    with_state(|s| {
        if i < s.conns.len() {
            let removed = s.conns.remove(i);
            Keychain::app().delete(&removed.id);
            save(s);
        }
    });
    refresh_list();
}

fn reordered(from: usize, to: usize) {
    // The list view has already moved the button; just persist the new order.
    with_state(|s| {
        if from < s.conns.len() && to < s.conns.len() {
            let c = s.conns.remove(from);
            s.conns.insert(to, c);
            save(s);
        }
    });
}

fn pin_changed(on: bool) {
    with_state(|s| {
        s.settings.always_on_top = on;
        if let Err(e) = s.store.save_settings(&s.settings) {
            tracing::error!(error = %e, "could not save settings");
        }
    });
}

/// Modal yes/no question; returns true for the `confirm` button.
pub fn confirm(mtm: MainThreadMarker, message: &str, confirm: &str) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(message));
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.addButtonWithTitle(&NSString::from_str(confirm));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}
