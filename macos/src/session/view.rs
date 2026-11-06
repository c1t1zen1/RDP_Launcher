//! `RdpView`: an `NSView` that shows (a region of) the remote desktop and turns
//! AppKit keyboard / mouse events into RDP input.

use std::cell::{Cell, RefCell};

use ironrdp_input::{MouseButton, MousePosition, Operation, WheelRotations};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSCursor, NSEvent, NSEventModifierFlags, NSImage, NSResponder, NSTrackingArea, NSTrackingAreaOptions, NSView,
};
use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use objc2_quartz_core::kCAGravityResize;

use crate::session::engine::{PointerUpdate, SessionHandle};
use crate::session::geometry::{contents_rect_top_left, view_to_remote, ScrollAccumulator, ViewMapping};
use crate::session::keymap::{self, KeyTranslator, KC_CAPS};

pub struct RdpViewIvars {
    handle: SessionHandle,
    translator: RefCell<KeyTranslator>,
    mapping: Cell<ViewMapping>,
    vscroll: RefCell<ScrollAccumulator>,
    hscroll: RefCell<ScrollAccumulator>,
    cursor: RefCell<Retained<NSCursor>>,
    tracking: RefCell<Option<Retained<NSTrackingArea>>>,
    can_toggle_full_screen: Cell<bool>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; RdpView does not implement Drop.
    #[unsafe(super(NSView, NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[name = "RdpLauncherRdpView"]
    #[ivars = RdpViewIvars]
    pub struct RdpView;

    impl RdpView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            let mut m = self.ivars().mapping.get();
            m.view_w = size.width;
            m.view_h = size.height;
            self.ivars().mapping.set(m);
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.release_input();
            unsafe { msg_send![super(self), resignFirstResponder] }
        }

        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(old) = self.ivars().tracking.borrow_mut().take() {
                self.removeTrackingArea(&old);
            }
            // ActiveAlways: in multi-monitor sessions the pointer moves between windows.
            let options = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect;
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    self.bounds(),
                    options,
                    Some(self),
                    None,
                )
            };
            self.addTrackingArea(&area);
            *self.ivars().tracking.borrow_mut() = Some(area);
            let _: () = unsafe { msg_send![super(self), updateTrackingAreas] };
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &self.ivars().cursor.borrow());
        }

        // ----- keyboard -----

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let ops = self.ivars().translator.borrow_mut().key_down(event.keyCode());
            self.ivars().handle.send_operations(ops);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            let ops = self.ivars().translator.borrow_mut().key_up(event.keyCode());
            self.ivars().handle.send_operations(ops);
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            let code = event.keyCode();
            let pressed = code == KC_CAPS || keymap::modifier_is_down(code, event.modifierFlags().0 as u64);
            let ops = self.ivars().translator.borrow_mut().modifier_changed(code, pressed);
            self.ivars().handle.send_operations(ops);
        }

        /// ⌘-combinations arrive here instead of `keyDown:`. Everything except the app's
        /// reserved shortcuts goes to the remote (as Ctrl+key). AppKit never sends `keyUp:`
        /// while ⌘ is held, so the key is pressed and released here.
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            let flags = event.modifierFlags();
            let ours = flags.contains(NSEventModifierFlags::Command)
                && self.is_key_view()
                && !is_reserved_shortcut(event.keyCode(), flags);
            if ours {
                let code = event.keyCode();
                let mut t = self.ivars().translator.borrow_mut();
                let mut ops = t.key_down(code);
                ops.extend(t.key_up(code));
                drop(t);
                self.ivars().handle.send_operations(ops);
            }
            ours
        }

        // ----- Session menu (found through the responder chain) -----

        #[unsafe(method(toggleSessionFullScreen:))]
        fn toggle_session_full_screen(&self, _sender: Option<&AnyObject>) {
            if self.ivars().can_toggle_full_screen.get() {
                if let Some(w) = self.window() {
                    w.toggleFullScreen(None);
                }
            }
        }

        #[unsafe(method(sendCtrlAltDel:))]
        fn send_ctrl_alt_del(&self, _sender: Option<&AnyObject>) {
            self.ivars().handle.send_operations(keymap::ctrl_alt_del());
        }

        #[unsafe(method(sendWindowsKey:))]
        fn send_windows_key(&self, _sender: Option<&AnyObject>) {
            self.ivars().handle.send_operations(keymap::windows_key());
        }

        #[unsafe(method(disconnectSession:))]
        fn disconnect_session(&self, _sender: Option<&AnyObject>) {
            if let Some(w) = self.window() {
                w.performClose(None);
            }
        }

        // ----- mouse -----

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.send_move(event, Vec::new());
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.send_move(event, Vec::new());
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.send_move(event, Vec::new());
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.send_move(event, Vec::new());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.send_move(event, vec![Operation::MouseButtonPressed(MouseButton::Left)]);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.send_move(event, vec![Operation::MouseButtonReleased(MouseButton::Left)]);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.send_move(event, vec![Operation::MouseButtonPressed(MouseButton::Right)]);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.send_move(event, vec![Operation::MouseButtonReleased(MouseButton::Right)]);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            if let Some(b) = other_button(event.buttonNumber()) {
                self.send_move(event, vec![Operation::MouseButtonPressed(b)]);
            }
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            if let Some(b) = other_button(event.buttonNumber()) {
                self.send_move(event, vec![Operation::MouseButtonReleased(b)]);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let precise = event.hasPreciseScrollingDeltas();
            let mut ops = Vec::new();
            let v = self.ivars().vscroll.borrow_mut().push(event.scrollingDeltaY(), precise);
            if v != 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: true, rotation_units: v }));
            }
            let h = self.ivars().hscroll.borrow_mut().push(-event.scrollingDeltaX(), precise);
            if h != 0 {
                ops.push(Operation::WheelRotations(WheelRotations { is_vertical: false, rotation_units: h }));
            }
            self.ivars().handle.send_operations(ops);
        }
    }
);

fn other_button(number: isize) -> Option<MouseButton> {
    match number {
        2 => Some(MouseButton::Middle),
        3 => Some(MouseButton::X1),
        4 => Some(MouseButton::X2),
        _ => None,
    }
}

fn is_reserved_shortcut(keycode: u16, flags: NSEventModifierFlags) -> bool {
    keymap::is_mac_shortcut(
        keycode,
        flags.contains(NSEventModifierFlags::Control),
        flags.contains(NSEventModifierFlags::Shift),
    )
}

impl RdpView {
    pub fn new(mtm: MainThreadMarker, handle: SessionHandle, mapping: ViewMapping) -> Retained<RdpView> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(mapping.view_w, mapping.view_h));
        let this = Self::alloc(mtm).set_ivars(RdpViewIvars {
            handle,
            translator: RefCell::new(KeyTranslator::new()),
            mapping: Cell::new(mapping),
            vscroll: RefCell::new(ScrollAccumulator::new()),
            hscroll: RefCell::new(ScrollAccumulator::new()),
            cursor: RefCell::new(NSCursor::arrowCursor()),
            tracking: RefCell::new(None),
            can_toggle_full_screen: Cell::new(true),
        });
        let view: Retained<RdpView> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setWantsLayer(true);
        if let Some(layer) = view.layer() {
            layer.setContentsGravity(unsafe { kCAGravityResize });
            // Black until the first frame arrives.
            layer.setBackgroundColor(Some(&objc2_core_graphics::CGColor::new_generic_gray(0.0, 1.0)));
        }
        view
    }

    pub fn handle(&self) -> &SessionHandle {
        &self.ivars().handle
    }

    /// Multi-monitor sessions are always full screen.
    pub fn set_can_toggle_full_screen(&self, on: bool) {
        self.ivars().can_toggle_full_screen.set(on);
    }

    pub fn set_mapping(&self, mapping: ViewMapping) {
        self.ivars().mapping.set(mapping);
    }

    fn is_key_view(&self) -> bool {
        self.window().is_some_and(|w| w.isKeyWindow() && w.firstResponder().is_some_and(|r| std::ptr::eq(&*r, self.as_ref())))
    }

    /// Show `image` (the whole framebuffer); this view displays its mapping's region of it.
    pub fn present(&self, image: &CGImage, fb_w: u16, fb_h: u16) {
        let Some(layer) = self.layer() else { return };
        let (x, y, w, h) = contents_rect_top_left(&self.ivars().mapping.get(), fb_w, fb_h);
        // This view is flipped, so its layer's contentsRect already counts from the image's top
        // (verified with stacked test displays: y = 0 shows the top of the desktop).
        layer.setContentsRect(objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(x, y),
            objc2_core_foundation::CGSize::new(w, h),
        ));
        let obj: &AnyObject = unsafe { &*(image as *const CGImage).cast::<AnyObject>() };
        unsafe { layer.setContents(Some(obj)) };
    }

    pub fn set_pointer(&self, update: &PointerUpdate) {
        let cursor = match update {
            PointerUpdate::Default => NSCursor::arrowCursor(),
            PointerUpdate::Hidden => hidden_cursor(),
            PointerUpdate::Bitmap(p) => cursor_from_rgba(
                &p.bitmap_data,
                p.width,
                p.height,
                p.hotspot_x,
                p.hotspot_y,
            )
            .unwrap_or_else(NSCursor::arrowCursor),
        };
        *self.ivars().cursor.borrow_mut() = cursor;
        if let Some(window) = self.window() {
            window.invalidateCursorRectsForView(self);
        }
    }

    /// Release keys/buttons held on the remote side and forget ⌘ state (focus lost).
    pub fn release_input(&self) {
        self.ivars().translator.borrow_mut().reset();
        self.ivars().handle.release_all();
    }

    fn send_move(&self, event: &NSEvent, mut extra: Vec<Operation>) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        let (x, y) = view_to_remote(&self.ivars().mapping.get(), p.x, p.y);
        let mut ops = vec![Operation::MouseMove(MousePosition { x, y })];
        ops.append(&mut extra);
        self.ivars().handle.send_operations(ops);
    }

    pub fn as_responder(&self) -> &NSResponder {
        self.as_ref()
    }

    pub fn make_first_responder(&self) {
        if let Some(window) = self.window() {
            window.makeFirstResponder(Some(self.as_ref()));
        }
    }

}

/// `0x00RRGGBB` little-endian pixels are B,G,R,X in memory.
pub fn make_cg_image(pixels: &[u32], width: u16, height: u16) -> Option<CFRetained<CGImage>> {
    if width == 0 || height == 0 || pixels.len() < usize::from(width) * usize::from(height) {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), pixels.len() * 4) };
    let data = CFData::from_bytes(bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = CGColorSpace::new_device_rgb()?;
    let info = CGBitmapInfo(CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::NoneSkipFirst.0);
    unsafe {
        CGImage::new(
            usize::from(width),
            usize::from(height),
            8,
            32,
            usize::from(width) * 4,
            Some(&space),
            info,
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

fn rgba_image(rgba: &[u8], width: u16, height: u16) -> Option<CFRetained<CGImage>> {
    if width == 0 || height == 0 || rgba.len() < usize::from(width) * usize::from(height) * 4 {
        return None;
    }
    let data = CFData::from_bytes(rgba);
    let provider = CGDataProvider::with_cf_data(Some(&data))?;
    let space = CGColorSpace::new_device_rgb()?;
    unsafe {
        CGImage::new(
            usize::from(width),
            usize::from(height),
            8,
            32,
            usize::from(width) * 4,
            Some(&space),
            CGBitmapInfo(CGImageAlphaInfo::Last.0),
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
}

fn cursor_from_rgba(
    rgba: &[u8],
    width: u16,
    height: u16,
    hot_x: u16,
    hot_y: u16,
) -> Option<Retained<NSCursor>> {
    let image = rgba_image(rgba, width, height)?;
    let size = NSSize::new(f64::from(width), f64::from(height));
    let ns_image = NSImage::initWithCGImage_size(NSImage::alloc(), &image, size);
    Some(NSCursor::initWithImage_hotSpot(
        NSCursor::alloc(),
        &ns_image,
        NSPoint::new(f64::from(hot_x), f64::from(hot_y)),
    ))
}

fn hidden_cursor() -> Retained<NSCursor> {
    cursor_from_rgba(&[0, 0, 0, 0], 1, 1, 0, 0).unwrap_or_else(NSCursor::arrowCursor)
}
