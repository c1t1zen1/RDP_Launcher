//! Clipboard text encoding for RDP (CF_UNICODETEXT) and detection of local copies.

/// Windows' clipboard format id for UTF-16 text.
pub const CF_UNICODETEXT: u32 = 13;

/// Mac text → CF_UNICODETEXT bytes: CRLF line endings, UTF-16LE, NUL-terminated.
pub fn encode_unicode_text(s: &str) -> Vec<u8> {
    let normalized = s.replace("\r\n", "\n").replace('\n', "\r\n");
    normalized
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// CF_UNICODETEXT bytes → Mac text (stops at the first NUL; CRLF → LF; invalid UTF-16 replaced).
pub fn decode_unicode_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units).replace("\r\n", "\n")
}

/// Tells local copies apart from our own writes to `NSPasteboard` (by `changeCount`).
#[derive(Debug)]
pub struct PasteboardWatcher {
    last_seen: isize,
    suppressed: Option<isize>,
}

impl PasteboardWatcher {
    pub fn new(current: isize) -> Self {
        PasteboardWatcher { last_seen: current, suppressed: None }
    }

    /// True when the pasteboard changed because the user copied something.
    pub fn observe(&mut self, change_count: isize) -> bool {
        if change_count == self.last_seen {
            return false;
        }
        self.last_seen = change_count;
        if self.suppressed == Some(change_count) {
            self.suppressed = None;
            return false;
        }
        true
    }

    /// We just wrote the pasteboard (remote → Mac); don't echo it back.
    pub fn suppress_next(&mut self, change_count_after_our_write: isize) {
        self.suppressed = Some(change_count_after_our_write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_adds_crlf_and_nul() {
        assert_eq!(encode_unicode_text("a\nb"), vec![b'a', 0, b'\r', 0, b'\n', 0, b'b', 0, 0, 0]);
    }

    #[test]
    fn encode_keeps_existing_crlf() {
        assert_eq!(decode_unicode_text(&encode_unicode_text("a\r\nb")), "a\nb");
        assert_eq!(encode_unicode_text("a\r\nb"), encode_unicode_text("a\nb"));
    }

    #[test]
    fn decode_strips_nul_and_crlf() {
        assert_eq!(decode_unicode_text(&encode_unicode_text("héllo\nwörld 🎉")), "héllo\nwörld 🎉");
    }

    #[test]
    fn decode_stops_at_first_nul() {
        assert_eq!(decode_unicode_text(&[b'a', 0, 0, 0, b'b', 0]), "a");
    }

    #[test]
    fn decode_tolerates_odd_length() {
        assert_eq!(decode_unicode_text(&[b'a', 0, b'b']), "a");
    }

    #[test]
    fn watcher_reports_user_copy_once() {
        let mut w = PasteboardWatcher::new(5);
        assert!(!w.observe(5));
        assert!(w.observe(6));
        assert!(!w.observe(6));
    }

    #[test]
    fn watcher_ignores_our_own_write() {
        let mut w = PasteboardWatcher::new(5);
        w.suppress_next(6);
        assert!(!w.observe(6));
        assert!(w.observe(7));
    }
}
