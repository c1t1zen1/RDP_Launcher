//! Standard RDP Security for IronRDP, through a loopback proxy.
//!
//! IronRDP only speaks TLS or NLA, but it can also reach a server through an RDCleanPath
//! proxy: it sends plain RDP over a WebSocket and leaves the secure transport to the proxy.
//! This proxy speaks Standard RDP Security to the server instead (MS-RDPBCGR 5.3, see
//! [`rdpsec`]): it sends the RSA-encrypted client random, then encrypts and signs every
//! PDU with RC4. Towards IronRDP the connection looks like a TLS session.
//!
//! The server's certificate goes through the same trust-on-first-use check as TLS
//! certificates. Only loopback clients holding the per-session token are served.

use std::borrow::Cow;
use std::io;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use ironrdp_core::{decode, encode_vec, Encode};
use ironrdp_pdu::gcc::{
    ClientSecurityData, ConferenceCreateResponse, EncryptionMethod, ServerSecurityData,
};
use ironrdp_pdu::mcs::{self, SendDataIndication, SendDataRequest};
use ironrdp_pdu::nego::{ConnectionConfirm, ConnectionRequest, SecurityProtocol};
use ironrdp_pdu::x224::{X224Data, X224};
use ironrdp_rdcleanpath::{DetectionResult, RDCleanPathMessage, RDCleanPathPdu};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::WebSocketStream;
use url::Url;

use crate::certs::{self, Trust};
use crate::session::engine::FailureKind;
use crate::session::rdpsec::{
    Cipher, ServerKey, SessionKeys, Strength, CLIENT_RANDOM_LEN, SEC_AUTODETECT_REQ, SEC_AUTODETECT_RSP,
    SEC_ENCRYPT, SEC_EXCHANGE_PKT, SEC_HEARTBEAT, SEC_INFO_PKT, SEC_LICENSE_ENCRYPT_CS, SEC_LICENSE_PKT,
    SEC_REDIRECTION_PKT, SEC_SECURE_CHECKSUM, SEC_TRANSPORT_REQ, SEC_TRANSPORT_RSP, SIGNATURE_LEN,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// IronRDP needs an X.509 certificate in the RDCleanPath response, but reads only its
/// public key, for NLA, which is never used here. This one's private key was discarded.
const PLACEHOLDER_CERT: &[u8] = include_bytes!("legacy/placeholder-cert.der");

/// The GCC node ID in the rewritten Connect Response; IronRDP doesn't use it.
const GCC_NODE_ID: u16 = 1002;

/// Server-to-client PDUs that keep a Basic Security Header under TLS.
const SERVER_HEADER_PDUS: u16 = SEC_LICENSE_PKT | SEC_REDIRECTION_PKT | SEC_AUTODETECT_REQ | SEC_HEARTBEAT | SEC_TRANSPORT_REQ;
/// Client-to-server PDUs that IronRDP sends with a Basic Security Header.
const CLIENT_HEADER_PDUS: u16 = SEC_INFO_PKT | SEC_LICENSE_PKT | SEC_AUTODETECT_RSP | SEC_TRANSPORT_RSP;

const FASTPATH_SECURE_CHECKSUM: u8 = 0x1;
const FASTPATH_ENCRYPTED: u8 = 0x2;

type ClientSink = SplitSink<WebSocketStream<TcpStream>, Message>;
type ClientStream = SplitStream<WebSocketStream<TcpStream>>;

/// Why the proxy gave up, for the session's error report.
#[derive(Debug, Clone)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    fn other(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Other, message)
    }
}

struct Shared {
    host: String,
    port: u16,
    token: String,
    trusted: Option<String>,
    presented: Arc<Mutex<Option<String>>>,
    failure: Arc<Mutex<Option<Failure>>>,
}

/// A running proxy for one session. Dropping it stops the proxy and its connections.
pub struct Proxy {
    url: Url,
    token: String,
    failure: Arc<Mutex<Option<Failure>>>,
    task: JoinHandle<()>,
}

impl Proxy {
    /// Listen on a loopback port for IronRDP's connections to `host:port`. The server's
    /// certificate fingerprint is stored in `presented` and must match `trusted`.
    pub fn start(
        rt: &tokio::runtime::Handle,
        host: &str,
        port: u16,
        trusted: Option<String>,
        presented: Arc<Mutex<Option<String>>>,
    ) -> io::Result<Self> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let url = Url::parse(&format!("ws://{}/", listener.local_addr()?)).map_err(io::Error::other)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let failure = Arc::new(Mutex::new(None));
        let shared = Arc::new(Shared {
            host: host.to_owned(),
            port,
            token: token.clone(),
            trusted,
            presented,
            failure: Arc::clone(&failure),
        });
        let task = rt.spawn(accept_loop(listener, shared));
        Ok(Self { url, token, failure, task })
    }

    /// The RDCleanPath URL to give IronRDP.
    pub fn url(&self) -> Url {
        self.url.clone()
    }

    /// The RDCleanPath token to give IronRDP.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Where the proxy reports why it closed a connection.
    pub fn failure(&self) -> Arc<Mutex<Option<Failure>>> {
        Arc::clone(&self.failure)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept_loop(listener: std::net::TcpListener, shared: Arc<Shared>) {
    let listener = match TcpListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            *shared.failure.lock().expect("proxy failure") = Some(Failure::other(format!("legacy proxy: {e}")));
            return;
        }
    };
    // Owned here so that aborting this task also ends the connections.
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((socket, _)) => {
                    let shared = Arc::clone(&shared);
                    connections.spawn(async move { serve(socket, &shared).await });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "legacy proxy stopped accepting");
                    return;
                }
            },
            Some(_) = connections.join_next() => {}
        }
    }
}

async fn serve(socket: TcpStream, shared: &Shared) {
    let _ = socket.set_nodelay(true);
    let ws = match tokio_tungstenite::accept_async(socket).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!(error = %e, "legacy proxy: WebSocket handshake failed");
            return;
        }
    };
    let (mut to_client, mut from_client) = ws.split();
    let mut client_frames = Frames::default();

    // Only the session's own client, holding the token, may use the proxy.
    let x224_connection_request = match client_frames.next_rdcleanpath(&mut from_client).await.map(RDCleanPathPdu::into_message) {
        Ok(Ok(RDCleanPathMessage::Request { proxy_auth, x224_connection_request, .. })) if proxy_auth == shared.token => {
            x224_connection_request
        }
        _ => {
            tracing::warn!("legacy proxy: rejected a connection without a valid request");
            return;
        }
    };

    if let Err(failure) = relay(x224_connection_request.as_bytes(), &mut to_client, &mut from_client, client_frames, shared).await {
        tracing::warn!(message = %failure.message, "Standard RDP Security connection closed");
        // Before the WebSocket closes, so that the session reports this reason.
        *shared.failure.lock().expect("proxy failure") = Some(failure);
    }
}

async fn relay(
    x224_connection_request: &[u8],
    to_client: &mut ClientSink,
    from_client: &mut ClientStream,
    client_frames: Frames,
    shared: &Shared,
) -> Result<(), Failure> {
    let server = connect(&shared.host, shared.port).await.map_err(|e| {
        Failure::new(FailureKind::Unreachable, format!("TCP connect to {}:{} failed: {e}", shared.host, shared.port))
    })?;
    let _ = server.set_nodelay(true);
    let server_addr = server.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let (mut from_server, mut to_server) = server.into_split();
    let mut server_frames = Frames::default();

    // X.224: ask the server for Standard RDP Security, tell IronRDP it got TLS.
    let mut request = decode::<X224<ConnectionRequest>>(x224_connection_request)
        .map_err(|e| Failure::other(format!("decode Connection Request: {e}")))?
        .0;
    let requested = request.protocol;
    request.protocol = SecurityProtocol::empty();
    write_server(&mut to_server, &encode(&X224(request))?).await?;
    let confirm = server_frames
        .next_tcp(&mut from_server)
        .await?
        .ok_or_else(|| Failure::other("the server closed the connection during negotiation"))?;
    let response = match decode::<X224<ConnectionConfirm>>(&confirm)
        .map_err(|e| Failure::other(format!("decode Connection Confirm: {e}")))?
        .0
    {
        ConnectionConfirm::Response { flags, protocol } if protocol.is_standard_rdp_security() => {
            let tls = encode(&X224(ConnectionConfirm::Response { flags, protocol: SecurityProtocol::SSL }))?;
            RDCleanPathPdu::new_response(server_addr, tls, [PLACEHOLDER_CERT.to_vec()])
        }
        ConnectionConfirm::Response { protocol, .. } => {
            return Err(Failure::other(format!("the server selected {protocol} instead of Standard RDP Security")));
        }
        // IronRDP reports the server's reason.
        ConnectionConfirm::Failure { .. } => RDCleanPathPdu::new_negotiation_error(confirm),
    };
    let response = response
        .and_then(|pdu| pdu.to_der())
        .map_err(|e| Failure::other(format!("encode RDCleanPath response: {e}")))?;
    send_client(to_client, response).await?;

    let (keys_tx, keys_rx) = oneshot::channel();
    let license_encrypted = AtomicBool::new(false);
    tokio::select! {
        r = downstream(from_server, server_frames, to_client, keys_tx, requested, shared, &license_encrypted) => r,
        r = upstream(from_client, client_frames, to_server, keys_rx, &license_encrypted) => r,
    }
}

/// Client-to-server state, set up when the server's security data arrives.
struct Outgoing {
    cipher: Cipher,
    /// The encrypted client random, sent once before the first PDU on a channel.
    exchange: Option<Vec<u8>>,
    io_channel: u16,
    message_channel: Option<u16>,
}

/// Server → IronRDP: rewrite the Connect Response, decrypt everything after it.
async fn downstream(
    mut server: OwnedReadHalf,
    mut frames: Frames,
    client: &mut ClientSink,
    keys: oneshot::Sender<Outgoing>,
    requested: SecurityProtocol,
    shared: &Shared,
    license_encrypted: &AtomicBool,
) -> Result<(), Failure> {
    let mut keys = Some(keys);
    let mut incoming: Option<Cipher> = None;
    while let Some(frame) = frames.next_tcp(&mut server).await? {
        let out = match pdu_kind(&frame) {
            PduKind::ConnectResponse => {
                let (out, outgoing, cipher) = connect_response(&frame, requested, shared)?;
                incoming = Some(cipher);
                if let Some(keys) = keys.take() {
                    let _ = keys.send(outgoing);
                }
                out
            }
            PduKind::SendDataIndication => open_send_data(&frame, incoming.as_mut(), license_encrypted)?,
            PduKind::FastPath => open_fast_path(&frame, incoming.as_mut())?,
            _ => frame,
        };
        send_client(client, out).await?;
    }
    let _ = client.close().await;
    Ok(())
}

/// IronRDP → server: rewrite the Connect Initial, encrypt everything after it.
async fn upstream(
    client: &mut ClientStream,
    mut frames: Frames,
    mut server: OwnedWriteHalf,
    keys: oneshot::Receiver<Outgoing>,
    license_encrypted: &AtomicBool,
) -> Result<(), Failure> {
    let mut keys = Some(keys);
    let mut outgoing: Option<Outgoing> = None;
    while let Some(frame) = frames.next_ws(client).await? {
        let kind = pdu_kind(&frame);
        if matches!(kind, PduKind::SendDataRequest | PduKind::FastPath) && outgoing.is_none() {
            let keys = keys.take().ok_or_else(|| Failure::other("legacy proxy: no session keys"))?;
            outgoing = Some(keys.await.map_err(|_| Failure::other("the server sent no security data"))?);
        }
        match (kind, outgoing.as_mut()) {
            (PduKind::ConnectInitial, _) => write_server(&mut server, &connect_initial(&frame)?).await?,
            (PduKind::SendDataRequest, Some(out)) => {
                for pdu in seal_send_data(&frame, out, license_encrypted)? {
                    write_server(&mut server, &pdu).await?;
                }
            }
            (PduKind::FastPath, Some(out)) => write_server(&mut server, &seal_fast_path(&frame, &mut out.cipher)?).await?,
            _ => write_server(&mut server, &frame).await?,
        }
    }
    let _ = server.shutdown().await;
    Ok(())
}

/// Offer the server RC4 encryption in place of the TLS IronRDP believes it negotiated.
fn connect_initial(frame: &[u8]) -> Result<Vec<u8>, Failure> {
    let data = decode::<X224<X224Data<'_>>>(frame).map_err(|e| Failure::other(format!("decode Connect Initial: {e}")))?.0.data;
    let mcs::ConnectInitial {
        conference_create_request,
        calling_domain_selector,
        called_domain_selector,
        upward_flag,
        target_parameters,
        min_parameters,
        max_parameters,
    } = decode(&data).map_err(|e| Failure::other(format!("decode Connect Initial: {e}")))?;
    let mut blocks = conference_create_request.into_gcc_blocks();
    blocks.core.optional_data.server_selected_protocol = Some(SecurityProtocol::empty());
    blocks.security = ClientSecurityData {
        encryption_methods: EncryptionMethod::BIT_40 | EncryptionMethod::BIT_56 | EncryptionMethod::BIT_128,
        ext_encryption_methods: 0,
    };
    let initial = mcs::ConnectInitial {
        conference_create_request: ironrdp_pdu::gcc::ConferenceCreateRequest::new(blocks)
            .map_err(|e| Failure::other(format!("encode Connect Initial: {e}")))?,
        calling_domain_selector,
        called_domain_selector,
        upward_flag,
        target_parameters,
        min_parameters,
        max_parameters,
    };
    x224_data(&initial)
}

/// Check the server's certificate, derive the session keys, and hide the encryption
/// from IronRDP. Returns the rewritten PDU and the two directions' ciphers.
fn connect_response(
    frame: &[u8],
    requested: SecurityProtocol,
    shared: &Shared,
) -> Result<(Vec<u8>, Outgoing, Cipher), Failure> {
    let data = decode::<X224<X224Data<'_>>>(frame).map_err(|e| Failure::other(format!("decode Connect Response: {e}")))?.0.data;
    let mcs::ConnectResponse { conference_create_response, called_connect_id, domain_parameters } =
        decode(&data).map_err(|e| Failure::other(format!("decode Connect Response: {e}")))?;
    let mut blocks = conference_create_response.into_gcc_blocks();
    let security = std::mem::replace(&mut blocks.security, ServerSecurityData::no_security());

    let strength = Strength::from_method(security.encryption_method).ok_or_else(|| {
        let message = if security.encryption_method.contains(EncryptionMethod::FIPS) {
            "The server requires FIPS encryption (xrdp: crypt_level=fips), which the built-in client doesn't support."
        } else {
            "The server offers no encryption at all (xrdp: crypt_level=none). The built-in client won't send your password unencrypted."
        };
        Failure::new(FailureKind::UnsupportedSecurity, message)
    })?;
    let server_random = security.server_random.ok_or_else(|| Failure::other("the server sent no server random"))?;
    let key = ServerKey::from_certificate(&security.server_cert)
        .map_err(|e| Failure::new(FailureKind::UnsupportedSecurity, format!("{e:#}")))?;

    let fingerprint = certs::fingerprint(&security.server_cert);
    *shared.presented.lock().expect("certificate slot") = Some(fingerprint.clone());
    if certs::check(shared.trusted.as_deref(), &fingerprint) != Trust::Trusted {
        return Err(Failure::other("the server's identity is not trusted yet"));
    }

    let mut client_random = [0; CLIENT_RANDOM_LEN];
    getrandom::fill(&mut client_random).map_err(|e| Failure::other(format!("client random: {e}")))?;
    let (cipher, incoming) = SessionKeys::derive(&client_random, &server_random, strength).client_ciphers();
    tracing::info!(?strength, level = ?security.encryption_level, "Standard RDP Security negotiated");

    if blocks.core.optional_data.client_requested_protocols.is_some() {
        blocks.core.optional_data.client_requested_protocols = Some(requested);
    }
    let outgoing = Outgoing {
        cipher,
        exchange: Some(key.encrypt_client_random(&client_random)),
        io_channel: blocks.network.io_channel,
        message_channel: blocks.message_channel.as_ref().map(|m| m.mcs_message_channel_id),
    };
    let response = mcs::ConnectResponse {
        conference_create_response: ConferenceCreateResponse::new(GCC_NODE_ID, blocks)
            .map_err(|e| Failure::other(format!("encode Connect Response: {e}")))?,
        called_connect_id,
        domain_parameters,
    };
    Ok((x224_data(&response)?, outgoing, incoming))
}

/// Decrypt a slow-path PDU and drop its security header unless IronRDP expects one.
fn open_send_data(frame: &[u8], cipher: Option<&mut Cipher>, license_encrypted: &AtomicBool) -> Result<Vec<u8>, Failure> {
    let pdu = decode::<X224<SendDataIndication<'_>>>(frame).map_err(|e| Failure::other(format!("decode server PDU: {e}")))?.0;
    let data = pdu.user_data.as_ref();
    if data.len() < 4 {
        return Ok(frame.to_vec());
    }
    let flags = u16::from_le_bytes([data[0], data[1]]);
    let body = if flags & SEC_ENCRYPT != 0 {
        let cipher = cipher.ok_or_else(|| Failure::other("encrypted PDU before the security exchange"))?;
        let signature = data.get(4..4 + SIGNATURE_LEN).ok_or_else(|| Failure::other("truncated encrypted PDU"))?;
        let mut body = data[4 + SIGNATURE_LEN..].to_vec();
        cipher.open(&mut body, signature, flags & SEC_SECURE_CHECKSUM != 0).map_err(|e| Failure::other(e.to_string()))?;
        body
    } else {
        data[4..].to_vec()
    };
    if flags & SEC_LICENSE_PKT != 0 {
        license_encrypted.store(flags & SEC_LICENSE_ENCRYPT_CS != 0, Ordering::Relaxed);
    }
    let user_data = if flags & SERVER_HEADER_PDUS != 0 {
        let mut headed = (flags & !(SEC_ENCRYPT | SEC_SECURE_CHECKSUM)).to_le_bytes().to_vec();
        headed.extend([0, 0]);
        headed.extend(body);
        headed
    } else {
        body
    };
    encode(&X224(SendDataIndication { initiator_id: pdu.initiator_id, channel_id: pdu.channel_id, user_data: Cow::Owned(user_data) }))
}

/// Encrypt a slow-path PDU, preceded by the Security Exchange PDU the first time.
fn seal_send_data(frame: &[u8], out: &mut Outgoing, license_encrypted: &AtomicBool) -> Result<Vec<Vec<u8>>, Failure> {
    let pdu = decode::<X224<SendDataRequest<'_>>>(frame).map_err(|e| Failure::other(format!("decode client PDU: {e}")))?.0;
    let data = pdu.user_data.as_ref();
    let headed = (pdu.channel_id == out.io_channel || Some(pdu.channel_id) == out.message_channel) && has_security_header(data);
    let (flags, body) = if headed { (u16::from_le_bytes([data[0], data[1]]), &data[4..]) } else { (0, data) };

    let mut pdus = Vec::with_capacity(2);
    if let Some(exchange) = out.exchange.take() {
        let mut exchange_data = SEC_EXCHANGE_PKT.to_le_bytes().to_vec();
        exchange_data.extend([0, 0]);
        exchange_data.extend(u32::try_from(exchange.len()).unwrap_or(u32::MAX).to_le_bytes());
        exchange_data.extend(exchange);
        pdus.push(encode(&X224(SendDataRequest {
            initiator_id: pdu.initiator_id,
            channel_id: pdu.channel_id,
            user_data: Cow::Owned(exchange_data),
        }))?);
    }
    // Licensing PDUs are encrypted only when the server asked for that.
    let user_data = if flags & SEC_LICENSE_PKT != 0 && !license_encrypted.load(Ordering::Relaxed) {
        data.to_vec()
    } else {
        let mut body = body.to_vec();
        let signature = out.cipher.seal(&mut body);
        let mut sealed = (flags | SEC_ENCRYPT).to_le_bytes().to_vec();
        sealed.extend([0, 0]);
        sealed.extend(signature);
        sealed.extend(body);
        sealed
    };
    pdus.push(encode(&X224(SendDataRequest { initiator_id: pdu.initiator_id, channel_id: pdu.channel_id, user_data: Cow::Owned(user_data) }))?);
    Ok(pdus)
}

/// Whether IronRDP put a Basic Security Header on this IO-channel PDU. A Share Control
/// header in its place has a non-zero PDU type where flagsHi would be.
fn has_security_header(data: &[u8]) -> bool {
    data.len() >= 4 && data[2..4] == [0, 0] && u16::from_le_bytes([data[0], data[1]]) & CLIENT_HEADER_PDUS != 0
}

fn open_fast_path(frame: &[u8], cipher: Option<&mut Cipher>) -> Result<Vec<u8>, Failure> {
    let (header, body) = fast_path_parts(frame)?;
    let flags = header >> 6;
    if flags & FASTPATH_ENCRYPTED == 0 {
        return Ok(fast_path_frame(header & 0x3f, &[body]));
    }
    let cipher = cipher.ok_or_else(|| Failure::other("encrypted update before the security exchange"))?;
    let signature = body.get(..SIGNATURE_LEN).ok_or_else(|| Failure::other("truncated encrypted update"))?;
    let mut data = body[SIGNATURE_LEN..].to_vec();
    cipher.open(&mut data, signature, flags & FASTPATH_SECURE_CHECKSUM != 0).map_err(|e| Failure::other(e.to_string()))?;
    Ok(fast_path_frame(header & 0x3f, &[&data]))
}

fn seal_fast_path(frame: &[u8], cipher: &mut Cipher) -> Result<Vec<u8>, Failure> {
    let (header, body) = fast_path_parts(frame)?;
    let mut data = body.to_vec();
    let signature = cipher.seal(&mut data);
    Ok(fast_path_frame((header & 0x3f) | (FASTPATH_ENCRYPTED << 6), &[&signature, &data]))
}

/// Header byte and everything after the 1- or 2-byte length.
fn fast_path_parts(frame: &[u8]) -> Result<(u8, &[u8]), Failure> {
    let length_size = if frame.get(1).is_some_and(|b| b & 0x80 != 0) { 2 } else { 1 };
    let body = frame.get(1 + length_size..).ok_or_else(|| Failure::other("truncated fast-path PDU"))?;
    Ok((frame[0], body))
}

fn fast_path_frame(header: u8, parts: &[&[u8]]) -> Vec<u8> {
    let body_len: usize = parts.iter().map(|p| p.len()).sum();
    let mut out = Vec::with_capacity(body_len + 3);
    out.push(header);
    if body_len + 2 <= 0x7f {
        out.push((body_len + 2) as u8);
    } else {
        let total = body_len + 3;
        out.extend([0x80 | (total >> 8) as u8, total as u8]);
    }
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PduKind {
    ConnectInitial,
    ConnectResponse,
    SendDataRequest,
    SendDataIndication,
    FastPath,
    Other,
}

fn pdu_kind(frame: &[u8]) -> PduKind {
    if frame.first().is_some_and(|b| b & 0x03 == 0) {
        return PduKind::FastPath;
    }
    // TPKT (4 bytes) and X.224 Data TPDU (3 bytes), then the MCS PDU.
    if frame.len() < 9 || frame[5] != 0xf0 {
        return PduKind::Other;
    }
    match (frame[7], frame[8]) {
        (0x7f, 0x65) => PduKind::ConnectInitial,
        (0x7f, 0x66) => PduKind::ConnectResponse,
        (choice, _) => match choice >> 2 {
            25 => PduKind::SendDataRequest,
            26 => PduKind::SendDataIndication,
            _ => PduKind::Other,
        },
    }
}

fn encode(pdu: &impl Encode) -> Result<Vec<u8>, Failure> {
    encode_vec(pdu).map_err(|e| Failure::other(format!("encode {}: {e}", pdu.name())))
}

fn x224_data(pdu: &impl Encode) -> Result<Vec<u8>, Failure> {
    encode(&X224(X224Data { data: Cow::Owned(encode(pdu)?) }))
}

async fn write_server(server: &mut OwnedWriteHalf, bytes: &[u8]) -> Result<(), Failure> {
    server.write_all(bytes).await.map_err(|e| Failure::other(format!("write to server: {e}")))
}

async fn send_client(client: &mut ClientSink, bytes: Vec<u8>) -> Result<(), Failure> {
    client.send(Message::binary(bytes)).await.map_err(|e| Failure::other(format!("write to client: {e}")))
}

async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    let mut last_error = None;
    for address in tokio::net::lookup_host((host, port)).await? {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last_error = Some(e),
            Err(_) => {
                last_error = Some(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{address} did not accept the connection within {CONNECT_TIMEOUT:?}"),
                ))
            }
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{host} resolved to no address"))))
}

/// Splits a byte stream into TPKT / fast-path PDUs.
#[derive(Default)]
struct Frames {
    buf: Vec<u8>,
}

impl Frames {
    fn take(&mut self) -> Result<Option<Vec<u8>>, Failure> {
        match ironrdp_pdu::find_size(&self.buf) {
            Ok(Some(info)) if self.buf.len() >= info.length => Ok(Some(self.buf.drain(..info.length).collect())),
            Ok(_) => Ok(None),
            Err(e) => Err(Failure::other(format!("unexpected data in the RDP stream: {e}"))),
        }
    }

    async fn next_tcp(&mut self, src: &mut OwnedReadHalf) -> Result<Option<Vec<u8>>, Failure> {
        loop {
            if let Some(frame) = self.take()? {
                return Ok(Some(frame));
            }
            let read = src.read_buf(&mut self.buf).await.map_err(|e| Failure::other(format!("read from server: {e}")))?;
            if read == 0 {
                return Ok(None);
            }
        }
    }

    async fn next_ws(&mut self, src: &mut ClientStream) -> Result<Option<Vec<u8>>, Failure> {
        loop {
            if let Some(frame) = self.take()? {
                return Ok(Some(frame));
            }
            if !self.fill_ws(src).await? {
                return Ok(None);
            }
        }
    }

    async fn next_rdcleanpath(&mut self, src: &mut ClientStream) -> Result<RDCleanPathPdu, Failure> {
        loop {
            match RDCleanPathPdu::detect(&self.buf) {
                DetectionResult::Detected { total_length, .. } if self.buf.len() >= total_length => {
                    let bytes: Vec<u8> = self.buf.drain(..total_length).collect();
                    return RDCleanPathPdu::from_der(&bytes).map_err(|e| Failure::other(format!("decode RDCleanPath request: {e}")));
                }
                DetectionResult::Failed => return Err(Failure::other("legacy proxy: not an RDCleanPath request")),
                _ => {}
            }
            if !self.fill_ws(src).await? {
                return Err(Failure::other("legacy proxy: the client left before connecting"));
            }
        }
    }

    /// Append the next binary message; false once the client has gone.
    async fn fill_ws(&mut self, src: &mut ClientStream) -> Result<bool, Failure> {
        loop {
            match src.next().await {
                Some(Ok(Message::Binary(data))) => {
                    self.buf.extend_from_slice(&data);
                    return Ok(true);
                }
                Some(Ok(Message::Close(_))) | None => return Ok(false),
                Some(Ok(_)) => continue,
                // IronRDP drops the socket without a closing handshake when it disconnects.
                Some(Err(
                    WsError::ConnectionClosed | WsError::AlreadyClosed | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
                )) => return Ok(false),
                Some(Err(e)) => return Err(Failure::other(format!("read from client: {e}"))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::rdpsec::tests::{decrypt_client_random, test_certificate};
    use ironrdp_pdu::gcc::{
        ClientCoreData, ClientGccBlocks, EncryptionLevel, ServerCoreData, ServerCoreOptionalData, ServerGccBlocks,
        ServerNetworkData,
    };

    fn shared(trusted: Option<String>) -> Shared {
        Shared {
            host: "server".into(),
            port: 3389,
            token: "token".into(),
            trusted,
            presented: Arc::default(),
            failure: Arc::default(),
        }
    }

    fn server_blocks(security: ServerSecurityData) -> ServerGccBlocks {
        ServerGccBlocks {
            core: ServerCoreData {
                version: ironrdp_pdu::gcc::RdpVersion::V5_PLUS,
                optional_data: ServerCoreOptionalData {
                    client_requested_protocols: Some(SecurityProtocol::empty()),
                    early_capability_flags: None,
                },
            },
            network: ServerNetworkData { channel_ids: vec![1004], io_channel: 1003 },
            security,
            message_channel: None,
            multi_transport_channel: None,
        }
    }

    fn connect_response_frame(security: ServerSecurityData) -> Vec<u8> {
        let response = mcs::ConnectResponse {
            conference_create_response: ConferenceCreateResponse::new(0x79f3, server_blocks(security)).unwrap(),
            called_connect_id: 0,
            domain_parameters: mcs::DomainParameters::target(),
        };
        x224_data(&response).unwrap()
    }

    fn rc4_security(method: EncryptionMethod) -> ServerSecurityData {
        ServerSecurityData {
            encryption_method: method,
            encryption_level: EncryptionLevel::High,
            server_random: Some([0x55; 32]),
            server_cert: test_certificate(),
        }
    }

    fn send_data_request(channel_id: u16, user_data: Vec<u8>) -> Vec<u8> {
        encode(&X224(SendDataRequest { initiator_id: 1007, channel_id, user_data: Cow::Owned(user_data) })).unwrap()
    }

    fn send_data_indication(channel_id: u16, user_data: Vec<u8>) -> Vec<u8> {
        encode(&X224(SendDataIndication { initiator_id: 1002, channel_id, user_data: Cow::Owned(user_data) })).unwrap()
    }

    fn user_data(frame: &[u8]) -> Vec<u8> {
        decode::<X224<SendDataRequest<'_>>>(frame).unwrap().0.user_data.into_owned()
    }

    /// The proxy's view after the Connect Response, plus the server side's ciphers.
    fn negotiated() -> (Outgoing, Cipher, Cipher, Cipher) {
        let frame = connect_response_frame(rc4_security(EncryptionMethod::BIT_128));
        let shared = shared(Some(certs::fingerprint(&test_certificate())));
        let (_, outgoing, incoming) = connect_response(&frame, SecurityProtocol::SSL, &shared).unwrap();
        let random = decrypt_client_random(outgoing.exchange.as_ref().unwrap());
        let (server_out, server_in) = SessionKeys::derive(&random, &[0x55; 32], Strength::Bits128).server_ciphers();
        (outgoing, incoming, server_out, server_in)
    }

    #[test]
    fn connect_initial_offers_rc4_and_standard_security() {
        let mut blocks = ClientGccBlocks {
            core: ClientCoreData {
                version: ironrdp_pdu::gcc::RdpVersion::V5_PLUS,
                desktop_width: 1024,
                desktop_height: 768,
                color_depth: ironrdp_pdu::gcc::ColorDepth::Bpp8,
                sec_access_sequence: ironrdp_pdu::gcc::SecureAccessSequence::Del,
                keyboard_layout: 0x409,
                client_build: 1,
                client_name: "Mac".into(),
                keyboard_type: ironrdp_pdu::gcc::KeyboardType(4),
                keyboard_subtype: 0,
                keyboard_functional_keys_count: 12,
                ime_file_name: String::new(),
                // Each optional field requires the ones before it.
                optional_data: ironrdp_pdu::gcc::ClientCoreOptionalData {
                    post_beta2_color_depth: Some(ironrdp_pdu::gcc::ColorDepth::Bpp8),
                    client_product_id: Some(1),
                    serial_number: Some(0),
                    high_color_depth: Some(ironrdp_pdu::gcc::HighColorDepth::Bpp24),
                    supported_color_depths: Some(ironrdp_pdu::gcc::SupportedColorDepths::BPP24),
                    early_capability_flags: Some(ironrdp_pdu::gcc::ClientEarlyCapabilityFlags::empty()),
                    dig_product_id: Some(String::new()),
                    connection_type: Some(ironrdp_pdu::gcc::ConnectionType::Lan),
                    ..Default::default()
                },
            },
            security: ClientSecurityData::no_security(),
            network: None,
            cluster: None,
            monitor: None,
            message_channel: None,
            multi_transport_channel: None,
            monitor_extended: None,
        };
        blocks.core.optional_data.server_selected_protocol = Some(SecurityProtocol::SSL);
        let frame = x224_data(&mcs::ConnectInitial::with_gcc_blocks(blocks).unwrap()).unwrap();
        assert_eq!(pdu_kind(&frame), PduKind::ConnectInitial);

        let rewritten = connect_initial(&frame).unwrap();
        let data = decode::<X224<X224Data<'_>>>(&rewritten).unwrap().0.data;
        let blocks = decode::<mcs::ConnectInitial>(&data).unwrap().conference_create_request.into_gcc_blocks();
        assert_eq!(blocks.core.optional_data.server_selected_protocol, Some(SecurityProtocol::empty()));
        assert_eq!(
            blocks.security.encryption_methods,
            EncryptionMethod::BIT_40 | EncryptionMethod::BIT_56 | EncryptionMethod::BIT_128
        );
    }

    #[test]
    fn connect_response_hides_encryption_and_checks_identity() {
        let frame = connect_response_frame(rc4_security(EncryptionMethod::BIT_128));
        assert_eq!(pdu_kind(&frame), PduKind::ConnectResponse);

        // Unknown server: the fingerprint is presented and the connection stops.
        let first = shared(None);
        assert!(connect_response(&frame, SecurityProtocol::SSL, &first).is_err());
        let fingerprint = first.presented.lock().unwrap().clone().unwrap();
        assert_eq!(fingerprint, certs::fingerprint(&test_certificate()));

        let trusted = shared(Some(fingerprint));
        let (rewritten, outgoing, _) = connect_response(&frame, SecurityProtocol::SSL, &trusted).unwrap();
        let data = decode::<X224<X224Data<'_>>>(&rewritten).unwrap().0.data;
        let blocks = decode::<mcs::ConnectResponse>(&data).unwrap().conference_create_response.into_gcc_blocks();
        assert_eq!(blocks.security, ServerSecurityData::no_security());
        assert_eq!(blocks.core.optional_data.client_requested_protocols, Some(SecurityProtocol::SSL));
        assert_eq!(outgoing.io_channel, 1003);
        assert_eq!(outgoing.exchange.as_ref().unwrap().len(), 64 + 8);
    }

    #[test]
    fn fips_and_unencrypted_servers_are_refused() {
        let fips = connect_response_frame(ServerSecurityData { encryption_level: EncryptionLevel::Fips, ..rc4_security(EncryptionMethod::FIPS) });
        let failure = connect_response(&fips, SecurityProtocol::SSL, &shared(None)).err().unwrap();
        assert_eq!(failure.kind, FailureKind::UnsupportedSecurity);
        assert!(failure.message.contains("FIPS"));

        let none = connect_response_frame(ServerSecurityData::no_security());
        let failure = connect_response(&none, SecurityProtocol::SSL, &shared(None)).err().unwrap();
        assert_eq!(failure.kind, FailureKind::UnsupportedSecurity);
        assert!(failure.message.contains("no encryption"));
    }

    #[test]
    fn client_info_follows_the_security_exchange_encrypted() {
        let (mut outgoing, _, _, mut server_in) = negotiated();
        let mut info = SEC_INFO_PKT.to_le_bytes().to_vec();
        info.extend([0, 0]);
        info.extend(b"client info");
        let pdus = seal_send_data(&send_data_request(1003, info), &mut outgoing, &AtomicBool::new(false)).unwrap();
        assert_eq!(pdus.len(), 2);

        let exchange = user_data(&pdus[0]);
        assert_eq!(u16::from_le_bytes([exchange[0], exchange[1]]), SEC_EXCHANGE_PKT);
        assert_eq!(u32::from_le_bytes(exchange[4..8].try_into().unwrap()), 72);

        let sealed = user_data(&pdus[1]);
        assert_eq!(u16::from_le_bytes([sealed[0], sealed[1]]), SEC_INFO_PKT | SEC_ENCRYPT);
        let mut body = sealed[12..].to_vec();
        server_in.open(&mut body, &sealed[4..12], false).unwrap();
        assert_eq!(body, b"client info");

        // Later PDUs: no second exchange; a Share Control PDU gets a fresh header.
        let share_control = vec![0x10, 0x00, 0x17, 0x00, 0xea, 0x03, 1, 2, 3, 4];
        let pdus = seal_send_data(&send_data_request(1003, share_control.clone()), &mut outgoing, &AtomicBool::new(false)).unwrap();
        assert_eq!(pdus.len(), 1);
        let sealed = user_data(&pdus[0]);
        assert_eq!(u16::from_le_bytes([sealed[0], sealed[1]]), SEC_ENCRYPT);
        let mut body = sealed[12..].to_vec();
        server_in.open(&mut body, &sealed[4..12], false).unwrap();
        assert_eq!(body, share_control);
    }

    #[test]
    fn virtual_channel_data_is_never_mistaken_for_a_header() {
        let (mut outgoing, _, _, mut server_in) = negotiated();
        outgoing.exchange = None;
        // CHANNEL_PDU_HEADER with length 0x40 looks like SEC_INFO_PKT on the IO channel.
        let channel_data = vec![0x40, 0, 0, 0, 0x03, 0, 0, 0, 9, 9];
        let pdus = seal_send_data(&send_data_request(1004, channel_data.clone()), &mut outgoing, &AtomicBool::new(false)).unwrap();
        let sealed = user_data(&pdus[0]);
        assert_eq!(u16::from_le_bytes([sealed[0], sealed[1]]), SEC_ENCRYPT);
        let mut body = sealed[12..].to_vec();
        server_in.open(&mut body, &sealed[4..12], false).unwrap();
        assert_eq!(body, channel_data);
    }

    #[test]
    fn unencrypted_license_pdus_pass_through_until_the_server_asks() {
        let (mut outgoing, _, _, _) = negotiated();
        outgoing.exchange = None;
        let mut license = SEC_LICENSE_PKT.to_le_bytes().to_vec();
        license.extend([0, 0, 0x13, 0x83, 4, 0]);
        let license_encrypted = AtomicBool::new(false);
        let pdus = seal_send_data(&send_data_request(1003, license.clone()), &mut outgoing, &license_encrypted).unwrap();
        assert_eq!(user_data(&pdus[0]), license);

        license_encrypted.store(true, Ordering::Relaxed);
        let pdus = seal_send_data(&send_data_request(1003, license), &mut outgoing, &license_encrypted).unwrap();
        let sealed = user_data(&pdus[0]);
        assert_eq!(u16::from_le_bytes([sealed[0], sealed[1]]), SEC_LICENSE_PKT | SEC_ENCRYPT);
    }

    #[test]
    fn server_pdus_are_decrypted_and_unwrapped() {
        let (_, mut incoming, mut server_out, _) = negotiated();
        let license_encrypted = AtomicBool::new(false);

        // Share Data: header removed.
        let mut body = b"demand active".to_vec();
        let signature = server_out.seal(&mut body);
        let mut data = SEC_ENCRYPT.to_le_bytes().to_vec();
        data.extend([0, 0]);
        data.extend(signature);
        data.extend(body);
        let out = open_send_data(&send_data_indication(1003, data), Some(&mut incoming), &license_encrypted).unwrap();
        let pdu = decode::<X224<SendDataIndication<'_>>>(&out).unwrap().0;
        assert_eq!(pdu.user_data.as_ref(), b"demand active");

        // Licensing: header kept, without the encryption flags.
        let mut data = (SEC_LICENSE_PKT | SEC_LICENSE_ENCRYPT_CS).to_le_bytes().to_vec();
        data.extend([0, 0]);
        data.extend(b"valid client");
        let out = open_send_data(&send_data_indication(1003, data.clone()), Some(&mut incoming), &license_encrypted).unwrap();
        let pdu = decode::<X224<SendDataIndication<'_>>>(&out).unwrap().0;
        assert_eq!(pdu.user_data.as_ref(), data.as_slice());
        assert!(license_encrypted.load(Ordering::Relaxed));
    }

    #[test]
    fn corrupted_server_pdus_are_rejected() {
        let (_, mut incoming, mut server_out, _) = negotiated();
        let mut body = b"update".to_vec();
        let signature = server_out.seal(&mut body);
        body[0] ^= 0xff;
        let mut data = SEC_ENCRYPT.to_le_bytes().to_vec();
        data.extend([0, 0]);
        data.extend(signature);
        data.extend(body);
        assert!(open_send_data(&send_data_indication(1003, data), Some(&mut incoming), &AtomicBool::new(false)).is_err());
    }

    #[test]
    fn fast_path_round_trips_with_short_and_long_lengths() {
        let (mut outgoing, mut incoming, mut server_out, mut server_in) = negotiated();
        for size in [3usize, 120, 125, 126, 2000] {
            // Server update: encrypted on the wire, plain for IronRDP.
            let plain: Vec<u8> = (0..size).map(|i| i as u8).collect();
            let mut data = plain.clone();
            let signature = server_out.seal(&mut data);
            let wire = fast_path_frame(FASTPATH_ENCRYPTED << 6, &[&signature, &data]);
            assert_eq!(pdu_kind(&wire), PduKind::FastPath);
            assert_eq!(ironrdp_pdu::find_size(&wire).unwrap().unwrap().length, wire.len());
            let out = open_fast_path(&wire, Some(&mut incoming)).unwrap();
            assert_eq!(out[0], 0);
            assert_eq!(ironrdp_pdu::find_size(&out).unwrap().unwrap().length, out.len());
            assert_eq!(fast_path_parts(&out).unwrap().1, plain.as_slice());

            // Client input: plain from IronRDP, encrypted for the server.
            let input = fast_path_frame(2 << 2, &[&plain]);
            let sealed = seal_fast_path(&input, &mut outgoing.cipher).unwrap();
            assert_eq!(sealed[0], (2 << 2) | (FASTPATH_ENCRYPTED << 6));
            let (_, body) = fast_path_parts(&sealed).unwrap();
            let mut data = body[SIGNATURE_LEN..].to_vec();
            server_in.open(&mut data, &body[..SIGNATURE_LEN], false).unwrap();
            assert_eq!(data, plain);
        }
    }

    #[test]
    fn frames_split_a_stream_into_pdus() {
        let first = send_data_indication(1003, vec![1, 2, 3, 4, 5]);
        let second = fast_path_frame(0, &[&[7; 200]]);
        let mut frames = Frames::default();
        frames.buf.extend(&first);
        frames.buf.extend(&second[..10]);
        assert_eq!(frames.take().unwrap(), Some(first));
        assert_eq!(frames.take().unwrap(), None);
        frames.buf.extend(&second[10..]);
        assert_eq!(frames.take().unwrap(), Some(second));
        assert!(frames.buf.is_empty());
    }
}
