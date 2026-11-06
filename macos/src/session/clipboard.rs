//! Text copy/paste between the Mac (NSPasteboard) and RDP sessions (CLIPRDR).
//!
//! One app-wide poller watches the pasteboard (macOS has no change notification);
//! each session has a backend that answers the server and a link that receives
//! local copies while the session is attached.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};

use ironrdp_client::rdp::RdpInputSender;
use ironrdp_cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp_core::{impl_as_any, IntoOwned};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString, NSTimer};

use crate::app::on_main;
use crate::session::clipboard_codec::{decode_unicode_text, encode_unicode_text, PasteboardWatcher, CF_UNICODETEXT};

const POLL_SECS: f64 = 0.5;

/// State shared between a session's backend (tokio thread) and its link (main thread).
#[derive(Debug, Default)]
struct Shared {
    /// The Mac's current clipboard text, offered to the server.
    local_text: Mutex<Option<String>>,
    sender: Mutex<Option<RdpInputSender>>,
}

impl Shared {
    fn send(&self, message: ClipboardMessage) {
        if let Some(sender) = self.sender.lock().expect("clipboard sender").as_ref() {
            if let Err(e) = sender.send_clipboard(message) {
                tracing::debug!(error = %e, "clipboard message dropped");
            }
        }
    }

    fn announce_local_text(&self) {
        if self.local_text.lock().expect("clipboard text").is_some() {
            let format = ClipboardFormat::new(ClipboardFormatId::new(CF_UNICODETEXT));
            self.send(ClipboardMessage::SendInitiateCopy(vec![format]));
        }
    }
}

#[derive(Debug)]
pub struct MacClipboardBackend {
    shared: Arc<Shared>,
}

impl_as_any!(MacClipboardBackend);

impl CliprdrBackend for MacClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // Text only: no file streaming, no clipboard data locking.
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        self.shared.announce_local_text();
    }

    fn on_request_format_list(&mut self) {
        self.shared.announce_local_text();
    }

    fn on_process_negotiated_capabilities(&mut self, _capabilities: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if available_formats.iter().any(|f| f.id().0 == CF_UNICODETEXT) {
            self.shared.send(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::new(CF_UNICODETEXT)));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let text = self.shared.local_text.lock().expect("clipboard text").clone();
        let response = match text {
            Some(t) if request.format.0 == CF_UNICODETEXT => FormatDataResponse::new_data(encode_unicode_text(&t)),
            _ => FormatDataResponse::new_error(),
        };
        self.shared.send(ClipboardMessage::SendFormatData(response.into_owned()));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        let text = decode_unicode_text(response.data());
        on_main(move |mtm| write_pasteboard(mtm, &text));
    }

    fn on_file_contents_request(&mut self, _request: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _response: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

pub struct MacClipboardFactory {
    shared: Arc<Shared>,
}

impl CliprdrBackendFactory for MacClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(MacClipboardBackend { shared: Arc::clone(&self.shared) })
    }
}

/// Main-thread side of one session's clipboard.
pub struct ClipboardLink {
    shared: Arc<Shared>,
}

impl ClipboardLink {
    /// Start delivering local copies to this session.
    pub fn attach(self: &Rc<Self>, sender: RdpInputSender) {
        *self.shared.sender.lock().expect("clipboard sender") = Some(sender);
        HUB.with(|h| h.borrow_mut().links.push(Rc::downgrade(self)));
    }

    pub fn detach(&self) {
        self.shared.sender.lock().expect("clipboard sender").take();
    }

    fn local_copy(&self, text: &str) {
        *self.shared.local_text.lock().expect("clipboard text") = Some(text.to_owned());
        self.shared.announce_local_text();
    }
}

/// A backend factory for `SessionRequest.clipboard` plus its main-thread link.
pub fn new_clipboard(mtm: MainThreadMarker) -> (MacClipboardFactory, Rc<ClipboardLink>) {
    start_poller(mtm);
    let shared = Arc::new(Shared::default());
    *shared.local_text.lock().expect("clipboard text") = current_text();
    (MacClipboardFactory { shared: Arc::clone(&shared) }, Rc::new(ClipboardLink { shared }))
}

// ---------- the app-wide pasteboard poller ----------

struct Hub {
    watcher: Option<PasteboardWatcher>,
    links: Vec<Weak<ClipboardLink>>,
    poller: Option<Retained<ClipboardPoller>>,
    timer: Option<Retained<NSTimer>>,
}

thread_local! {
    static HUB: RefCell<Hub> = const { RefCell::new(Hub { watcher: None, links: Vec::new(), poller: None, timer: None }) };
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherClipboardPoller"]
    struct ClipboardPoller;

    unsafe impl NSObjectProtocol for ClipboardPoller {}

    impl ClipboardPoller {
        #[unsafe(method(poll:))]
        fn poll(&self, _timer: Option<&AnyObject>) {
            poll_pasteboard();
        }
    }
);

fn current_text() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    pb.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string())
}

fn start_poller(mtm: MainThreadMarker) {
    HUB.with(|h| {
        let mut hub = h.borrow_mut();
        if hub.timer.is_some() {
            return;
        }
        hub.watcher = Some(PasteboardWatcher::new(NSPasteboard::generalPasteboard().changeCount()));
        let this = ClipboardPoller::alloc(mtm).set_ivars(());
        let poller: Retained<ClipboardPoller> = unsafe { msg_send![super(this), init] };
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                POLL_SECS,
                &poller,
                sel!(poll:),
                None,
                true,
            )
        };
        hub.poller = Some(poller);
        hub.timer = Some(timer);
    });
}

fn poll_pasteboard() {
    let count = NSPasteboard::generalPasteboard().changeCount();
    let (copied, links) = HUB.with(|h| {
        let mut hub = h.borrow_mut();
        hub.links.retain(|l| l.strong_count() > 0);
        let copied = hub.watcher.as_mut().is_some_and(|w| w.observe(count));
        (copied, hub.links.iter().filter_map(Weak::upgrade).collect::<Vec<_>>())
    });
    if !copied {
        return;
    }
    let Some(text) = current_text() else { return }; // non-text copies are not shared
    for link in links {
        link.local_copy(&text);
    }
}

/// Remote → Mac: put text on the pasteboard without echoing it back to the servers.
fn write_pasteboard(_mtm: MainThreadMarker, text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    let count = pb.changeCount();
    HUB.with(|h| {
        if let Some(w) = h.borrow_mut().watcher.as_mut() {
            w.suppress_next(count);
        }
    });
    // Other sessions should still see the new text as the Mac's clipboard.
    let links = HUB.with(|h| h.borrow().links.iter().filter_map(Weak::upgrade).collect::<Vec<_>>());
    for link in links {
        *link.shared.local_text.lock().expect("clipboard text") = Some(text.to_owned());
    }
}
