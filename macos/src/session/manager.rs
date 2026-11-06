//! One session per connection: start, focus-existing, events → windows,
//! certificate / error prompts, status for the launcher's green glow.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use objc2::MainThreadMarker;

use crate::app::{on_main, runtime};
use crate::controller::show_error;
use crate::keychain::Keychain;
use crate::launcher::{password_prompt, ItemStatus};
use crate::displays::current_displays;
use crate::model::{Connection, DisplayMode};
use crate::monitors::{even_size, remote_layout, resolve_selection, DisplayInfo, RemoteMonitor};
use crate::session::alerts::{self, FailureChoice};
use crate::session::clipboard::{new_clipboard, ClipboardLink};
use crate::session::engine::{start_session, FailureKind, SessionEvent, SessionHandle, SessionRequest};
use crate::session::window::{windowed_size, SessionWindows};

struct Session {
    token: u64,
    conn: Connection,
    password: String,
    handle: SessionHandle,
    windows: Rc<SessionWindows>,
    clipboard: Rc<ClipboardLink>,
    connected: bool,
}

impl Session {
    /// Stop everything for this session (windows are closed without re-entering the manager).
    fn shut_down(&self) {
        self.clipboard.detach();
        self.windows.set_on_closed(Box::new(|| {}));
        self.handle.disconnect();
        self.windows.close_all();
    }
}

type StatusCallback = Box<dyn Fn(&str, ItemStatus)>;
type TrustCallback = Box<dyn Fn(&str, String)>;

pub struct SessionCallbacks {
    pub on_status: StatusCallback,
    /// The user chose "Edit Connection" after a failure.
    pub on_edit_request: Box<dyn Fn(&str)>,
    /// The user trusted a certificate fingerprint for this connection; persist it.
    pub on_trust: TrustCallback,
}

pub struct SessionManager {
    mtm: MainThreadMarker,
    sessions: RefCell<HashMap<String, Session>>,
    callbacks: SessionCallbacks,
    next_token: Cell<u64>,
}

thread_local! {
    static MANAGER: RefCell<Option<Rc<SessionManager>>> = const { RefCell::new(None) };
}

/// The app's session manager (main thread only).
pub fn manager() -> Option<Rc<SessionManager>> {
    MANAGER.with(|m| m.borrow().clone())
}

impl SessionManager {
    pub fn install(mtm: MainThreadMarker, callbacks: SessionCallbacks) -> Rc<SessionManager> {
        let m = Rc::new(SessionManager {
            mtm,
            sessions: RefCell::new(HashMap::new()),
            callbacks,
            next_token: Cell::new(1),
        });
        MANAGER.with(|slot| *slot.borrow_mut() = Some(m.clone()));
        m
    }

    pub fn count(&self) -> usize {
        self.sessions.borrow().len()
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.sessions.borrow().contains_key(id)
    }

    pub fn status_of(&self, id: &str) -> ItemStatus {
        match self.sessions.borrow().get(id) {
            Some(s) if s.connected => ItemStatus::Connected,
            Some(_) => ItemStatus::Connecting,
            None => ItemStatus::Idle,
        }
    }

    /// Click on a connection: focus its open session, or start one.
    pub fn connect(&self, conn: &Connection) {
        if let Some(windows) = self.sessions.borrow().get(&conn.id).map(|s| s.windows.clone()) {
            windows.focus();
            return;
        }
        #[cfg(debug_assertions)]
        let dev_password = std::env::var("RDPL_DEV_PASSWORD").ok();
        #[cfg(not(debug_assertions))]
        let dev_password: Option<String> = None;
        let password = match Keychain::app().get(&conn.id).or(dev_password) {
            Some(p) => p,
            None if conn.username.trim().is_empty() => String::new(),
            None => match password_prompt::ask(self.mtm, None, &conn.name, &conn.username) {
                Some((p, remember)) => {
                    if remember {
                        if let Err(e) = Keychain::app().set(&conn.id, &p) {
                            tracing::warn!(error = %e, "could not remember password");
                        }
                    }
                    p
                }
                None => return,
            },
        };
        self.start(conn.clone(), password);
    }

    fn start(&self, conn: Connection, password: String) {
        let (host, port) = match conn.endpoint() {
            Ok(hp) => hp,
            Err(e) => {
                show_error(self.mtm, &format!("Can't connect to {}", conn.name), &e.to_string());
                return;
            }
        };
        let token = self.next_token.get();
        self.next_token.set(token + 1);

        let (displays, layout) = self.layout_for(&conn);
        let (username, domain) = conn.credentials();
        let (clipboard_factory, clipboard) = new_clipboard(self.mtm);
        let req = SessionRequest {
            host: host.clone(),
            port,
            username,
            domain,
            password: password.clone(),
            trusted_fingerprint: conn.cert_fingerprint.clone(),
            layout: layout.clone(),
            sound: conn.sound,
            shared_folder: conn.shared_folder.clone(),
            clipboard: Some(Box::new(clipboard_factory)),
        };
        let id = conn.id.clone();
        let event_id = id.clone();
        let handle = start_session(
            runtime().handle(),
            req,
            Box::new(move |event| {
                let id = event_id.clone();
                on_main(move |_| {
                    if let Some(m) = manager() {
                        m.on_event(&id, token, event);
                    }
                });
            }),
        );
        // Attach right away so the server's first clipboard "ready" is answered.
        clipboard.attach(handle.input_sender());
        let title = format!("{} — {}", conn.name, conn.host);
        let windows = SessionWindows::open(self.mtm, &title, handle.clone(), conn.display, &displays, &layout);
        let closed_id = id.clone();
        windows.set_on_closed(Box::new(move || {
            if let Some(m) = manager() {
                m.closed(&closed_id, token);
            }
        }));
        self.sessions.borrow_mut().insert(id.clone(), Session { token, conn, password, handle, windows, clipboard, connected: false });
        (self.callbacks.on_status)(&id, ItemStatus::Connecting);
        tracing::info!(connection = %id, "session window opened");
    }

    /// Full screen: the chosen (connected) displays; windowed: a 1280×800-ish window.
    fn layout_for(&self, conn: &Connection) -> (Vec<DisplayInfo>, Vec<RemoteMonitor>) {
        match conn.display {
            DisplayMode::Fullscreen => {
                #[cfg(debug_assertions)]
                let fake = crate::dev::fake_displays();
                #[cfg(not(debug_assertions))]
                let fake: Option<Vec<DisplayInfo>> = None;
                let (available, saved) = match fake {
                    Some(f) => {
                        let keys = f.iter().map(|d| d.key.clone()).collect::<Vec<_>>();
                        (f, keys)
                    }
                    None => (current_displays(self.mtm), conn.monitors.clone()),
                };
                let displays = resolve_selection(&saved, &available);
                let layout = remote_layout(&displays);
                tracing::info!(displays = displays.len(), ?layout, "full-screen layout");
                (displays, layout)
            }
            DisplayMode::Windowed => {
                let (w, h) = windowed_size(self.mtm);
                let layout =
                    vec![RemoteMonitor { left: 0, top: 0, width: even_size(w as u32), height: h as u32, primary: true }];
                (Vec::new(), layout)
            }
        }
    }

    fn current(&self, id: &str, token: u64) -> bool {
        self.sessions.borrow().get(id).is_some_and(|s| s.token == token)
    }

    fn on_event(&self, id: &str, token: u64, event: SessionEvent) {
        if !self.current(id, token) {
            return; // a stale event from a session that was already replaced or closed
        }
        let windows = self.sessions.borrow().get(id).map(|s| s.windows.clone());
        let Some(windows) = windows else { return };
        match event {
            SessionEvent::FrameReady => windows.present_frame(),
            SessionEvent::Pointer(p) => windows.set_pointer(&p),
            SessionEvent::Connected => {
                if let Some(s) = self.sessions.borrow_mut().get_mut(id) {
                    s.connected = true;
                }
                (self.callbacks.on_status)(id, ItemStatus::Connected);
                tracing::info!(connection = %id, "session connected");
                #[cfg(debug_assertions)]
                crate::dev::resize_if_requested(&windows);
            }
            SessionEvent::CertificateUntrusted { fingerprint, previous } => {
                let Some(name) = self.sessions.borrow().get(id).map(|s| s.conn.name.clone()) else { return };
                #[cfg(debug_assertions)]
                let auto = crate::dev::auto_trust();
                #[cfg(not(debug_assertions))]
                let auto = false;
                let trusted = auto || alerts::confirm_certificate(self.mtm, &name, &fingerprint, previous.as_deref());
                if !self.current(id, token) {
                    return; // closed while the prompt was up
                }
                if trusted {
                    (self.callbacks.on_trust)(id, fingerprint.clone());
                    self.restart(id, |conn, _| conn.cert_fingerprint = Some(fingerprint));
                } else {
                    self.close(id);
                }
            }
            SessionEvent::Failed { kind, message } => {
                let Some(name) = self.sessions.borrow().get(id).map(|s| s.conn.name.clone()) else { return };
                let choice = alerts::connection_failed(self.mtm, &name, kind, &message);
                if !self.current(id, token) {
                    return;
                }
                match choice {
                    FailureChoice::Reconnect => self.restart(id, |_, _| {}),
                    FailureChoice::Edit => {
                        self.close(id);
                        (self.callbacks.on_edit_request)(id);
                    }
                    FailureChoice::Close => self.close(id),
                }
                if kind == FailureKind::BadCredentials {
                    tracing::info!("sign-in failed; the saved password may be wrong");
                }
            }
            SessionEvent::Ended => self.close(id),
        }
    }

    /// Close this session's windows and start it again (after `adjust`).
    fn restart(&self, id: &str, adjust: impl FnOnce(&mut Connection, &mut String)) {
        let Some(mut s) = self.sessions.borrow_mut().remove(id) else { return };
        s.shut_down();
        adjust(&mut s.conn, &mut s.password);
        // Re-read the Keychain in case the password was changed meanwhile.
        let password = Keychain::app().get(id).unwrap_or(s.password);
        self.start(s.conn, password);
    }

    fn close(&self, id: &str) {
        let s = self.sessions.borrow_mut().remove(id);
        if let Some(s) = s {
            s.shut_down();
        }
        (self.callbacks.on_status)(id, ItemStatus::Idle);
    }

    /// A session window was closed by the user.
    fn closed(&self, id: &str, token: u64) {
        if self.current(id, token) {
            self.close(id);
        }
    }

    pub fn disconnect(&self, id: &str) {
        if self.is_open(id) {
            self.close(id);
        }
    }

    pub fn disconnect_all(&self) {
        let ids: Vec<String> = self.sessions.borrow().keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
    }

    /// Update a running session's copy of the connection (e.g. after it was edited).
    pub fn update_connection(&self, conn: &Connection) {
        if let Some(s) = self.sessions.borrow_mut().get_mut(&conn.id) {
            s.conn = conn.clone();
        }
    }
}
