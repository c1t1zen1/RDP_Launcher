//! The Mac's connected displays, with stable keys for remembering a selection.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::{CGDisplayModelNumber, CGDisplaySerialNumber, CGDisplayVendorNumber};
use objc2_foundation::{NSNumber, NSRect, NSString};

use crate::monitors::{DisplayInfo, Rect};

fn rect(r: NSRect) -> Rect {
    Rect { x: r.origin.x, y: r.origin.y, w: r.size.width, h: r.size.height }
}

/// Connected displays; the first (menu-bar) screen is marked as main.
pub fn current_displays(mtm: MainThreadMarker) -> Vec<DisplayInfo> {
    NSScreen::screens(mtm)
        .iter()
        .enumerate()
        .map(|(i, screen)| {
            let name = screen.localizedName().to_string();
            let id = screen
                .deviceDescription()
                .objectForKey(&NSString::from_str("NSScreenNumber"))
                .and_then(|n| n.downcast::<NSNumber>().ok())
                .map(|n| n.unsignedIntValue())
                .unwrap_or(0);
            let key = display_key(CGDisplayVendorNumber(id), CGDisplayModelNumber(id), CGDisplaySerialNumber(id), &name);
            DisplayInfo { key, name, frame: rect(screen.frame()), visible_frame: rect(screen.visibleFrame()), is_main: i == 0 }
        })
        .collect()
}

/// Stable identity for a display: hardware vendor/model/serial, or its name when
/// the display reports no IDs (e.g. AirPlay / Sidecar).
pub fn display_key(vendor: u32, model: u32, serial: u32, name: &str) -> String {
    if vendor == 0 && model == 0 && serial == 0 {
        format!("name:{name}")
    } else {
        format!("{vendor:x}-{model:x}-{serial:x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_from_hardware_ids() {
        assert_eq!(display_key(0x10ac, 0xa0c4, 0x1234, "DELL U2412M"), "10ac-a0c4-1234");
    }

    #[test]
    fn key_falls_back_to_name_without_ids() {
        assert_eq!(display_key(0, 0, 0, "Sidecar Display"), "name:Sidecar Display");
    }

    #[test]
    fn identical_monitors_differ_by_serial() {
        assert_ne!(display_key(0x10ac, 0xa0c4, 1, "DELL"), display_key(0x10ac, 0xa0c4, 2, "DELL"));
    }
}
