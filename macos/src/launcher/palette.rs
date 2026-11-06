//! Colors copied from the Windows launcher (0xRRGGBB).

use objc2::rc::Retained;
use objc2_app_kit::NSColor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonColors {
    pub top: u32,
    pub bottom: u32,
    pub border: u32,
    pub text: u32,
    pub sub: u32,
    pub grip: u32,
}

pub const WINDOW_BG: u32 = 0xEFF2F7;
pub const HEADER_BG: u32 = 0xE7ECF4;
pub const HEADER_LINE: u32 = 0xC5CEDC;
pub const HEADER_TEXT: u32 = 0x445066;
pub const ICON_DARK: u32 = 0x2B3A55;
pub const PIN_ON: u32 = 0x0B63CE;
pub const PIN_OFF: u32 = 0x5A667A;
pub const FLAT_HOT: u32 = 0xDDE5F1;
pub const FLAT_PRESSED: u32 = 0xBFCBDE;

pub fn button_colors(connected: bool, pressed: bool, hot: bool) -> ButtonColors {
    if connected {
        let (top, bottom, border) =
            if pressed { (0xB8E6B8, 0x7CC77C, 0x3E8E3E) } else { (0xDFF6DF, 0x9FDD9F, 0x4C9A4C) };
        return ButtonColors { top, bottom, border, text: 0x0F3D0F, sub: 0x2E6B2E, grip: 0x4C9A4C };
    }
    let (top, bottom, border) = if pressed {
        (0xC9D3E4, 0xAEBBD2, 0x5A6C8C)
    } else if hot {
        (0xFFFFFF, 0xE3E9F3, 0x6C7E9B)
    } else {
        (0xF4F7FB, 0xD5DDEA, 0x7A8AA5)
    };
    ButtonColors { top, bottom, border, text: 0x101A2B, sub: 0x5A667A, grip: 0x8A97AC }
}

pub fn color(rgb: u32) -> Retained<NSColor> {
    let c = |shift: u32| f64::from((rgb >> shift) & 0xFF) / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_wins_over_hover() {
        assert_eq!(button_colors(true, false, true).top, 0xDFF6DF);
    }

    #[test]
    fn pressed_wins_over_hover() {
        assert_eq!(button_colors(false, true, true).top, 0xC9D3E4);
    }

    #[test]
    fn idle_colors_match_windows() {
        let c = button_colors(false, false, false);
        assert_eq!((c.top, c.bottom, c.border, c.text), (0xF4F7FB, 0xD5DDEA, 0x7A8AA5, 0x101A2B));
    }
}
