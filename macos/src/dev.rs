//! Debug-build-only aids for checking the UI without Screen Recording permission.

use crate::app::on_main;

/// Debug aid: `RDPL_SNAPSHOT=/path/prefix` writes `<prefix>-<name>.png` renders of views
/// (no Screen Recording permission needed — it draws the app's own views offscreen).
pub fn snapshot_if_requested(view: &objc2_app_kit::NSView, name: &str) {
    use objc2_app_kit::NSBitmapImageFileType;
    use objc2_foundation::{NSDictionary, NSString};
    let Ok(prefix) = std::env::var("RDPL_SNAPSHOT") else { return };
    // Windows paint their own background; give the snapshot one so light-on-dark text shows.
    if view.layer().is_none_or(|l| l.backgroundColor().is_none()) {
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            let bg = objc2_app_kit::NSColor::windowBackgroundColor();
            layer.setBackgroundColor(Some(&bg.CGColor()));
        }
    }
    let rect = view.bounds();
    let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(rect) else { return };
    view.cacheDisplayInRect_toBitmapImageRep(rect, &rep);
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
    if let Some(data) = data {
        let path = format!("{prefix}-{name}.png");
        data.writeToFile_atomically(&NSString::from_str(&path), true);
        tracing::info!(%path, "snapshot written");
    }
}

/// Debug aid: `RDPL_DEV_OPEN=add` or `edit` opens that dialog right after launch.
pub fn open_if_requested() {
    let Ok(what) = std::env::var("RDPL_DEV_OPEN") else { return };
    on_main(move |mtm| match what.as_str() {
        "add" => crate::controller::add(mtm),
        "edit" => crate::controller::edit(mtm, 0),
        "about" => crate::launcher::about::show(mtm),
        "connect" => {
            let conn = crate::controller::with_state(|s| s.conns.first().cloned()).flatten();
            if let (Some(conn), Some(m)) = (conn, crate::session::manager::manager()) {
                m.connect(&conn);
            }
        }
        _ => {}
    });
}

/// Debug aid: `RDPL_DEV_AUTOTRUST=1` accepts certificate prompts without asking.
pub fn auto_trust() -> bool {
    std::env::var("RDPL_DEV_AUTOTRUST").is_ok()
}

/// Debug aid: with `RDPL_SNAPSHOT` set, write the session image as `<prefix>-frame.png`.
pub fn snapshot_frame(image: &objc2_core_graphics::CGImage) {
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    use objc2_foundation::{NSDictionary, NSString};
    let Ok(prefix) = std::env::var("RDPL_SNAPSHOT") else { return };
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), image);
    if let Some(data) = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) } {
        data.writeToFile_atomically(&NSString::from_str(&format!("{prefix}-frame.png")), true);
    }
}
