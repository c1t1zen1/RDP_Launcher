//! Certificate-trust and connection-failure prompts for a session.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSAlertStyle};
use objc2_foundation::NSString;

use crate::certs::display_fingerprint;
use crate::session::engine::FailureKind;

pub struct AlertText {
    pub title: String,
    pub body: String,
    pub buttons: [&'static str; 2],
    /// Which button means "trust".
    pub accept_index: usize,
}

pub fn certificate_text(conn_name: &str, fingerprint: &str, previous: Option<&str>) -> AlertText {
    match previous {
        None => AlertText {
            title: format!("First connection to {conn_name}"),
            body: format!(
                "This server's identity certificate isn't from a trusted authority. Confirm its \
                 fingerprint, then trust it for future connections.\n\nSHA-256: {}",
                display_fingerprint(fingerprint)
            ),
            buttons: ["Trust and Connect", "Cancel"],
            accept_index: 0,
        },
        Some(old) => AlertText {
            title: format!("WARNING: {conn_name}'s identity changed"),
            body: format!(
                "The certificate is different from the one you trusted before. This can mean the \
                 server was reinstalled — or that someone is intercepting the connection.\n\n\
                 New SHA-256: {}\nPrevious: {}",
                display_fingerprint(fingerprint),
                display_fingerprint(old)
            ),
            buttons: ["Cancel", "Trust New Certificate"],
            accept_index: 1,
        },
    }
}

pub fn failure_title(kind: FailureKind, conn_name: &str) -> String {
    match kind {
        FailureKind::BadCredentials => format!("Couldn't sign in to {conn_name}"),
        FailureKind::Unreachable => format!("Can't reach {conn_name}"),
        FailureKind::UnsupportedSecurity => format!("{conn_name} requires legacy RDP security"),
        FailureKind::Other => format!("Connection to {conn_name} failed"),
    }
}

/// Ask whether to trust a server certificate. Returns true to trust.
pub fn confirm_certificate(mtm: MainThreadMarker, conn_name: &str, fingerprint: &str, previous: Option<&str>) -> bool {
    let text = certificate_text(conn_name, fingerprint, previous);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&text.title));
    alert.setInformativeText(&NSString::from_str(&text.body));
    alert.setAlertStyle(if previous.is_some() { NSAlertStyle::Critical } else { NSAlertStyle::Warning });
    for b in text.buttons {
        alert.addButtonWithTitle(&NSString::from_str(b));
    }
    let response = alert.runModal();
    let accepted = if text.accept_index == 0 { NSAlertFirstButtonReturn } else { NSAlertSecondButtonReturn };
    response == accepted
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureChoice {
    Reconnect,
    Edit,
    Close,
}

pub fn failure_hint(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::BadCredentials => "Check the username and password, or whether this account may use Remote Desktop.",
        FailureKind::Unreachable => {
            "Check the host name and that Remote Desktop is on and reachable from this Mac (same network or VPN). \
             Also check System Settings > Privacy & Security > Local Network."
        }
        FailureKind::UnsupportedSecurity => {
            "This server selected Standard RDP Security. The built-in client requires TLS or NLA.\n\n\
             For Linux/xrdp: set security_layer=tls in the [Globals] section of /etc/xrdp/xrdp.ini, \
             ensure xrdp can read its TLS certificate and private key, then restart the xrdp service.\n\n\
             For Windows: enable TLS or Network Level Authentication on the Remote Desktop server."
        }
        FailureKind::Other => "",
    }
}

pub fn connection_failed(mtm: MainThreadMarker, conn_name: &str, kind: FailureKind, message: &str) -> FailureChoice {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&failure_title(kind, conn_name)));
    let hint = failure_hint(kind);
    alert.setInformativeText(&NSString::from_str(format!("{hint}\n\n{message}").trim()));
    alert.setAlertStyle(NSAlertStyle::Warning);
    for b in ["Reconnect", "Edit Connection", "Close"] {
        alert.addButtonWithTitle(&NSString::from_str(b));
    }
    match alert.runModal() {
        r if r == NSAlertFirstButtonReturn => FailureChoice::Reconnect,
        r if r == NSAlertSecondButtonReturn => FailureChoice::Edit,
        _ => FailureChoice::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certs::fingerprint;
    use crate::session::engine::FailureKind;

    #[test]
    fn first_use_certificate_text() {
        let fp = fingerprint(b"abc");
        let t = certificate_text("Media Server", &fp, None);
        assert_eq!(t.title, "First connection to Media Server");
        assert!(t.body.contains("SHA-256: BA:78:16:BF"));
        assert_eq!(t.buttons, ["Trust and Connect", "Cancel"]);
        assert_eq!(t.accept_index, 0);
    }

    #[test]
    fn changed_certificate_text_defaults_to_cancel() {
        let (new, old) = (fingerprint(b"new"), fingerprint(b"old"));
        let t = certificate_text("Pi5", &new, Some(&old));
        assert_eq!(t.title, "WARNING: Pi5's identity changed");
        assert!(t.body.contains("intercepting"));
        assert!(t.body.contains(&format!("New SHA-256: {}", crate::certs::display_fingerprint(&new))));
        assert!(t.body.contains(&format!("Previous: {}", crate::certs::display_fingerprint(&old))));
        assert_eq!(t.buttons, ["Cancel", "Trust New Certificate"]);
        assert_eq!(t.accept_index, 1);
    }

    #[test]
    fn failure_titles_by_kind() {
        assert_eq!(failure_title(FailureKind::BadCredentials, "Pi5"), "Couldn't sign in to Pi5");
        assert_eq!(failure_title(FailureKind::Unreachable, "Pi5"), "Can't reach Pi5");
        assert_eq!(failure_title(FailureKind::UnsupportedSecurity, "Pi5"), "Pi5 requires legacy RDP security");
        assert_eq!(failure_title(FailureKind::Other, "Pi5"), "Connection to Pi5 failed");
    }

    #[test]
    fn legacy_security_hint_explains_the_xrdp_fix() {
        let hint = failure_hint(FailureKind::UnsupportedSecurity);
        assert!(hint.contains("/etc/xrdp/xrdp.ini"));
        assert!(hint.contains("security_layer=tls"));
        assert!(hint.contains("certificate and private key"));
    }
}
