//! Trust-on-first-use for RDP server certificates: remember each server's
//! certificate fingerprint and warn when it changes.

use sha2::{Digest, Sha256};

const PREFIX: &str = "sha256:";

/// `"sha256:"` + lowercase hex SHA-256 of the certificate's DER bytes.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{PREFIX}{hex}")
}

/// `"BA:78:16:…"` — how the fingerprint is shown to the user.
pub fn display_fingerprint(fp: &str) -> String {
    let hex = fp.strip_prefix(PREFIX).unwrap_or(fp).to_ascii_uppercase();
    hex.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    Trusted,
    FirstUse,
    Changed { previous: String },
}

pub fn check(stored: Option<&str>, presented: &str) -> Trust {
    match stored {
        None => Trust::FirstUse,
        Some(s) if s == presented => Trust::Trusted,
        Some(s) => Trust::Changed { previous: s.to_owned() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_hex() {
        assert_eq!(
            fingerprint(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn display_fingerprint_groups_pairs() {
        let shown = display_fingerprint(&fingerprint(b"abc"));
        assert!(shown.starts_with("BA:78:16:BF"));
        assert_eq!(shown.len(), 32 * 3 - 1);
    }

    #[test]
    fn trust_decisions() {
        assert_eq!(check(None, "sha256:aa"), Trust::FirstUse);
        assert_eq!(check(Some("sha256:aa"), "sha256:aa"), Trust::Trusted);
        assert_eq!(check(Some("sha256:aa"), "sha256:bb"), Trust::Changed { previous: "sha256:aa".into() });
    }
}
