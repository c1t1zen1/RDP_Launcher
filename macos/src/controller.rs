//! App state (connections, settings) and what each launcher action does.
//! Everything here runs on the main thread.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle};
use objc2_foundation::NSString;

use crate::displays::current_displays;
use crate::keychain::Keychain;
use crate::launcher::edit_dialog::{self, PasswordChange};
use crate::launcher::status_item::{self, StatusTarget};
use crate::launcher::{about, ItemStatus, LauncherCallbacks, LauncherController};
use crate::model::{Connection, Settings, Store};
use crate::session::manager::{manager, SessionCallbacks, SessionManager};

pub struct AppState {
    pub store: Store,
    pub conns: Vec<Connection>,
    pub settings: Settings,
    pub launcher: Option<Rc<LauncherController>>,
    pub status_item: Option<Retained<StatusTarget>>,
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

pub fn show_launcher() {
    if let Some(l) = launcher() {
        l.show();
    }
}

fn status_of(id: &str) -> ItemStatus {
    manager().map_or(ItemStatus::Idle, |m| m.status_of(id))
}

fn index_of(id: &str) -> Option<usize> {
    with_state(|s| s.conns.iter().position(|c| c.id == id)).flatten()
}

/// Number of open RDP sessions.
pub fn session_count() -> usize {
    manager().map_or(0, |m| m.count())
}

/// Ask before quitting while sessions are open; disconnects them if the user agrees.
pub fn confirm_quit(mtm: MainThreadMarker) -> bool {
    let n = session_count();
    if n == 0 {
        return true;
    }
    let ok = confirm(mtm, &format!("Disconnect {n} session(s) and quit?"), "Quit");
    if ok {
        if let Some(m) = manager() {
            m.disconnect_all();
        }
    }
    ok
}

fn connect(i: usize) {
    let Some(Some(conn)) = with_state(|s| s.conns.get(i).cloned()) else { return };
    if let Some(m) = manager() {
        m.connect(&conn);
    }
}

fn set_status(id: &str, status: ItemStatus) {
    if let (Some(i), Some(l)) = (index_of(id), launcher()) {
        l.set_status(i, status);
        #[cfg(debug_assertions)]
        if status == ItemStatus::Connected {
            if let Some(view) = l.window().contentView() {
                view.displayIfNeeded();
                crate::dev::snapshot_if_requested(&view, "launcher-connected");
            }
        }
    }
}

fn trust(id: &str, fingerprint: String) {
    with_state(|s| {
        if let Some(c) = s.conns.iter_mut().find(|c| c.id == id) {
            c.cert_fingerprint = Some(fingerprint);
            save(s);
        }
    });
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
            .map(|c| (c.name.clone(), c.host.clone(), status_of(&c.id)))
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
    STATE.with(|s| *s.borrow_mut() = Some(AppState { store, conns, settings, launcher: None, status_item: None }));

    SessionManager::install(
        mtm,
        SessionCallbacks {
            on_status: Box::new(set_status),
            on_edit_request: Box::new(move |id| {
                if let Some(i) = index_of(id) {
                    edit(mtm, i);
                }
            }),
            on_trust: Box::new(trust),
        },
    );
    let callbacks = LauncherCallbacks {
        connect: Box::new(connect),
        edit: Box::new(move |i| edit(mtm, i)),
        duplicate: Box::new(duplicate),
        delete: Box::new(move |i| delete(mtm, i)),
        add: Box::new(move || add(mtm)),
        reordered: Box::new(reordered),
        pin_changed: Box::new(pin_changed),
        about: Box::new(move || about::show(mtm)),
    };
    let ctl = LauncherController::new(mtm, callbacks, pinned);
    let status = status_item::install(
        mtm,
        Box::new(|| {
            if let Some(l) = launcher() {
                l.toggle();
            }
        }),
        Box::new(move || about::show(mtm)),
        Box::new(move || objc2_app_kit::NSApplication::sharedApplication(mtm).terminate(None)),
    );
    with_state(|s| {
        s.launcher = Some(ctl.clone());
        s.status_item = Some(status);
    });
    refresh_list();
    ctl.show();
    #[cfg(debug_assertions)]
    {
        if let Some(view) = ctl.window().contentView() {
            crate::dev::snapshot_if_requested(&view, "launcher");
        }
        crate::dev::open_if_requested();
    }
}

pub(crate) fn add(mtm: MainThreadMarker) {
    let displays = current_displays(mtm);
    let initial = Connection::new("", "");
    let Some(result) = edit_dialog::show(mtm, "New Connection", &initial, false, &displays) else { return };
    if !apply_password(mtm, &result.connection.id, &result.password) {
        return;
    }
    with_state(|s| {
        s.conns.push(result.connection);
        save(s);
    });
    refresh_list();
}

pub(crate) fn edit(mtm: MainThreadMarker, i: usize) {
    let Some(Some(conn)) = with_state(|s| s.conns.get(i).cloned()) else { return };
    let has_password = match Keychain::app().get(&conn.id) {
        Ok(password) => password.is_some(),
        Err(e) => {
            show_error(mtm, "The saved password could not be read from the Keychain.", &e.to_string());
            return;
        }
    };
    let displays = current_displays(mtm);
    let Some(result) = edit_dialog::show(mtm, "Edit Connection", &conn, has_password, &displays) else { return };
    if !apply_password(mtm, &result.connection.id, &result.password) {
        return;
    }
    with_state(|s| {
        // Look up by id: the list may have changed while the dialog was open.
        if let Some(slot) = s.conns.iter_mut().find(|c| c.id == result.connection.id) {
            *slot = result.connection.clone();
            save(s);
        }
    });
    if let Some(m) = manager() {
        m.update_connection(&result.connection);
    }
    refresh_list();
}

fn apply_password(mtm: MainThreadMarker, id: &str, change: &PasswordChange) -> bool {
    let kc = Keychain::app();
    let result = match change {
        PasswordChange::Keep => Ok(()),
        PasswordChange::Clear => kc.delete(id),
        PasswordChange::Set(p) => kc.set(id, p),
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(error = %e, "could not update saved password");
            show_error(mtm, "The saved password could not be updated in the Keychain.", &e.to_string());
            false
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
    if let (Some(Some(id)), Some(m)) = (with_state(|s| s.conns.get(i).map(|c| c.id.clone())), manager()) {
        m.disconnect(&id);
    }
    with_state(|s| {
        if i < s.conns.len() {
            let removed = s.conns.remove(i);
            if let Err(e) = Keychain::app().delete(&removed.id) {
                tracing::warn!(error = %e, "could not delete saved password");
            }
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
