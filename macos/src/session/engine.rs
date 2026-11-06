//! One RDP session: wraps `ironrdp_client::rdp::RdpClient` and bridges it to the UI.
//!
//! The client runs on its own thread (`Handle::block_on`), sharing the app's tokio
//! runtime. Screen updates are written into a shared [`Framebuffer`]; the UI is told
//! once per batch (`FrameReady`) and reads the image when it redraws.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironrdp_client::config::{ClipboardType, ConfigBuilder, Destination};
use ironrdp_client::output_channel::output_channel;
use ironrdp_client::rdp::{RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent};
use ironrdp_cliprdr::backend::CliprdrBackendFactory;
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
    input: RdpInputSender,
    framebuffer: Arc<Mutex<Framebuffer>>,
    keys: Mutex<Database>,
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
        if let Err(e) = self.inner.input.try_send(RdpInputEvent::FastPath(events)) {
            tracing::debug!(error = %e, "dropped input event");
        }
    }

    pub fn resize(&self, width: u16, height: u16) {
        let event = RdpInputEvent::Resize { width, height, scale_factor: 100, physical_size: None };
        if let Err(e) = self.inner.input.try_send(event) {
            tracing::debug!(error = %e, "dropped resize request");
        }
    }

    pub fn set_monitor_layout(&self, layout: &[RemoteMonitor]) {
        match display_control_layout(layout) {
            Ok(l) => {
                if let Err(e) = self.inner.input.try_send(RdpInputEvent::MonitorLayout(l)) {
                    tracing::debug!(error = %e, "dropped monitor layout request");
                }
            }
            Err(e) => tracing::warn!(error = %e, "invalid monitor layout"),
        }
    }

    pub fn input_sender(&self) -> RdpInputSender {
        self.inner.input.clone()
    }

    pub fn disconnect(&self) {
        self.inner.input.request_graceful_close();
    }

    pub fn framebuffer(&self) -> Arc<Mutex<Framebuffer>> {
        Arc::clone(&self.inner.framebuffer)
    }
}

/// Start connecting. Events arrive on tokio threads; the caller hops to the main thread.
pub fn start_session(
    rt: &tokio::runtime::Handle,
    req: SessionRequest,
    on_event: Box<dyn Fn(SessionEvent) + Send + Sync>,
) -> SessionHandle {
    tracing::info!(request = ?req, "starting session");
    let presented: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let framebuffer = Arc::new(Mutex::new(Framebuffer::new()));
    let (output_tx, mut output_rx) = output_channel(OUTPUT_QUEUE);

    let trusted = req.trusted_fingerprint.clone();
    let built = build_config(&req, &presented);
    let mut client = match built {
        Ok(config) => RdpClient::new(config, output_tx).with_desktop_updates(),
        Err(e) => {
            // Still return a usable (inert) handle; report the failure.
            let (tx, _rx) = output_channel(1);
            let client = RdpClient::new(fallback_config(), tx);
            let handle = SessionHandle { inner: Arc::new(Inner { input: client.input_sender(), framebuffer, keys: Mutex::new(Database::new()) }) };
            on_event(SessionEvent::Failed { kind: FailureKind::Other, message: format!("{e:#}") });
            return handle;
        }
    };
    if let Some(factory) = req.clipboard {
        client = client.with_cliprdr_backend_factory(factory);
    }

    let handle = SessionHandle {
        inner: Arc::new(Inner {
            input: client.input_sender(),
            framebuffer: Arc::clone(&framebuffer),
            keys: Mutex::new(Database::new()),
        }),
    };

    let pump_rt = rt.clone();
    let pump = rt.spawn(async move {
        while let Some(event) = output_rx.recv().await {
            if let Some(mapped) = map_event(event, &framebuffer, &presented, trusted.as_deref()) {
                let terminal = matches!(mapped, SessionEvent::Failed { .. } | SessionEvent::Ended | SessionEvent::CertificateUntrusted { .. });
                on_event(mapped);
                if terminal {
                    break;
                }
            }
        }
    });

    let client_rt = rt.clone();
    let spawned = std::thread::Builder::new().name("rdp-session".into()).spawn(move || {
        client_rt.block_on(client.run());
        // Let the pump deliver the final event before the thread ends.
        let _ = pump_rt.block_on(pump);
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "could not spawn session thread");
    }
    handle
}

fn map_event(
    event: RdpOutputEvent,
    framebuffer: &Mutex<Framebuffer>,
    presented: &Mutex<Option<String>>,
    trusted: Option<&str>,
) -> Option<SessionEvent> {
    match event {
        RdpOutputEvent::Connected => Some(SessionEvent::Connected),
        RdpOutputEvent::DesktopUpdate(update) => {
            let (pixels, width, height, r) = update.into_parts();
            let region = Region { left: r.left, top: r.top, right: r.right, bottom: r.bottom };
            let first = framebuffer.lock().expect("framebuffer").apply(width.get(), height.get(), region, &pixels);
            first.then_some(SessionEvent::FrameReady)
        }
        RdpOutputEvent::Image { buffer, width, height } => {
            let first = framebuffer.lock().expect("framebuffer").replace(width.get(), height.get(), buffer);
            first.then_some(SessionEvent::FrameReady)
        }
        RdpOutputEvent::PointerDefault => Some(SessionEvent::Pointer(PointerUpdate::Default)),
        RdpOutputEvent::PointerHidden => Some(SessionEvent::Pointer(PointerUpdate::Hidden)),
        RdpOutputEvent::PointerBitmap(p) => Some(SessionEvent::Pointer(PointerUpdate::Bitmap(p))),
        RdpOutputEvent::ConnectionFailure(e) => {
            let seen = presented.lock().expect("certificate slot").clone();
            if let Some(fingerprint) = seen {
                if certs::check(trusted, &fingerprint) != Trust::Trusted {
                    return Some(SessionEvent::CertificateUntrusted { fingerprint, previous: trusted.map(str::to_owned) });
                }
            }
            tracing::warn!(error = %e.report(), "connection failed");
            Some(SessionEvent::Failed { kind: classify_failure(&e), message: e.report().to_string() })
        }
        RdpOutputEvent::Terminated(Ok(reason)) => {
            tracing::info!(?reason, "session ended");
            Some(SessionEvent::Ended)
        }
        RdpOutputEvent::Terminated(Err(e)) => {
            tracing::warn!(error = %e.report(), "session terminated with error");
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

fn build_config(req: &SessionRequest, presented: &Arc<Mutex<Option<String>>>) -> anyhow::Result<ironrdp_client::config::Config> {
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
        .with_clipboard(if req.clipboard.is_some() { ClipboardType::Enable } else { ClipboardType::Disable })
        // Our own RDPDR channel (see `drive::channel`) replaces the client's built-in one.
        .with_rdpdr(false)
        .with_printers(false)
        .with_ports(false);
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
        let config = build_config(&request(), &Arc::new(Mutex::new(None))).unwrap();
        assert_eq!(config.connector().compression_type, None);
    }
}
