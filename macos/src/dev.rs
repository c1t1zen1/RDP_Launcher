//! Debug-build-only shortcut: `RDPL_DEV_HOST=host[:port] RDPL_DEV_USER=… RDPL_DEV_PASS=… cargo run`
//! opens one windowed session without the launcher. Removed once the launcher drives sessions.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::MainThreadMarker;

use crate::app::{on_main, runtime};
use crate::model::{parse_host, split_username};
use crate::monitors::{even_size, RemoteMonitor};
use crate::session::engine::{start_session, SessionEvent, SessionRequest};
use crate::session::window::{windowed_size, SessionWindows};

struct Dev {
    host: String,
    port: u16,
    user: String,
    password: String,
    windows: Option<Rc<SessionWindows>>,
}

thread_local! {
    static DEV: RefCell<Option<Dev>> = const { RefCell::new(None) };
}

pub fn start_from_env(mtm: MainThreadMarker) {
    let Ok(host_input) = std::env::var("RDPL_DEV_HOST") else { return };
    let (host, port) = match parse_host(&host_input) {
        Ok(hp) => hp,
        Err(e) => {
            tracing::error!(%e, "RDPL_DEV_HOST");
            return;
        }
    };
    DEV.with(|d| {
        *d.borrow_mut() = Some(Dev {
            host,
            port,
            user: std::env::var("RDPL_DEV_USER").unwrap_or_default(),
            password: std::env::var("RDPL_DEV_PASS").unwrap_or_default(),
            windows: None,
        })
    });
    connect(mtm, None);
}

fn connect(mtm: MainThreadMarker, trusted: Option<String>) {
    let size = windowed_size(mtm);
    DEV.with(|d| {
        let mut d = d.borrow_mut();
        let Some(dev) = d.as_mut() else { return };
        if let Some(old) = dev.windows.take() {
            old.close_all();
        }
        let (username, domain) = split_username(&dev.user);
        let req = SessionRequest {
            host: dev.host.clone(),
            port: dev.port,
            username,
            domain,
            password: dev.password.clone(),
            trusted_fingerprint: trusted,
            layout: vec![RemoteMonitor {
                left: 0,
                top: 0,
                width: even_size(size.0 as u32),
                height: size.1 as u32,
                primary: true,
            }],
            sound: true,
            shared_folder: None,
            clipboard: None,
        };
        let handle = start_session(
            runtime().handle(),
            req,
            Box::new(|event| on_main(move |mtm| handle_event(mtm, event))),
        );
        let title = format!("{}:{}", dev.host, dev.port);
        dev.windows = Some(SessionWindows::open_windowed(mtm, &title, handle, size));
    });
}

fn handle_event(mtm: MainThreadMarker, event: SessionEvent) {
    let windows = DEV.with(|d| d.borrow().as_ref().and_then(|dev| dev.windows.clone()));
    match event {
        SessionEvent::FrameReady => {
            if let Some(w) = windows {
                w.present_frame();
            }
        }
        SessionEvent::Pointer(p) => {
            if let Some(w) = windows {
                w.set_pointer(&p);
            }
        }
        SessionEvent::CertificateUntrusted { fingerprint, .. } => {
            tracing::warn!(%fingerprint, "dev mode: trusting server certificate and reconnecting");
            connect(mtm, Some(fingerprint));
        }
        SessionEvent::Connected => tracing::info!("dev session connected"),
        SessionEvent::Failed { kind, message } => tracing::error!(?kind, %message, "dev session failed"),
        SessionEvent::Ended => {
            tracing::info!("dev session ended");
            if let Some(w) = windows {
                w.close_all();
            }
        }
    }
}

/// Debug aid: `RDPL_SNAPSHOT=/path/prefix` writes `<prefix>-<name>.png` renders of views
/// (no Screen Recording permission needed — it draws the app's own views offscreen).
pub fn snapshot_if_requested(view: &objc2_app_kit::NSView, name: &str) {
    use objc2_app_kit::NSBitmapImageFileType;
    use objc2_foundation::{NSDictionary, NSString};
    let Ok(prefix) = std::env::var("RDPL_SNAPSHOT") else { return };
    // Windows paint their own background; give the snapshot one so light-on-dark text shows.
    if view.layer().is_none_or(|l| l.backgroundColor().is_none()) {
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            let bg = objc2_app_kit::NSColor::windowBackgroundColor();
            layer.setBackgroundColor(Some(&bg.CGColor()));
        }
    }
    let rect = view.bounds();
    let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(rect) else { return };
    view.cacheDisplayInRect_toBitmapImageRep(rect, &rep);
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
    if let Some(data) = data {
        let path = format!("{prefix}-{name}.png");
        data.writeToFile_atomically(&NSString::from_str(&path), true);
        tracing::info!(%path, "snapshot written");
    }
}

/// Debug aid: `RDPL_DEV_OPEN=add` or `edit` opens that dialog right after launch.
pub fn open_if_requested() {
    let Ok(what) = std::env::var("RDPL_DEV_OPEN") else { return };
    on_main(move |mtm| match what.as_str() {
        "add" => crate::controller::add(mtm),
        "edit" => crate::controller::edit(mtm, 0),
        "about" => crate::launcher::about::show(mtm),
        _ => {}
    });
}
