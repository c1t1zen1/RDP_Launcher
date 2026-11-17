//! Live check of Standard RDP Security against a real server (e.g. xrdp with
//! `security_layer=rdp`). Ignored by default; run with
//!
//!     RDPL_LEGACY_HOST=203.0.113.7 cargo test --test legacy_live -- --ignored --nocapture
//!
//! It connects without credentials, so the server shows its own login screen: no
//! account is used. Optional `RDPL_LEGACY_PORT` (default 3389).

use std::time::Duration;

use rdp_launcher::monitors::RemoteMonitor;
use rdp_launcher::session::engine::{start_session, SessionEvent, SessionHandle, SessionRequest};
use tokio::sync::mpsc::{self, UnboundedReceiver};

fn request(trusted: Option<String>) -> SessionRequest {
    SessionRequest {
        host: std::env::var("RDPL_LEGACY_HOST").expect("set RDPL_LEGACY_HOST"),
        port: std::env::var("RDPL_LEGACY_PORT").map(|p| p.parse().expect("port")).unwrap_or(3389),
        username: String::new(),
        domain: None,
        password: String::new(),
        trusted_fingerprint: trusted,
        layout: vec![RemoteMonitor { left: 0, top: 0, width: 1024, height: 768, primary: true }],
        sound: false,
        shared_folder: None,
        clipboard: None,
    }
}

fn start(req: SessionRequest) -> (SessionHandle, UnboundedReceiver<SessionEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = start_session(
        &tokio::runtime::Handle::current(),
        req,
        Box::new(move |event| {
            let _ = tx.send(event);
        }),
    );
    (handle, rx)
}

async fn next_event(rx: &mut UnboundedReceiver<SessionEvent>) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .expect("session event within 30 s")
        .expect("event channel open")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real Standard RDP Security server (RDPL_LEGACY_HOST)"]
async fn connects_to_a_standard_rdp_security_server() {
    let _ = tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).try_init();

    // First contact: TLS is refused, the proxy takes over and asks to trust the server.
    let (_handle, mut rx) = start(request(None));
    let fingerprint = match next_event(&mut rx).await {
        SessionEvent::CertificateUntrusted { fingerprint, previous: None } => fingerprint,
        other => panic!("expected CertificateUntrusted, got {other:?}"),
    };
    println!("server key fingerprint: {fingerprint}");

    // Trusted: the session connects and the login screen arrives, decrypted.
    let (handle, mut rx) = start(request(Some(fingerprint)));
    let mut connected = false;
    loop {
        match next_event(&mut rx).await {
            SessionEvent::Connected => connected = true,
            SessionEvent::FrameReady => break,
            SessionEvent::Pointer(_) => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(connected, "a frame arrived before Connected");
    let (width, height) = {
        let fb = handle.framebuffer();
        let fb = fb.lock().unwrap();
        (fb.width(), fb.height())
    };
    println!("first frame: {width}x{height}");
    assert!(width > 0 && height > 0);

    // Input is encrypted too: move the mouse, then disconnect cleanly.
    handle.send_operations(vec![ironrdp_input::Operation::MouseMove(ironrdp_input::MousePosition { x: 10, y: 10 })]);
    tokio::time::sleep(Duration::from_secs(1)).await;
    handle.disconnect();
    match next_event(&mut rx).await {
        SessionEvent::Ended | SessionEvent::FrameReady | SessionEvent::Pointer(_) => {}
        other => panic!("unexpected event after disconnect: {other:?}"),
    }
}
