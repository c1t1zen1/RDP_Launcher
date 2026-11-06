//! End-to-end tests: our session engine against an in-process IronRDP server
//! (TLS + CredSSP/NLA, self-signed certificate from `tests/certs`).

use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ironrdp_input::{Operation, Scancode};
use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayContext, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat,
    RdpServer, RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerEvent, ServerResult,
    TlsIdentityCtx,
};
use rdp_launcher::certs;
use rdp_launcher::monitors::RemoteMonitor;
use rdp_launcher::session::engine::{start_session, FailureKind, SessionEvent, SessionHandle, SessionRequest};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::{oneshot, Mutex};
use tokio::task::LocalSet;

use ironrdp_core::impl_as_any;
use ironrdp_pdu::PduResult;
use ironrdp_rdpdr::pdu::efs::{
    ClientDriveLockControlResponse, ClientDriveNotifyChangeDirectoryResponse, ClientDriveQueryDirectoryResponse,
    ClientDriveQueryInformationResponse, ClientDriveQuerySecurityResponse, ClientDriveQueryVolumeInformationResponse,
    ClientDriveSetInformationResponse, ClientDriveSetSecurityResponse, DeviceAnnounceHeader, DeviceCloseResponse,
    DeviceControlResponse, DeviceCreateResponse, DeviceFlushBuffersResponse, DeviceReadResponse, DeviceWriteResponse,
};
use ironrdp_rdpdr::server::RdpdrServerBackend;
use ironrdp_server::{RdpdrServerFactory, ServerEventSender};

const USER: &str = "user";
const PASSWORD: &str = "password";
const SIZE: u16 = 64;
const RED: u32 = 0x00FF_0000;

// ---------- test server ----------

struct Display {
    rx: Arc<Mutex<UnboundedReceiver<DisplayUpdate>>>,
}

struct Updates {
    rx: Arc<Mutex<UnboundedReceiver<DisplayUpdate>>>,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        Ok(self.rx.lock().await.recv().await)
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        DesktopSize { width: SIZE, height: SIZE }
    }

    async fn updates(&mut self, _: DisplayContext) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        Ok(Box::new(Updates { rx: Arc::clone(&self.rx) }))
    }
}

struct Input(UnboundedSender<KeyboardEvent>);

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        let _ = self.0.send(event);
    }

    fn mouse(&mut self, _: MouseEvent) {}
}

/// Server-side RDPDR that records announced devices (as Debug text).
#[derive(Debug)]
struct DriveRecorder(UnboundedSender<String>);

impl_as_any!(DriveRecorder);

impl RdpdrServerBackend for DriveRecorder {
    fn on_device_announce(&mut self, devices: &[DeviceAnnounceHeader]) -> Vec<(u32, bool)> {
        for d in devices {
            let _ = self.0.send(format!("id={} {d:?}", d.device_id()));
        }
        devices.iter().map(|d| (d.device_id(), true)).collect()
    }
    fn on_device_remove(&mut self, _: &[u32]) {}
    fn on_client_name(&mut self, computer_name: &str) {
        let _ = self.0.send(format!("client-name={computer_name}"));
    }
    fn on_create_complete(&mut self, _: &DeviceCreateResponse) -> PduResult<()> { Ok(()) }
    fn on_close_complete(&mut self, _: &DeviceCloseResponse) -> PduResult<()> { Ok(()) }
    fn on_read_complete(&mut self, _: &DeviceReadResponse) -> PduResult<()> { Ok(()) }
    fn on_write_complete(&mut self, _: &DeviceWriteResponse) -> PduResult<()> { Ok(()) }
    fn on_flush_buffers_complete(&mut self, _: &DeviceFlushBuffersResponse) -> PduResult<()> { Ok(()) }
    fn on_device_control_complete(&mut self, _: &DeviceControlResponse) -> PduResult<()> { Ok(()) }
    fn on_query_information_complete(&mut self, _: &ClientDriveQueryInformationResponse) -> PduResult<()> { Ok(()) }
    fn on_set_information_complete(&mut self, _: &ClientDriveSetInformationResponse) -> PduResult<()> { Ok(()) }
    fn on_query_directory_complete(&mut self, _: &ClientDriveQueryDirectoryResponse) -> PduResult<()> { Ok(()) }
    fn on_notify_change_directory_complete(&mut self, _: &ClientDriveNotifyChangeDirectoryResponse) -> PduResult<()> { Ok(()) }
    fn on_query_volume_information_complete(&mut self, _: &ClientDriveQueryVolumeInformationResponse) -> PduResult<()> { Ok(()) }
    fn on_lock_control_complete(&mut self, _: &ClientDriveLockControlResponse) -> PduResult<()> { Ok(()) }
    fn on_query_security_complete(&mut self, _: &ClientDriveQuerySecurityResponse) -> PduResult<()> { Ok(()) }
    fn on_set_security_complete(&mut self, _: &ClientDriveSetSecurityResponse) -> PduResult<()> { Ok(()) }
}

struct DriveRecorderFactory(UnboundedSender<String>);

impl ServerEventSender for DriveRecorderFactory {
    fn set_sender(&mut self, _: UnboundedSender<ServerEvent>) {}
}

impl RdpdrServerFactory for DriveRecorderFactory {
    fn build_backend(&self) -> Box<dyn RdpdrServerBackend> {
        Box::new(DriveRecorder(self.0.clone()))
    }
}

struct TestServer {
    addr: SocketAddr,
    cert_der: Vec<u8>,
    keys: UnboundedReceiver<KeyboardEvent>,
    drives: UnboundedReceiver<String>,
    display_tx: Option<UnboundedSender<DisplayUpdate>>,
}

impl TestServer {
    /// Ends the display stream; the server then closes the session.
    fn stop(&mut self) {
        self.display_tx = None;
    }

    /// Paint the whole desktop one color (send after the client is connected).
    fn paint(&self, color: u32) {
        self.display_tx.as_ref().expect("server running").send(solid_bitmap(color)).unwrap();
    }
}

fn solid_bitmap(color: u32) -> DisplayUpdate {
    let [_, r, g, b] = color.to_be_bytes();
    let data: Vec<u8> = (0..usize::from(SIZE) * usize::from(SIZE)).flat_map(|_| [b, g, r, 0xFF]).collect();
    DisplayUpdate::Bitmap(BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(SIZE).unwrap(),
        height: NonZeroU16::new(SIZE).unwrap(),
        format: PixelFormat::BgrA32,
        data: data.into(),
        stride: NonZeroUsize::new(usize::from(SIZE) * 4).unwrap(),
    })
}

/// Must be called inside a `LocalSet` (the server future is `!Send`).
async fn spawn_server() -> TestServer {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs");
    let identity = TlsIdentityCtx::init_from_paths(&dir.join("server-cert.pem"), &dir.join("server-key.pem"))
        .expect("test TLS identity");
    let cert_der = identity.certs[0].as_ref().to_vec();
    let acceptor = identity.make_acceptor().expect("TLS acceptor");
    let (display_tx, display_rx) = mpsc::unbounded_channel();
    let (keys_tx, keys) = mpsc::unbounded_channel();
    let (drives_tx, drives) = mpsc::unbounded_channel();
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], 0))
        .with_hybrid(acceptor, identity.pub_key)
        .with_input_handler(Input(keys_tx))
        .with_display_handler(Display { rx: Arc::new(Mutex::new(display_rx)) })
        .with_rdpdr_factory(Some(Box::new(DriveRecorderFactory(drives_tx))))
        .build();
    server.set_credentials(Some(Credentials { username: USER.into(), password: PASSWORD.into(), domain: None }));
    let events = server.event_sender().clone();
    tokio::task::spawn_local(async move {
        let _ = server.run().await;
    });
    let (tx, rx) = oneshot::channel();
    events.send(ServerEvent::GetLocalAddr(tx)).unwrap();
    let addr = rx.await.unwrap().expect("server is listening");
    TestServer { addr, cert_der, keys, drives, display_tx: Some(display_tx) }
}

// ---------- client helpers ----------

fn request(port: u16, password: &str, trusted: Option<String>) -> SessionRequest {
    SessionRequest {
        host: "127.0.0.1".into(),
        port,
        username: USER.into(),
        domain: None,
        password: password.into(),
        trusted_fingerprint: trusted,
        layout: vec![RemoteMonitor { left: 0, top: 0, width: u32::from(SIZE), height: u32::from(SIZE), primary: true }],
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
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("session event within 10 s")
        .expect("event channel open")
}

async fn wait_for(rx: &mut UnboundedReceiver<SessionEvent>, wanted: fn(&SessionEvent) -> bool) -> SessionEvent {
    loop {
        let event = next_event(rx).await;
        if wanted(&event) {
            return event;
        }
        if let SessionEvent::Failed { kind, message } = &event {
            panic!("session failed: {kind:?}: {message}");
        }
    }
}

async fn next_key(keys: &mut UnboundedReceiver<KeyboardEvent>) -> KeyboardEvent {
    tokio::time::timeout(Duration::from_secs(10), keys.recv()).await.expect("key within 10 s").expect("server alive")
}

/// Runs `body` inside a `LocalSet` on the multi-thread test runtime.
async fn local<F: std::future::Future<Output = ()>>(body: F) {
    LocalSet::new().run_until(body).await;
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread")]
async fn first_connection_reports_untrusted_certificate() {
    local(async {
        let server = spawn_server().await;
        let (_handle, mut rx) = start(request(server.addr.port(), PASSWORD, None));
        match next_event(&mut rx).await {
            SessionEvent::CertificateUntrusted { fingerprint, previous } => {
                assert_eq!(fingerprint, certs::fingerprint(&server.cert_der));
                assert_eq!(previous, None);
            }
            other => panic!("expected CertificateUntrusted, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_certificate_reports_previous() {
    local(async {
        let server = spawn_server().await;
        let old = format!("sha256:{}", "0".repeat(64));
        let (_handle, mut rx) = start(request(server.addr.port(), PASSWORD, Some(old.clone())));
        match next_event(&mut rx).await {
            SessionEvent::CertificateUntrusted { fingerprint, previous } => {
                assert_eq!(fingerprint, certs::fingerprint(&server.cert_der));
                assert_eq!(previous, Some(old));
            }
            other => panic!("expected CertificateUntrusted, got {other:?}"),
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_certificate_connects_and_receives_frame() {
    local(async {
        let server = spawn_server().await;
        let fp = certs::fingerprint(&server.cert_der);
        let (handle, mut rx) = start(request(server.addr.port(), PASSWORD, Some(fp)));
        wait_for(&mut rx, |e| matches!(e, SessionEvent::Connected)).await;
        server.paint(RED);
        // Consume frames like the UI does (take_dirty per redraw) until the red bitmap lands;
        // the server may send a blank initial update first.
        let fb = handle.framebuffer();
        loop {
            wait_for(&mut rx, |e| matches!(e, SessionEvent::FrameReady)).await;
            let mut fb = fb.lock().unwrap();
            fb.take_dirty();
            assert_eq!((fb.width(), fb.height()), (SIZE, SIZE));
            if fb.pixels()[0] == RED {
                break;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_input_reaches_server() {
    local(async {
        let mut server = spawn_server().await;
        let fp = certs::fingerprint(&server.cert_der);
        let (handle, mut rx) = start(request(server.addr.port(), PASSWORD, Some(fp)));
        wait_for(&mut rx, |e| matches!(e, SessionEvent::Connected)).await;
        handle.send_operations(vec![Operation::KeyPressed(Scancode::from_u8(false, 0x1E))]);
        loop {
            if let KeyboardEvent::Pressed { code: 0x1E, extended: false } = next_key(&mut server.keys).await {
                break;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn release_all_releases_held_key() {
    local(async {
        let mut server = spawn_server().await;
        let fp = certs::fingerprint(&server.cert_der);
        let (handle, mut rx) = start(request(server.addr.port(), PASSWORD, Some(fp)));
        wait_for(&mut rx, |e| matches!(e, SessionEvent::Connected)).await;
        handle.send_operations(vec![Operation::KeyPressed(Scancode::from_u8(false, 0x1E))]);
        handle.release_all();
        loop {
            if let KeyboardEvent::Released { code: 0x1E, extended: false } = next_key(&mut server.keys).await {
                break;
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_password_fails_as_bad_credentials() {
    local(async {
        let server = spawn_server().await;
        let fp = certs::fingerprint(&server.cert_der);
        let (_handle, mut rx) = start(request(server.addr.port(), "wrong", Some(fp)));
        loop {
            match next_event(&mut rx).await {
                SessionEvent::Failed { kind, .. } => {
                    assert_eq!(kind, FailureKind::BadCredentials);
                    break;
                }
                SessionEvent::Connected => panic!("connected with a wrong password"),
                _ => {}
            }
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_port_fails_as_unreachable() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (_handle, mut rx) = start(request(port, PASSWORD, None));
    match next_event(&mut rx).await {
        SessionEvent::Failed { kind, .. } => assert_eq!(kind, FailureKind::Unreachable),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn server_disconnect_reports_end() {
    local(async {
        let mut server = spawn_server().await;
        let fp = certs::fingerprint(&server.cert_der);
        let (_handle, mut rx) = start(request(server.addr.port(), PASSWORD, Some(fp)));
        wait_for(&mut rx, |e| matches!(e, SessionEvent::Connected)).await;
        server.stop();
        loop {
            match next_event(&mut rx).await {
                SessionEvent::Ended | SessionEvent::Failed { .. } => break,
                _ => {}
            }
        }
        // exactly one terminal event: nothing else arrives afterwards
        assert!(tokio::time::timeout(Duration::from_millis(500), rx.recv()).await.map_or(true, |e| e.is_none()));
    })
    .await;
}

/// Drives are announced only after Windows' "user logged on" notification, which the
/// test server never sends, so this checks the RDPDR channel is ours and named "Mac"
/// (Windows labels the drive "<folder> on Mac").
#[tokio::test(flavor = "multi_thread")]
async fn shared_folder_channel_identifies_as_mac() {
    local(async {
        let mut server = spawn_server().await;
        let folder = tempfile::Builder::new().prefix("SharedTest").tempdir().unwrap();
        let fp = certs::fingerprint(&server.cert_der);
        let mut req = request(server.addr.port(), PASSWORD, Some(fp));
        req.shared_folder = Some(folder.path().to_path_buf());
        let (_handle, mut rx) = start(req);
        wait_for(&mut rx, |e| matches!(e, SessionEvent::Connected)).await;
        let name = tokio::time::timeout(Duration::from_secs(10), server.drives.recv())
            .await
            .expect("RDPDR client name within 10 s")
            .expect("server alive");
        assert_eq!(name, "client-name=Mac");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_shared_folder_means_no_drive_channel() {
    local(async {
        let mut server = spawn_server().await;
        let fp = certs::fingerprint(&server.cert_der);
        let (_handle, mut rx) = start(request(server.addr.port(), PASSWORD, Some(fp)));
        wait_for(&mut rx, |e| matches!(e, SessionEvent::Connected)).await;
        assert!(tokio::time::timeout(Duration::from_secs(2), server.drives.recv()).await.is_err());
    })
    .await;
}
