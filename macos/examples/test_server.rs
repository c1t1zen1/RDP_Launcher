//! A tiny local RDP server for trying the app without a real Windows/xrdp box.
//!
//!     cargo run --example test_server            # listens on 127.0.0.1:3390
//!     cargo run --example test_server -- 3391    # another port
//!
//! Sign in as `user` / `password`. The desktop shows a test pattern: red top-left,
//! green top-right, blue bottom-left, white bottom-right, with a grey grid every
//! 100 px, so orientation, scaling and multi-monitor placement are easy to check.
//! Keyboard and mouse input, and resize requests, are printed to the terminal.
//! Uses IronRDP's self-signed test certificate from `tests/certs`.

use std::num::{NonZeroU16, NonZeroUsize};
use std::path::Path;
use std::sync::{Arc, Mutex};

use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::{
    BitmapUpdate, Credentials, DesktopSize, DisplayContext, DisplayUpdate, KeyboardEvent, MouseEvent, PixelFormat,
    RdpServer, RdpServerDisplay, RdpServerDisplayUpdates, RdpServerInputHandler, ServerResult, TlsIdentityCtx,
};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

#[derive(Default)]
struct Shared {
    size: (u16, u16),
    tx: Option<UnboundedSender<DisplayUpdate>>,
}

struct Display(Arc<Mutex<Shared>>);

struct Updates(UnboundedReceiver<DisplayUpdate>);

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        Ok(self.0.recv().await)
    }
}

fn pattern(w: u16, h: u16) -> DisplayUpdate {
    let (wu, hu) = (usize::from(w), usize::from(h));
    let mut data = Vec::with_capacity(wu * hu * 4);
    for y in 0..hu {
        for x in 0..wu {
            let (r, g, b) = if x % 100 == 0 || y % 100 == 0 {
                (128, 128, 128)
            } else {
                match (x < wu / 2, y < hu / 2) {
                    (true, true) => (220, 40, 40),
                    (false, true) => (40, 180, 60),
                    (true, false) => (40, 80, 220),
                    (false, false) => (240, 240, 240),
                }
            };
            data.extend_from_slice(&[b, g, r, 0xFF]);
        }
    }
    DisplayUpdate::Bitmap(BitmapUpdate {
        x: 0,
        y: 0,
        width: NonZeroU16::new(w).unwrap(),
        height: NonZeroU16::new(h).unwrap(),
        format: PixelFormat::BgrA32,
        data: data.into(),
        stride: NonZeroUsize::new(wu * 4).unwrap(),
    })
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        let (width, height) = self.0.lock().unwrap().size;
        DesktopSize { width, height }
    }

    async fn request_initial_size(&mut self, client: DesktopSize) -> DesktopSize {
        println!("client asked for {}x{}", client.width, client.height);
        self.0.lock().unwrap().size = (client.width, client.height);
        client
    }

    async fn updates(&mut self, _: DisplayContext) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        let (tx, rx) = unbounded_channel();
        let mut shared = self.0.lock().unwrap();
        let (w, h) = shared.size;
        let _ = tx.send(pattern(w, h));
        shared.tx = Some(tx);
        Ok(Box::new(Updates(rx)))
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        let (w, h) = layout.desktop_size();
        println!("resize requested: {w}x{h} across {} monitor(s)", layout.monitors().len());
        let (w, h) = (w as u16, h as u16);
        let mut shared = self.0.lock().unwrap();
        shared.size = (w, h);
        if let Some(tx) = &shared.tx {
            let _ = tx.send(DisplayUpdate::Resize(DesktopSize { width: w, height: h }));
            let _ = tx.send(pattern(w, h));
        }
    }

    async fn monitor_count(&mut self) -> u32 {
        4
    }

    async fn offers_display_control(&mut self) -> bool {
        true
    }
}

struct Input;

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        println!("key   {event:?}");
    }

    fn mouse(&mut self, event: MouseEvent) {
        if !matches!(event, MouseEvent::Move { .. }) {
            println!("mouse {event:?}");
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(3390);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/certs");
    let identity = TlsIdentityCtx::init_from_paths(&dir.join("server-cert.pem"), &dir.join("server-key.pem"))
        .expect("test certificate");
    let acceptor = identity.make_acceptor().expect("TLS acceptor");
    let shared = Arc::new(Mutex::new(Shared { size: (1280, 800), tx: None }));
    let mut server = RdpServer::builder()
        .with_addr(([127, 0, 0, 1], port))
        .with_hybrid(acceptor, identity.pub_key)
        .with_input_handler(Input)
        .with_display_handler(Display(shared))
        .build();
    server.set_credentials(Some(Credentials { username: "user".into(), password: "password".into(), domain: None }));
    println!("test RDP server on 127.0.0.1:{port} — sign in as user / password");
    let local = tokio::task::LocalSet::new();
    local.run_until(async move { server.run().await }).await.expect("server");
}
