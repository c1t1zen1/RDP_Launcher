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

/// Debug aid: `RDPL_DEV_RESIZE=WxH` resizes a windowed session's window after it connects.
pub fn resize_if_requested(windows: &crate::session::window::SessionWindows) {
    let Ok(spec) = std::env::var("RDPL_DEV_RESIZE") else { return };
    let Some((w, h)) = spec.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) else { return };
    if let Some(window) = windows.windows().first() {
        window.setContentSize(objc2_foundation::NSSize::new(w, h));
    }
}

/// Debug aid: `RDPL_DEV_FAKE_DISPLAYS=side|stack` replaces the real displays with two
/// small 400×300 "displays" on the main screen, to exercise multi-monitor sessions
/// without covering the real monitors.
pub fn fake_displays() -> Option<Vec<crate::monitors::DisplayInfo>> {
    use crate::monitors::{DisplayInfo, Rect};
    let mode = std::env::var("RDPL_DEV_FAKE_DISPLAYS").ok()?;
    let second = if mode == "stack" { Rect { x: 200.0, y: 500.0, w: 400.0, h: 300.0 } } else { Rect { x: 600.0, y: 200.0, w: 400.0, h: 300.0 } };
    let d = |key: &str, frame: Rect, is_main| DisplayInfo { key: key.into(), name: key.into(), frame, visible_frame: frame, is_main };
    Some(vec![d("fake-a", Rect { x: 200.0, y: 200.0, w: 400.0, h: 300.0 }, true), d("fake-b", second, false)])
}

/// Debug aid: with `RDPL_SNAPSHOT`, render a view's layer (contents + contentsRect) to `<prefix>-<name>.png`.
pub fn snapshot_layer(view: &objc2_app_kit::NSView, name: &str) {
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
    use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGImageAlphaInfo};
    use objc2_foundation::{NSDictionary, NSString};
    let Ok(prefix) = std::env::var("RDPL_SNAPSHOT") else { return };
    let Some(layer) = view.layer() else { return };
    let size = view.bounds().size;
    let (w, h) = (size.width as usize, size.height as usize);
    let Some(space) = CGColorSpace::new_device_rgb() else { return };
    let ctx = unsafe {
        CGBitmapContextCreate(std::ptr::null_mut(), w, h, 8, w * 4, Some(&space), CGImageAlphaInfo::PremultipliedLast.0)
    };
    let Some(ctx) = ctx else { return };
    if layer.isGeometryFlipped() {
        // Draw a flipped (top-left origin) layer upright into the bottom-left bitmap context.
        objc2_core_graphics::CGContext::translate_ctm(Some(&ctx), 0.0, h as f64);
        objc2_core_graphics::CGContext::scale_ctm(Some(&ctx), 1.0, -1.0);
    }
    layer.renderInContext(&ctx);
    let Some(image) = CGBitmapContextCreateImage(Some(&ctx)) else { return };
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &image);
    if let Some(data) = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) } {
        data.writeToFile_atomically(&NSString::from_str(&format!("{prefix}-{name}.png")), true);
    }
}

/// Debug aid: `RDPL_DEV_CLOSE=1` closes the session's window (as the user would) once connected.
pub fn close_if_requested(windows: &crate::session::window::SessionWindows) {
    if std::env::var("RDPL_DEV_CLOSE").is_err() {
        return;
    }
    if let Some(window) = windows.windows().first() {
        window.performClose(None);
    }
}
