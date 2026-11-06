//! macOS virtual keycodes → RDP (PC set-1) scancodes, plus the ⌘-key rules:
//! ⌘+key sends Ctrl+key, tapping ⌘ alone sends the Windows key.

use ironrdp_input::{Operation, Scancode};

pub const KC_CMD_L: u16 = 0x37;
pub const KC_CMD_R: u16 = 0x36;
pub const KC_SHIFT_L: u16 = 0x38;
pub const KC_SHIFT_R: u16 = 0x3C;
pub const KC_OPT_L: u16 = 0x3A;
pub const KC_OPT_R: u16 = 0x3D;
pub const KC_CTRL_L: u16 = 0x3B;
pub const KC_CTRL_R: u16 = 0x3E;
pub const KC_CAPS: u16 = 0x39;
pub const KC_FN: u16 = 0x3F;

const fn plain(code: u8) -> Scancode {
    Scancode::from_u8(false, code)
}

const fn ext(code: u8) -> Scancode {
    Scancode::from_u8(true, code)
}

const SC_LCTRL: Scancode = plain(0x1D);
const SC_LALT: Scancode = plain(0x38);
const SC_DELETE: Scancode = ext(0x53);
const SC_LWIN: Scancode = ext(0x5B);
const SC_CAPS: Scancode = plain(0x3A);

/// Physical-position mapping from macOS `kVK_*` codes to PC set-1 scancodes.
pub fn scancode_for_keycode(keycode: u16) -> Option<Scancode> {
    Some(match keycode {
        0x00 => plain(0x1E), // A
        0x01 => plain(0x1F), // S
        0x02 => plain(0x20), // D
        0x03 => plain(0x21), // F
        0x04 => plain(0x23), // H
        0x05 => plain(0x22), // G
        0x06 => plain(0x2C), // Z
        0x07 => plain(0x2D), // X
        0x08 => plain(0x2E), // C
        0x09 => plain(0x2F), // V
        0x0A => plain(0x56), // ISO §
        0x0B => plain(0x30), // B
        0x0C => plain(0x10), // Q
        0x0D => plain(0x11), // W
        0x0E => plain(0x12), // E
        0x0F => plain(0x13), // R
        0x10 => plain(0x15), // Y
        0x11 => plain(0x14), // T
        0x12 => plain(0x02), // 1
        0x13 => plain(0x03), // 2
        0x14 => plain(0x04), // 3
        0x15 => plain(0x05), // 4
        0x16 => plain(0x07), // 6
        0x17 => plain(0x06), // 5
        0x18 => plain(0x0D), // =
        0x19 => plain(0x0A), // 9
        0x1A => plain(0x08), // 7
        0x1B => plain(0x0C), // -
        0x1C => plain(0x09), // 8
        0x1D => plain(0x0B), // 0
        0x1E => plain(0x1B), // ]
        0x1F => plain(0x18), // O
        0x20 => plain(0x16), // U
        0x21 => plain(0x1A), // [
        0x22 => plain(0x17), // I
        0x23 => plain(0x19), // P
        0x24 => plain(0x1C), // Return
        0x25 => plain(0x26), // L
        0x26 => plain(0x24), // J
        0x27 => plain(0x28), // '
        0x28 => plain(0x25), // K
        0x29 => plain(0x27), // ;
        0x2A => plain(0x2B), // \
        0x2B => plain(0x33), // ,
        0x2C => plain(0x35), // /
        0x2D => plain(0x31), // N
        0x2E => plain(0x32), // M
        0x2F => plain(0x34), // .
        0x30 => plain(0x0F), // Tab
        0x31 => plain(0x39), // Space
        0x32 => plain(0x29), // `
        0x33 => plain(0x0E), // Backspace
        0x35 => plain(0x01), // Esc
        0x38 => plain(0x2A), // Left Shift
        0x39 => SC_CAPS,     // Caps Lock
        0x3A => plain(0x38), // Left Option → Alt
        0x3B => plain(0x1D), // Left Control
        0x3C => plain(0x36), // Right Shift
        0x3D => ext(0x38),   // Right Option → AltGr
        0x3E => ext(0x1D),   // Right Control
        0x40 => plain(0x68), // F17
        0x41 => plain(0x53), // Keypad .
        0x43 => plain(0x37), // Keypad *
        0x45 => plain(0x4E), // Keypad +
        0x47 => plain(0x45), // Keypad Clear → Num Lock
        0x4B => ext(0x35),   // Keypad /
        0x4C => ext(0x1C),   // Keypad Enter
        0x4E => plain(0x4A), // Keypad -
        0x4F => plain(0x69), // F18
        0x50 => plain(0x6A), // F19
        0x51 => plain(0x59), // Keypad =
        0x52 => plain(0x52), // Keypad 0
        0x53 => plain(0x4F), // Keypad 1
        0x54 => plain(0x50), // Keypad 2
        0x55 => plain(0x51), // Keypad 3
        0x56 => plain(0x4B), // Keypad 4
        0x57 => plain(0x4C), // Keypad 5
        0x58 => plain(0x4D), // Keypad 6
        0x59 => plain(0x47), // Keypad 7
        0x5A => plain(0x6B), // F20
        0x5B => plain(0x48), // Keypad 8
        0x5C => plain(0x49), // Keypad 9
        0x5D => plain(0x7D), // JIS ¥
        0x5E => plain(0x73), // JIS _
        0x5F => plain(0x7E), // JIS keypad ,
        0x60 => plain(0x3F), // F5
        0x61 => plain(0x40), // F6
        0x62 => plain(0x41), // F7
        0x63 => plain(0x3D), // F3
        0x64 => plain(0x42), // F8
        0x65 => plain(0x43), // F9
        0x66 => plain(0x7B), // JIS Eisu
        0x67 => plain(0x57), // F11
        0x68 => plain(0x79), // JIS Kana
        0x69 => plain(0x64), // F13
        0x6A => plain(0x67), // F16
        0x6B => plain(0x65), // F14
        0x6D => plain(0x44), // F10
        0x6E => ext(0x5D),   // Context menu
        0x6F => plain(0x58), // F12
        0x71 => plain(0x66), // F15
        0x72 => ext(0x52),   // Help → Insert
        0x73 => ext(0x47),   // Home
        0x74 => ext(0x49),   // Page Up
        0x75 => SC_DELETE,   // Forward Delete
        0x76 => plain(0x3E), // F4
        0x77 => ext(0x4F),   // End
        0x78 => plain(0x3C), // F2
        0x79 => ext(0x51),   // Page Down
        0x7A => plain(0x3B), // F1
        0x7B => ext(0x4B),   // Left
        0x7C => ext(0x4D),   // Right
        0x7D => ext(0x50),   // Down
        0x7E => ext(0x48),   // Up
        _ => return None,
    })
}

fn is_cmd(keycode: u16) -> bool {
    keycode == KC_CMD_L || keycode == KC_CMD_R
}

/// Turns AppKit key events into RDP key operations, applying the ⌘ rules.
#[derive(Debug, Default)]
pub struct KeyTranslator {
    cmd_down: bool,
    cmd_used: bool,
    ctrl_injected: bool,
}

impl KeyTranslator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn key_down(&mut self, keycode: u16) -> Vec<Operation> {
        let Some(sc) = scancode_for_keycode(keycode) else {
            return Vec::new();
        };
        let mut ops = Vec::with_capacity(2);
        if self.cmd_down {
            self.cmd_used = true;
            if !self.ctrl_injected {
                self.ctrl_injected = true;
                ops.push(Operation::KeyPressed(SC_LCTRL));
            }
        }
        ops.push(Operation::KeyPressed(sc));
        ops
    }

    pub fn key_up(&mut self, keycode: u16) -> Vec<Operation> {
        scancode_for_keycode(keycode)
            .map(|sc| vec![Operation::KeyReleased(sc)])
            .unwrap_or_default()
    }

    /// A modifier key (from `flagsChanged:`) went down or up.
    pub fn modifier_changed(&mut self, keycode: u16, pressed: bool) -> Vec<Operation> {
        if is_cmd(keycode) {
            return if pressed {
                self.cmd_down = true;
                self.cmd_used = false;
                Vec::new()
            } else if !self.cmd_down {
                Vec::new()
            } else {
                let ops = if self.ctrl_injected {
                    vec![Operation::KeyReleased(SC_LCTRL)]
                } else if !self.cmd_used {
                    windows_key()
                } else {
                    Vec::new()
                };
                self.reset();
                ops
            };
        }
        if keycode == KC_CAPS {
            // macOS reports the lock state, not the key; Windows wants a key press.
            return vec![Operation::KeyPressed(SC_CAPS), Operation::KeyReleased(SC_CAPS)];
        }
        match scancode_for_keycode(keycode) {
            Some(sc) if pressed => vec![Operation::KeyPressed(sc)],
            Some(sc) => vec![Operation::KeyReleased(sc)],
            None => Vec::new(),
        }
    }

    /// Forget ⌘ state (the window lost focus; the remote side is released separately).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub fn ctrl_alt_del() -> Vec<Operation> {
    vec![
        Operation::KeyPressed(SC_LCTRL),
        Operation::KeyPressed(SC_LALT),
        Operation::KeyPressed(SC_DELETE),
        Operation::KeyReleased(SC_DELETE),
        Operation::KeyReleased(SC_LALT),
        Operation::KeyReleased(SC_LCTRL),
    ]
}

pub fn windows_key() -> Vec<Operation> {
    vec![Operation::KeyPressed(SC_LWIN), Operation::KeyReleased(SC_LWIN)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_input::Operation;

    /// `Operation` has no `PartialEq`; compare as (pressed, scancode-u16) pairs.
    fn k(ops: Vec<Operation>) -> Vec<(bool, u16)> {
        ops.into_iter()
            .map(|op| match op {
                Operation::KeyPressed(s) => (true, s.as_u16()),
                Operation::KeyReleased(s) => (false, s.as_u16()),
                other => panic!("unexpected operation {other:?}"),
            })
            .collect()
    }

    const LCTRL: u16 = 0x1D;
    const LWIN: u16 = 0xE05B;

    #[test]
    fn letter_a_maps_to_1e() {
        assert_eq!(scancode_for_keycode(0x00).map(|s| s.as_u16()), Some(0x1E));
    }

    #[test]
    fn arrows_are_extended() {
        assert_eq!(scancode_for_keycode(0x7E).map(|s| s.as_u16()), Some(0xE048));
    }

    #[test]
    fn function_key_is_ignored() {
        assert!(scancode_for_keycode(KC_FN).is_none());
        let mut t = KeyTranslator::new();
        assert!(t.key_down(KC_FN).is_empty());
        assert!(t.modifier_changed(KC_FN, true).is_empty());
    }

    #[test]
    fn cmd_c_sends_ctrl_c() {
        let mut t = KeyTranslator::new();
        assert!(t.modifier_changed(KC_CMD_L, true).is_empty());
        assert_eq!(k(t.key_down(0x08)), vec![(true, LCTRL), (true, 0x2E)]);
        assert_eq!(k(t.key_up(0x08)), vec![(false, 0x2E)]);
        assert_eq!(k(t.modifier_changed(KC_CMD_L, false)), vec![(false, LCTRL)]);
    }

    #[test]
    fn cmd_with_two_keys_presses_ctrl_once() {
        let mut t = KeyTranslator::new();
        t.modifier_changed(KC_CMD_L, true);
        let mut all = k(t.key_down(0x08));
        all.extend(k(t.key_up(0x08)));
        all.extend(k(t.key_down(0x09)));
        assert_eq!(all.iter().filter(|e| **e == (true, LCTRL)).count(), 1);
        assert_eq!(all.last(), Some(&(true, 0x2F)));
    }

    #[test]
    fn tapping_cmd_alone_sends_windows_key() {
        let mut t = KeyTranslator::new();
        assert!(t.modifier_changed(KC_CMD_R, true).is_empty());
        assert_eq!(k(t.modifier_changed(KC_CMD_R, false)), vec![(true, LWIN), (false, LWIN)]);
    }

    #[test]
    fn option_maps_to_alt() {
        let mut t = KeyTranslator::new();
        assert_eq!(k(t.modifier_changed(KC_OPT_L, true)), vec![(true, 0x38)]);
        assert_eq!(k(t.modifier_changed(KC_OPT_L, false)), vec![(false, 0x38)]);
    }

    #[test]
    fn right_option_is_extended_alt() {
        let mut t = KeyTranslator::new();
        assert_eq!(k(t.modifier_changed(KC_OPT_R, true)), vec![(true, 0xE038)]);
    }

    #[test]
    fn ctrl_and_shift_pass_through() {
        let mut t = KeyTranslator::new();
        assert_eq!(k(t.modifier_changed(KC_CTRL_L, true)), vec![(true, 0x1D)]);
        assert_eq!(k(t.modifier_changed(KC_SHIFT_R, true)), vec![(true, 0x36)]);
        assert_eq!(k(t.modifier_changed(KC_CTRL_R, false)), vec![(false, 0xE01D)]);
    }

    #[test]
    fn caps_lock_change_sends_press_and_release() {
        let mut t = KeyTranslator::new();
        assert_eq!(k(t.modifier_changed(KC_CAPS, true)), vec![(true, 0x3A), (false, 0x3A)]);
        assert_eq!(k(t.modifier_changed(KC_CAPS, false)), vec![(true, 0x3A), (false, 0x3A)]);
    }

    #[test]
    fn reset_after_focus_loss_forgets_cmd() {
        let mut t = KeyTranslator::new();
        t.modifier_changed(KC_CMD_L, true);
        t.reset();
        assert!(t.modifier_changed(KC_CMD_L, false).is_empty()); // no stray Windows key
    }

    #[test]
    fn ctrl_alt_del_sequence() {
        assert_eq!(
            k(ctrl_alt_del()),
            vec![(true, 0x1D), (true, 0x38), (true, 0xE053), (false, 0xE053), (false, 0x38), (false, 0x1D)]
        );
    }

    #[test]
    fn windows_key_sequence() {
        assert_eq!(k(windows_key()), vec![(true, LWIN), (false, LWIN)]);
    }
}
