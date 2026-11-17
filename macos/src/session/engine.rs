//! One RDP session: wraps `ironrdp_client::rdp::RdpClient` and bridges it to the UI.
//!
//! The client runs on its own thread (`Handle::block_on`), sharing the app's tokio
//! runtime. Screen updates are written into a shared [`Framebuffer`]; the UI is told
//! once per batch (`FrameReady`) and reads the image when it redraws.
//!
//! A server that selects Standard RDP Security instead of TLS is reconnected through
//! the [`legacy`] proxy, transparently to the UI.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironrdp_client::config::{ClipboardType, ConfigBuilder, Destination, TransportKind};
use ironrdp_client::output_channel::{output_channel, OutputEventReceiver};
use ironrdp_client::rdp::{RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent};
use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_connector::{ConnectorError, ConnectorErrorKind};
use ironrdp_displaycontrol::pdu::{DisplayControlMonitorLayout, MonitorLayoutEntry};
use ironrdp_graphics::pointer::DecodedPointer;
use ironrdp_input::{Database, Operation};
use ironrdp_pdu::gcc::{ClientMonitorData, Monitor, MonitorFlags};
use ironrdp_pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp_tls::CertificateValidation;

use crate::certs::{self, Trust};
use crate::monitors::{virtual_desktop, RemoteMonitor};
use crate::session::framebuffer::{Framebuffer, Region};
use crate::session::legacy;

const CLIENT_NAME: &str = "Mac";
const CLIENT_BUILD: u32 = 26100;
const KEYBOARD_LAYOUT_US: u32 = 0x409;
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_QUEUE: usize = 64;

pub struct SessionRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub password: String,
    pub trusted_fingerprint: Option<String>,
    /// One entry = single monitor; desktop size = bounding box of all entries.
    pub layout: Vec<RemoteMonitor>,
    pub sound: bool,
    pub shared_folder: Option<PathBuf>,
    pub clipboard: Option<Box<dyn CliprdrBackendFactory + Send>>,
}

impl fmt::Debug for SessionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRequest")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("password", &"<redacted>")
            .field("trusted_fingerprint", &self.trusted_fingerprint)
            .field("layout", &self.layout)
            .field("sound", &self.sound)
            .field("shared_folder", &self.shared_folder)
            .field("clipboard", &self.clipboard.is_some())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub enum PointerUpdate {
    Default,
    Hidden,
    Bitmap(Arc<DecodedPointer>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    BadCredentials,
    Unreachable,
    UnsupportedSecurity,
    Other,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Connected,
    /// The framebuffer changed; read it via [`SessionHandle::framebuffer`].
    FrameReady,
    Pointer(PointerUpdate),
    CertificateUntrusted { fingerprint: String, previous: Option<String> },
    Failed { kind: FailureKind, message: String },
    /// Graceful end: logoff, server disconnect, or the user disconnected.
    Ended,
}

struct Inner {
    /// Replaced when the session reconnects with Standard RDP Security.
    input: Mutex<RdpInputSender>,
    framebuffer: Arc<Mutex<Framebuffer>>,
    keys: Mutex<Database>,
    closed: AtomicBool,
}

impl Inner {
    fn new(input: RdpInputSender, framebuffer: Arc<Mutex<Framebuffer>>) -> Self {
        Self { input: Mutex::new(input), framebuffer, keys: Mutex::new(Database::new()), closed: AtomicBool::new(false) }
    }

    fn input(&self) -> std::sync::MutexGuard<'_, RdpInputSender> {
        self.input.lock().expect("input sender")
    }
}

/// Cheap to clone; all clones control the same session.
#[derive(Clone)]
pub struct SessionHandle {
    inner: Arc<Inner>,
}

impl SessionHandle {
    pub fn send_operations(&self, ops: Vec<Operation>) {
        if ops.is_empty() {
            return;
        }
        let events = self.inner.keys.lock().expect("input state").apply(ops);
        self.send_fast_path(events);
    }

    /// Release every key and mouse button the remote side thinks is held.
    pub fn release_all(&self) {
        let events = self.inner.keys.lock().expect("input state").release_all();
        self.send_fast_path(events);
    }

    fn send_fast_path(&self, events: smallvec::SmallVec<[ironrdp_pdu::input::fast_path::FastPathInputEvent; 2]>) {
        if events.is_empty() {
            return;
        }
        if let Err(e) = self.inner.input().try_send(RdpInputEvent::FastPath(events)) {
            tracing::debug!(error = %e, "dropped input event");
        }
    }

    pub fn resize(&self, width: u16, height: u16) {
        let event = RdpInputEvent::Resize { width, height, scale_factor: 100, physical_size: None };
        if let Err(e) = self.inner.input().try_send(event) {
            tracing::debug!(error = %e, "dropped resize request");
        }
    }

    pub fn set_monitor_layout(&self, layout: &[RemoteMonitor]) {
        match display_control_layout(layout) {
            Ok(l) => {
                if let Err(e) = self.inner.input().try_send(RdpInputEvent::MonitorLayout(l)) {
                    tracing::debug!(error = %e, "dropped monitor layout request");
                }
            }
            Err(e) => tracing::warn!(error = %e, "invalid monitor layout"),
        }
    }

    pub fn send_clipboard(&self, message: ClipboardMessage) {
        if let Err(e) = self.inner.input().send_clipboard(message) {
            tracing::debug!(error = %e, "clipboard message dropped");
        }
    }

    pub fn disconnect(&self) {
        let input = self.inner.input();
        self.inner.closed.store(true, Ordering::SeqCst);
        input.request_graceful_close();
    }

    /// Route input to a new client; false if the session was closed meanwhile.
    fn install(&self, input: RdpInputSender) -> bool {
        let mut current = self.inner.input();
        if self.inner.closed.load(Ordering::SeqCst) {
            return false;
        }
        *current = input;
        true
    }

    pub fn framebuffer(&self) -> Arc<Mutex<Framebuffer>> {
        Arc::clone(&self.inner.framebuffer)
    }
}

/// Start connecting. Events arrive on tokio threads; the caller hops to the main thread.
pub fn start_session(
    rt: &tokio::runtime::Handle,
    mut req: SessionRequest,
    on_event: Box<dyn Fn(SessionEvent) + Send + Sync>,
) -> SessionHandle {
    tracing::info!(request = ?req, "starting session");
    let presented: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let framebuffer = Arc::new(Mutex::new(Framebuffer::new()));
    let clipboard = req.clipboard.take().map(|factory| SharedClipboard(Arc::new(Mutex::new(factory))));

    let legacy = uses_legacy_security(&req.host, req.port);
    let attempt = match Attempt::prepare(rt, &req, legacy, &presented, clipboard.as_ref()) {
        Ok(attempt) => attempt,
        Err(e) => {
            // Still return a usable (inert) handle; report the failure.
            let (tx, _rx) = output_channel(1);
            let client = RdpClient::new(fallback_config(), tx);
            let handle = SessionHandle { inner: Arc::new(Inner::new(client.input_sender(), framebuffer)) };
            on_event(SessionEvent::Failed { kind: FailureKind::Other, message: format!("{e:#}") });
            return handle;
        }
    };
    let handle = SessionHandle { inner: Arc::new(Inner::new(attempt.client.input_sender(), Arc::clone(&framebuffer))) };

    let on_event: Arc<dyn Fn(SessionEvent) + Send + Sync> = Arc::from(on_event);
    let session = handle.clone();
    let rt = rt.clone();
    let spawned = std::thread::Builder::new().name("rdp-session".into()).spawn(move || {
        let mut attempt = attempt;
        loop {
            let Attempt { client, output, proxy } = attempt;
            let legacy = proxy.is_some();
            let pump = rt.spawn(pump(
                output,
                PumpContext {
                    framebuffer: Arc::clone(&framebuffer),
                    presented: Arc::clone(&presented),
                    trusted: req.trusted_fingerprint.clone(),
                    proxy_failure: proxy.as_ref().map(legacy::Proxy::failure),
                    legacy,
                },
                Arc::clone(&on_event),
            ));
            rt.block_on(client.run());
            // Let the pump deliver the final event before going on.
            let outcome = rt.block_on(pump).unwrap_or(Outcome::Ended { failed: false });
            drop(proxy);

            match outcome {
                Outcome::RetryLegacy => {
                    tracing::info!(host = %req.host, "server selected Standard RDP Security; reconnecting through the legacy proxy");
                    remember_legacy_security(&req.host, req.port, true);
                    match Attempt::prepare(&rt, &req, true, &presented, clipboard.as_ref()) {
                        Ok(next) if session.install(next.client.input_sender()) => attempt = next,
                        Ok(_) => break, // closed meanwhile
                        Err(e) => {
                            on_event(SessionEvent::Failed { kind: FailureKind::Other, message: format!("{e:#}") });
                            break;
                        }
                    }
                }
                Outcome::Ended { failed } => {
                    if failed && legacy {
                        // Maybe the server changed; try TLS first next time.
                        remember_legacy_security(&req.host, req.port, false);
                    }
                    break;
                }
            }
        }
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "could not spawn session thread");
    }
    handle
}

/// One connection attempt: a client and, for Standard RDP Security, its proxy.
struct Attempt {
    client: RdpClient,
    output: OutputEventReceiver,
    proxy: Option<legacy::Proxy>,
}

impl Attempt {
    fn prepare(
        rt: &tokio::runtime::Handle,
        req: &SessionRequest,
        legacy: bool,
        presented: &Arc<Mutex<Option<String>>>,
        clipboard: Option<&SharedClipboard>,
    ) -> anyhow::Result<Self> {
        let proxy = if legacy {
            Some(legacy::Proxy::start(rt, &req.host, req.port, req.trusted_fingerprint.clone(), Arc::clone(presented))?)
        } else {
            None
        };
        let config = build_config(req, presented, proxy.as_ref(), clipboard.is_some())?;
        let (output_tx, output) = output_channel(OUTPUT_QUEUE);
        let mut client = RdpClient::new(config, output_tx).with_desktop_updates();
        if let Some(clipboard) = clipboard {
            client = client.with_cliprdr_backend_factory(Box::new(clipboard.clone()));
        }
        Ok(Self { client, output, proxy })
    }
}

/// Lets a retried attempt reuse the session's clipboard backend factory.
#[derive(Clone)]
struct SharedClipboard(Arc<Mutex<Box<dyn CliprdrBackendFactory + Send>>>);

impl CliprdrBackendFactory for SharedClipboard {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        self.0.lock().expect("clipboard factory").build_cliprdr_backend()
    }
}

/// Servers that selected Standard RDP Security in this run (host, port), so that later
/// sessions go straight to the legacy proxy.
static LEGACY_SERVERS: Mutex<Vec<(String, u16)>> = Mutex::new(Vec::new());

fn uses_legacy_security(host: &str, port: u16) -> bool {
    LEGACY_SERVERS.lock().expect("legacy servers").iter().any(|(h, p)| h == host && *p == port)
}

fn remember_legacy_security(host: &str, port: u16, legacy: bool) {
    let mut servers = LEGACY_SERVERS.lock().expect("legacy servers");
    servers.retain(|(h, p)| !(h == host && *p == port));
    if legacy {
        servers.push((host.to_owned(), port));
    }
}

struct PumpContext {
    framebuffer: Arc<Mutex<Framebuffer>>,
    presented: Arc<Mutex<Option<String>>>,
    trusted: Option<String>,
    proxy_failure: Option<Arc<Mutex<Option<legacy::Failure>>>>,
    legacy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The server wants Standard RDP Security: connect again through the proxy.
    RetryLegacy,
    /// The attempt is over; `failed` if it never connected and reported a failure.
    Ended { failed: bool },
}

/// Forward one attempt's events until a terminal one.
async fn pump(mut output: OutputEventReceiver, ctx: PumpContext, on_event: Arc<dyn Fn(SessionEvent) + Send + Sync>) -> Outcome {
    let mut connected = false;
    while let Some(event) = output.recv().await {
        let Some(mapped) = map_event(event, &ctx) else { continue };
        match &mapped {
            SessionEvent::Failed { kind: FailureKind::UnsupportedSecurity, .. } if !ctx.legacy => return Outcome::RetryLegacy,
            SessionEvent::Connected => connected = true,
            _ => {}
        }
        let failed = !connected && matches!(mapped, SessionEvent::Failed { .. });
        let terminal = matches!(mapped, SessionEvent::Failed { .. } | SessionEvent::Ended | SessionEvent::CertificateUntrusted { .. });
        on_event(mapped);
        if terminal {
            return Outcome::Ended { failed };
        }
    }
    Outcome::Ended { failed: false }
}

fn map_event(event: RdpOutputEvent, ctx: &PumpContext) -> Option<SessionEvent> {
    let trusted = ctx.trusted.as_deref();
    // The legacy proxy knows better why a connection through it ended.
    let proxy_failure = || ctx.proxy_failure.as_ref().and_then(|f| f.lock().expect("proxy failure").clone());
    match event {
        RdpOutputEvent::Connected => Some(SessionEvent::Connected),
        RdpOutputEvent::DesktopUpdate(update) => {
            let (pixels, width, height, r) = update.into_parts();
            let region = Region { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
            let first = ctx.framebuffer.lock().expect("framebuffer").apply(width.get(), height.get(), region, &pixels);
            first.then_some(SessionEvent::FrameReady)
        }
        RdpOutputEvent::Image { buffer, width, height } => {
            let first = ctx.framebuffer.lock().expect("framebuffer").replace(width.get(), height.get(), buffer);
            first.then_some(SessionEvent::FrameReady)
        }
        RdpOutputEvent::PointerDefault => Some(SessionEvent::Pointer(PointerUpdate::Default)),
        RdpOutputEvent::PointerHidden => Some(SessionEvent::Pointer(PointerUpdate::Hidden)),
        RdpOutputEvent::PointerBitmap(p) => Some(SessionEvent::Pointer(PointerUpdate::Bitmap(p))),
        RdpOutputEvent::ConnectionFailure(e) => {
            let seen = ctx.presented.lock().expect("certificate slot").clone();
            if let Some(fingerprint) = seen {
                if certs::check(trusted, &fingerprint) != Trust::Trusted {
                    return Some(SessionEvent::CertificateUntrusted { fingerprint, previous: trusted.map(str::to_owned) });
                }
            }
            tracing::warn!(error = %e.report(), "connection failed");
            if let Some(failure) = proxy_failure() {
                return Some(SessionEvent::Failed { kind: failure.kind, message: failure.message });
            }
            Some(SessionEvent::Failed { kind: classify_failure(&e), message: e.report().to_string() })
        }
        RdpOutputEvent::Terminated(Ok(reason)) => {
            tracing::info!(?reason, "session ended");
            Some(SessionEvent::Ended)
        }
        RdpOutputEvent::Terminated(Err(e)) => {
            tracing::warn!(error = %e.report(), "session terminated with error");
            if let Some(failure) = proxy_failure() {
                return Some(SessionEvent::Failed { kind: failure.kind, message: failure.message });
            }
            Some(SessionEvent::Failed { kind: FailureKind::Other, message: e.report().to_string() })
        }
        _ => None,
    }
}

/// The folder to share, if one is set and still exists.
fn shared_folder(req: &SessionRequest) -> Option<PathBuf> {
    let folder = req.shared_folder.as_ref()?;
    if folder.is_dir() {
        Some(folder.clone())
    } else {
        tracing::warn!(folder = %folder.display(), "shared folder is missing; connecting without it");
        None
    }
}

pub fn classify_failure(err: &ConnectorError) -> FailureKind {
    match err.kind() {
        ConnectorErrorKind::Negotiation(failure)
            if failure.code() == ironrdp_pdu::nego::FailureCode::SSL_NOT_ALLOWED_BY_SERVER =>
        {
            return FailureKind::UnsupportedSecurity;
        }
        ConnectorErrorKind::Reason(reason) if reason.contains("server selected STANDARD_RDP_SECURITY") => {
            return FailureKind::UnsupportedSecurity;
        }
        _ => {}
    }
    if matches!(err.kind(), ConnectorErrorKind::Credssp(_) | ConnectorErrorKind::AccessDenied) {
        return FailureKind::BadCredentials;
    }
    if err.report().to_string().contains("TCP connect") {
        return FailureKind::Unreachable;
    }
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(err);
    while let Some(e) = source {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            if matches!(io.kind(), ConnectionRefused | TimedOut | HostUnreachable | NetworkUnreachable) {
                return FailureKind::Unreachable;
            }
        }
        source = e.source();
    }
    FailureKind::Other
}

/// `clipboard`: whether a clipboard backend comes with the client (`start_session` takes it
/// out of `req`, so `req.clipboard` is always `None` here).
fn build_config(
    req: &SessionRequest,
    presented: &Arc<Mutex<Option<String>>>,
    proxy: Option<&legacy::Proxy>,
    clipboard: bool,
) -> anyhow::Result<ironrdp_client::config::Config> {
    let (_, _, width, height) = virtual_desktop(&req.layout);
    let slot = Arc::clone(presented);
    let trusted = req.trusted_fingerprint.clone();
    let callback: ironrdp_tls::CertificateValidationCallback = Arc::new(move |der: &[u8], _endpoint: &str, _why: &str| {
        let fp = certs::fingerprint(der);
        let ok = certs::check(trusted.as_deref(), &fp) == Trust::Trusted;
        *slot.lock().expect("certificate slot") = Some(fp);
        ok
    });

    let mut builder = ConfigBuilder::new()
        .with_destination(Destination::from_parts(req.host.clone(), req.port))
        .with_username(req.username.clone())
        .with_password(req.password.clone())
        // TLS-only servers such as xrdp need INFO_AUTOLOGON to use the supplied
        // credentials rather than displaying their own login form.
        .with_autologon(!req.username.is_empty() && !req.password.is_empty())
        .with_client_build(CLIENT_BUILD)
        .with_client_dir("C:\\Windows\\System32\\mstscax.dll")
        .with_platform(MajorPlatformType::MACINTOSH)
        .with_client_name(CLIENT_NAME)
        .with_keyboard_layout(KEYBOARD_LAYOUT_US)
        .with_desktop_width(u16::try_from(width).unwrap_or(u16::MAX))
        .with_desktop_height(u16::try_from(height).unwrap_or(u16::MAX))
        .with_certificate_validation(CertificateValidation::Strict)
        .with_certificate_validation_callback(callback)
        .with_tcp_connect_attempt_timeout(TCP_CONNECT_TIMEOUT)
        // No bulk compression: servers compress the Font Map PDU of the Deactivation-
        // Reactivation sequence they use to resize, which IronRDP can't decode there
        // (seen with xrdp: resizing the window dropped the session).
        .with_compression(false)
        // Native macOS cursor from the server's pointer bitmaps (not drawn into the image).
        .with_server_pointer(true)
        .with_pointer_software_rendering(false)
        .with_sound(req.sound)
        .with_audio_capture(false)
        .with_clipboard(if clipboard { ClipboardType::Enable } else { ClipboardType::Disable })
        // Our own RDPDR channel (see `drive::channel`) replaces the client's built-in one.
        .with_rdpdr(false)
        .with_printers(false)
        .with_ports(false);
    if let Some(proxy) = proxy {
        // Standard RDP Security: IronRDP sends plain RDP to the proxy, which encrypts it.
        builder = builder.with_transport(TransportKind::RDCleanPath { url: proxy.url() }).with_rdcleanpath_token(proxy.token());
    }
    if let Some(domain) = &req.domain {
        builder = builder.with_domain(domain.clone());
    }
    if req.layout.len() > 1 {
        builder = builder.with_monitor_layout(client_monitor_data(&req.layout));
    }
    if let Some(folder) = shared_folder(req) {
        builder = builder.with_static_channel(move |_| match crate::session::drive::channel(&folder) {
            Ok(channel) => Some(channel),
            Err(e) => {
                tracing::warn!(error = %e, "could not share folder");
                None
            }
        });
    }
    builder.build()
}

/// Minimal valid config for the inert handle returned when the real one fails to build.
fn fallback_config() -> ironrdp_client::config::Config {
    ConfigBuilder::new()
        .with_destination(Destination::from_parts("127.0.0.1", 1))
        .with_username("")
        .with_password("")
        .with_client_build(CLIENT_BUILD)
        .with_client_dir("C:\\")
        .with_platform(MajorPlatformType::MACINTOSH)
        .with_client_name(CLIENT_NAME)
        .build()
        .expect("fallback config is complete")
}

fn client_monitor_data(layout: &[RemoteMonitor]) -> ClientMonitorData {
    ClientMonitorData {
        monitors: layout
            .iter()
            .map(|m| Monitor {
                left: m.left,
                top: m.top,
                right: m.left + m.width as i32 - 1,
                bottom: m.top + m.height as i32 - 1,
                flags: if m.primary { MonitorFlags::PRIMARY } else { MonitorFlags::empty() },
            })
            .collect(),
    }
}

fn display_control_layout(layout: &[RemoteMonitor]) -> anyhow::Result<DisplayControlMonitorLayout> {
    let entries = layout
        .iter()
        .map(|m| {
            let entry = if m.primary {
                MonitorLayoutEntry::new_primary(m.width, m.height)
            } else {
                MonitorLayoutEntry::new_secondary(m.width, m.height)
            }?;
            entry.with_position(m.left, m.top)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DisplayControlMonitorLayout::new(&entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_connector::{ClientConnector, ClientConnectorState, Sequence};
    use ironrdp_core::{decode, encode_vec, WriteBuf};
    use ironrdp_pdu::mcs::SendDataRequest;
    use ironrdp_pdu::nego::{ConnectionConfirm, FailureCode, ResponseFlags, SecurityProtocol};
    use ironrdp_pdu::rdp::client_info::{ClientInfo, ClientInfoFlags};
    use ironrdp_pdu::rdp::ClientInfoPdu;
    use ironrdp_pdu::x224::X224;

    fn request() -> SessionRequest {
        SessionRequest {
            host: "192.168.0.112".into(),
            port: 3389,
            username: "ai".into(),
            domain: None,
            password: String::new(),
            trusted_fingerprint: None,
            layout: vec![RemoteMonitor { left: 0, top: 0, width: 1280, height: 800, primary: true }],
            sound: true,
            shared_folder: None,
            clipboard: None,
        }
    }

    /// xrdp (and Windows) resize via a Deactivation-Reactivation sequence; with bulk
    /// compression negotiated the server compresses the reactivation's Font Map PDU, which
    /// IronRDP's reactivation step can't decompress ("unexpected server message").
    #[test]
    fn bulk_compression_is_not_offered() {
        let config = build_config(&request(), &Arc::new(Mutex::new(None)), None, false).unwrap();
        assert_eq!(config.connector().compression_type, None);
    }

    /// Decode the actual secure-settings packet, as a TLS-only server would.
    fn client_info(req: &SessionRequest) -> ClientInfo {
        let config = build_config(req, &Arc::new(Mutex::new(None)), None, false).unwrap();
        let mut connector = ClientConnector::new(config.connector().clone(), "127.0.0.1:12345".parse().unwrap());
        connector.state = ClientConnectorState::SecureSettingsExchange { io_channel_id: 1003, user_channel_id: 1002 };
        let mut out = WriteBuf::new();
        connector.step_no_input(&mut out).unwrap();
        let packet = decode::<X224<SendDataRequest<'_>>>(out.filled()).unwrap();
        decode::<ClientInfoPdu>(packet.0.user_data.as_ref()).unwrap().client_info
    }

    #[test]
    fn supplied_credentials_request_auto_login_on_the_wire() {
        let mut req = request();
        req.password = " p@ss wörd 🔑 ".into();
        req.domain = Some("CORP".into());
        let info = client_info(&req);
        assert!(info.flags.contains(ClientInfoFlags::AUTOLOGON));
        assert_eq!(info.credentials.username, req.username);
        assert_eq!(info.credentials.password, req.password);
        assert_eq!(info.credentials.domain, req.domain);
    }

    #[test]
    fn missing_credentials_leave_auto_login_disabled() {
        let mut req = request();
        assert!(!client_info(&req).flags.contains(ClientInfoFlags::AUTOLOGON));
        req.username.clear();
        req.password = "password".into();
        assert!(!client_info(&req).flags.contains(ClientInfoFlags::AUTOLOGON));
    }

    #[test]
    fn legacy_security_responses_are_classified_before_credentials_are_sent() {
        let responses = [
            ConnectionConfirm::Response { flags: ResponseFlags::empty(), protocol: SecurityProtocol::empty() },
            ConnectionConfirm::Failure { code: FailureCode::SSL_NOT_ALLOWED_BY_SERVER },
        ];
        for response in responses {
            let config = build_config(&request(), &Arc::new(Mutex::new(None)), None, false).unwrap();
            assert!(config.connector().enable_tls && config.connector().enable_credssp);
            assert!(!config.connector().enable_standard_rdp_security);
            let mut connector = ClientConnector::new(config.connector().clone(), "127.0.0.1:12345".parse().unwrap());
            let mut out = WriteBuf::new();
            connector.step_no_input(&mut out).unwrap();
            out.clear();
            let response = encode_vec(&X224(response)).unwrap();
            let error = connector.step(&response, None, &mut out).unwrap_err();
            assert_eq!(classify_failure(&error), FailureKind::UnsupportedSecurity);
            assert!(out.filled().is_empty());
        }
    }

    fn pump_context(legacy: bool, proxy_failure: Option<legacy::Failure>) -> PumpContext {
        PumpContext {
            framebuffer: Arc::new(Mutex::new(Framebuffer::new())),
            presented: Arc::default(),
            trusted: None,
            proxy_failure: legacy.then(|| Arc::new(Mutex::new(proxy_failure))),
            legacy,
        }
    }

    fn legacy_refusal() -> ConnectorError {
        ironrdp_connector::reason_err!(
            "Initiation",
            "client advertised SSL | HYBRID | HYBRID_EX, but server selected STANDARD_RDP_SECURITY"
        )
    }

    async fn run_pump(ctx: PumpContext, event: RdpOutputEvent) -> (Outcome, Vec<SessionEvent>) {
        let (tx, rx) = output_channel(4);
        tx.send(event).await.unwrap();
        drop(tx);
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let outcome = pump(rx, ctx, Arc::new(move |e| sink.lock().unwrap().push(e))).await;
        let events = events.lock().unwrap().clone();
        (outcome, events)
    }

    #[tokio::test]
    async fn legacy_security_refusal_retries_through_the_proxy_silently() {
        let (outcome, events) = run_pump(pump_context(false, None), RdpOutputEvent::ConnectionFailure(legacy_refusal())).await;
        assert_eq!(outcome, Outcome::RetryLegacy);
        assert!(events.is_empty(), "the UI must not see the first attempt fail: {events:?}");
    }

    #[tokio::test]
    async fn proxy_attempts_report_their_failures_instead_of_retrying() {
        let (outcome, events) = run_pump(pump_context(true, None), RdpOutputEvent::ConnectionFailure(legacy_refusal())).await;
        assert_eq!(outcome, Outcome::Ended { failed: true });
        assert!(matches!(events[..], [SessionEvent::Failed { kind: FailureKind::UnsupportedSecurity, .. }]));
    }

    #[tokio::test]
    async fn the_proxy_explains_why_a_connection_failed() {
        let failure = legacy::Failure { kind: FailureKind::Unreachable, message: "TCP connect failed".into() };
        let error = ironrdp_connector::general_err!("read RDCleanPath response");
        let (_, events) = run_pump(pump_context(true, Some(failure)), RdpOutputEvent::ConnectionFailure(error)).await;
        match &events[..] {
            [SessionEvent::Failed { kind: FailureKind::Unreachable, message }] => assert_eq!(message, "TCP connect failed"),
            other => panic!("unexpected events {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_untrusted_legacy_server_asks_for_trust() {
        let ctx = pump_context(true, Some(legacy::Failure { kind: FailureKind::Other, message: "not trusted".into() }));
        *ctx.presented.lock().unwrap() = Some("sha256:00".into());
        let error = ironrdp_connector::general_err!("read frame");
        let (outcome, events) = run_pump(ctx, RdpOutputEvent::ConnectionFailure(error)).await;
        assert_eq!(outcome, Outcome::Ended { failed: false });
        assert!(matches!(&events[..], [SessionEvent::CertificateUntrusted { fingerprint, previous: None }] if fingerprint == "sha256:00"));
    }

    #[test]
    fn legacy_servers_are_remembered_per_host_and_port() {
        let host = "legacy.example.test";
        assert!(!uses_legacy_security(host, 3389));
        remember_legacy_security(host, 3389, true);
        remember_legacy_security(host, 3389, true);
        assert!(uses_legacy_security(host, 3389));
        assert!(!uses_legacy_security(host, 3390));
        remember_legacy_security(host, 3389, false);
        assert!(!uses_legacy_security(host, 3389));
    }
}
